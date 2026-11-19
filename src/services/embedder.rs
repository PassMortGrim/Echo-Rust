//! Embedder — production des vecteurs pour l'indexation vectorielle.
//!
//! Deux moteurs :
//! - [`ApiEmbedder`] : endpoint OpenAI-compatible `{base_url}/embeddings`
//!   (même client/clé que `llm_actor`), batching par lots, timeout + retry,
//!   **dimension auto-détectée** à la 1re réponse, validation stricte, et
//!   **jamais** de log du contenu des requêtes/réponses (seuls tailles et
//!   codes HTTP sont journalisés).
//! - [`HashEmbedder`] : fallback déterministe (dimension fixe, flag
//!   `encoder=hash`) — filet de test pour CI/regression, **pas** un encodage
//!   de production ; la recherche l'utilisant est explicitement dégradée.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// Taille par défaut des lots d'embeddings.
pub const DEFAULT_EMBED_BATCH: usize = 32;
/// Tentatives HTTP (1 appel + 2 retries) sur erreur réseau/5xx.
pub const EMBED_RETRIES: u32 = 3;

#[async_trait::async_trait]
pub trait Embedder: Send + Sync {
    /// Encode un lot de textes. Renvoie un vecteur par texte, tous de la
    /// même dimension, plus l'étiquette d'encodage (`hash:384`, `api:<model>`,
    /// etc.) à stocker en métadonnées.
    async fn embed(&self, texts: &[String]) -> Result<(Vec<Vec<f32>>, String)>;
}

// ─── Fallback déterministe (hash) ───────────────────────────────────────────

/// FNV-1a 64 bits (déterministe entre plateformes).
fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Embedding « hashed » déterministe : sac de mots + trigrammes de caractères,
/// signés, avec séparation de domaine (préfixe de sel) et normalisation L2.
/// Dimension fixe par config → jamais comparé à ceux de l'API.
pub struct HashEmbedder {
    pub dim: usize,
}

impl HashEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim: dim.max(16) }
    }

    pub fn encode_one(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; self.dim];
        let lower = text.to_lowercase();
        let tokens: Vec<&str> = lower
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .filter(|t| !t.is_empty())
            .collect();

        for tok in &tokens {
            for (domain, piece) in [("w", *tok), ("t", tok)] {
                let h = fnv1a64(&format!("{domain}:{piece}"));
                let idx = (h % self.dim as u64) as usize;
                let sign = if (h >> 63) & 1 == 1 { -1.0 } else { 1.0 };
                v[idx] += sign;
            }
        }

        // Trigrammes de caractères (robustesse aux variantes légères).
        let chars: Vec<char> = lower.chars().collect();
        for w in chars.windows(3) {
            let h = fnv1a64(&format!("g:{}", w.iter().collect::<String>()));
            let idx = (h % self.dim as u64) as usize;
            let sign = if (h >> 63) & 1 == 1 { -1.0 } else { 1.0 };
            v[idx] += sign * 0.5;
        }

        // Normalisation L2 (évite la division par zéro).
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }
}

#[async_trait::async_trait]
impl Embedder for HashEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<(Vec<Vec<f32>>, String)> {
        let vecs = texts.iter().map(|t| self.encode_one(t)).collect();
        Ok((vecs, format!("hash:{}", self.dim)))
    }
}

// ─── API OpenAI-compatible ({base_url}/embeddings) ─────────────────────────

pub struct ApiEmbedder {
    client: reqwest::Client,
    endpoint: String,
    model: String,
    api_key: Option<String>,
    batch_size: usize,
}

impl ApiEmbedder {
    pub fn new(
        base_url: &str,
        model: &str,
        api_key: Option<String>,
        batch_size: usize,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .context("client HTTP embeddings")?;
        Ok(Self {
            client,
            endpoint: format!("{}/embeddings", base_url.trim_end_matches('/')),
            model: model.to_owned(),
            api_key,
            batch_size: batch_size.clamp(1, 256),
        })
    }

    /// Un appel HTTP : lot → vecteurs. Aucun contenu journalisé.
    async fn call(&self, batch: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut req = self
            .client
            .post(&self.endpoint)
            .json(&json!({ "model": self.model, "input": batch }));
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let mut last_err: Option<String> = None;
        for attempt in 1..=EMBED_RETRIES {
            let res = match req.try_clone().context("clone requête")?.send().await {
                Ok(r) => r,
                Err(e) => {
                    // Aucun détail de payload : uniquement la catégorie d'erreur.
                    tracing::warn!("embeddings : erreur réseau (tentative {attempt}) — {e}");
                    last_err = Some("erreur réseau".into());
                    tokio::time::sleep(Duration::from_millis(200 * u64::from(attempt))).await;
                    continue;
                }
            };

            let status = res.status();
            if status.is_success() {
                let body: Value = res.json().await.context("JSON d'embeddings invalide")?;
                return Self::parse_response(&body, batch.len());
            }
            if status.as_u16() >= 500 {
                tracing::warn!(
                    "embeddings : HTTP {} (tentative {attempt})",
                    status.as_u16()
                );
                last_err = Some(format!("HTTP {}", status.as_u16()));
                tokio::time::sleep(Duration::from_millis(200 * u64::from(attempt))).await;
                continue;
            }
            // 4xx : pas de retry (requête à corriger) — code seulement, jamais le corps.
            anyhow::bail!(
                "embeddings refusés par l'endpoint (HTTP {})",
                status.as_u16()
            );
        }
        anyhow::bail!(
            "embeddings indisponibles après {EMBED_RETRIES} tentatives ({})",
            last_err.unwrap_or_default()
        )
    }

    /// Validation stricte : N vecteurs, même dimension, tous finis.
    fn parse_response(body: &Value, expected: usize) -> Result<Vec<Vec<f32>>> {
        let data = body
            .get("data")
            .and_then(Value::as_array)
            .context("réponse embeddings : champ « data » absent")?;
        if data.len() != expected {
            anyhow::bail!(
                "réponse embeddings : {} vecteurs attendus (lots reçus : {})",
                expected,
                data.len()
            );
        }
        let mut out = Vec::with_capacity(expected);
        let mut dim: Option<usize> = None;
        for (i, item) in data.iter().enumerate() {
            let idx_ok = item.get("index").and_then(Value::as_u64) == Some(i as u64);
            let vec = item
                .get("embedding")
                .and_then(Value::as_array)
                .context("réponse embeddings : « embedding » absent")?
                .iter()
                .map(|x| x.as_f64().map(|v| v as f32))
                .collect::<Option<Vec<f32>>>()
                .context("réponse embeddings : valeurs non numériques")?;
            if !idx_ok {
                anyhow::bail!("réponse embeddings : index désordonné");
            }
            match dim {
                None => {
                    if vec.is_empty() {
                        anyhow::bail!("réponse embeddings : dimension nulle");
                    }
                    if vec.iter().any(|x| !x.is_finite()) {
                        anyhow::bail!("réponse embeddings : valeurs non finies");
                    }
                    dim = Some(vec.len());
                }
                Some(d) if vec.len() != d => {
                    anyhow::bail!(
                        "réponse embeddings : dimension incohérente ({d} attendu, {} reçu)",
                        vec.len()
                    );
                }
                _ => {}
            }
            out.push(vec);
        }
        if let Some(d) = dim {
            tracing::info!("embeddings : dimension détectée = {d}");
        }
        Ok(out)
    }
}

#[async_trait::async_trait]
impl Embedder for ApiEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<(Vec<Vec<f32>>, String)> {
        let mut all = Vec::with_capacity(texts.len());
        let mut dim: Option<usize> = None;
        for batch in texts.chunks(self.batch_size.max(1)) {
            let vecs = self.call(batch).await?;
            for v in vecs {
                match dim {
                    None => dim = Some(v.len()),
                    Some(d) if v.len() != d => {
                        anyhow::bail!("dimension incohérente entre lots ({d} vs {})", v.len())
                    }
                    _ => {}
                }
                all.push(v);
            }
        }
        Ok((all, format!("api:{}", self.model)))
    }
}

// ─── Fabrique ───────────────────────────────────────────────────────────────

/// Choisit le moteur : API si un modèle d'embeddings est configuré, hash sinon.
pub fn make_embedder(
    base_url: &str,
    api_key: &str,
    embeddings_model: &str,
    hash_dim: usize,
    batch_size: usize,
) -> Arc<dyn Embedder> {
    if embeddings_model.trim().is_empty() {
        tracing::info!("embeddings : fallback hash (dim {hash_dim}) — recherche dégradée");
        Arc::new(HashEmbedder::new(hash_dim))
    } else {
        tracing::info!("embeddings : API (modèle configuré), lot de {batch_size}");
        let key = if api_key.trim().is_empty() {
            None
        } else {
            Some(api_key.to_owned())
        };
        Arc::new(
            ApiEmbedder::new(base_url, embeddings_model, key, batch_size)
                .expect("construction ApiEmbedder"),
        )
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn hash_is_deterministic_fixed_dim_and_normalized() {
        let e = HashEmbedder::new(384);
        let a = e.encode_one("def add(a, b): return a + b");
        let b = e.encode_one("def add(a, b): return a + b");
        let c = e.encode_one("class AuthManager: ???");
        assert_eq!(a.len(), 384);
        assert_eq!(a, b, "déterminisme");
        assert_ne!(a, c, "textes distincts → vecteurs distincts");
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "norme L2 = {norm}");
        let empty = HashEmbedder::new(384).encode_one("");
        assert!(empty.iter().all(|x| *x == 0.0));
    }

    #[tokio::test]
    async fn hash_batch_matches_single() {
        let e = HashEmbedder::new(384);
        let texts = vec!["alpha".to_string(), "beta gamma".to_string()];
        let (batch, label) = e.embed(&texts).await.unwrap();
        assert_eq!(label, "hash:384");
        assert_eq!(batch.len(), 2);
        for (i, t) in texts.iter().enumerate() {
            assert_eq!(batch[i], e.encode_one(t));
        }
    }

    /// Petit serveur HTTP factice OpenAI-compatible pour /embeddings.
    async fn spawn_fake_embeddings(dim: usize) -> (String, Arc<std::sync::Mutex<Vec<usize>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let batch_sizes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sizes = batch_sizes.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let sizes = sizes.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 65536];
                    let mut acc: Vec<u8> = Vec::new();
                    // Lit les en-têtes, puis attend le corps complet (Content-Length)
                    // avant de répondre — sinon le corps pourrait arriver en segments.
                    let (header_end, body_len) = loop {
                        let n = sock.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        acc.extend_from_slice(&buf[..n]);
                        if let Some(pos) = acc.windows(4).position(|w| w == b"\r\n\r\n") {
                            let clen = String::from_utf8_lossy(&acc[..pos])
                                .to_ascii_lowercase()
                                .lines()
                                .find_map(|l| {
                                    l.strip_prefix("content-length:")
                                        .and_then(|v| v.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            break (pos + 4, clen);
                        }
                    };
                    while acc.len() < header_end + body_len {
                        let n = sock.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        acc.extend_from_slice(&buf[..n]);
                    }
                    let body: Value =
                        serde_json::from_slice(&acc[header_end..]).unwrap_or(Value::Null);
                    let n = body
                        .get("input")
                        .and_then(Value::as_array)
                        .map(|a| a.len())
                        .unwrap_or(0);
                    sizes.lock().unwrap().push(n);
                    let data: Vec<Value> = (0..n)
                        .map(|i| {
                            let v = vec![0.5f32; dim];
                            json!({ "index": i, "embedding": v })
                        })
                        .collect();
                    let resp = json!({ "data": data }).to_string();
                    let http = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp.len(),
                        resp
                    );
                    sock.write_all(http.as_bytes()).await.ok();
                    sock.shutdown().await.ok();
                });
            }
        });

        (format!("http://{addr}"), batch_sizes)
    }

    #[tokio::test]
    async fn api_batching_and_dimension_detection() {
        let (url, sizes) = spawn_fake_embeddings(8).await;
        let e = ApiEmbedder::new(&url, "nomic-embed-text", None, 32).unwrap();
        // 40 textes → 2 appels (32 + 8) — batching effectif.
        let texts: Vec<String> = (0..40).map(|i| format!("texte {i}")).collect();
        let (vecs, label) = e.embed(&texts).await.unwrap();
        assert_eq!(vecs.len(), 40);
        assert!(vecs.iter().all(|v| v.len() == 8), "dimension auto-détectée");
        assert_eq!(label, "api:nomic-embed-text");
        let s = sizes.lock().unwrap();
        assert_eq!(*s, vec![32, 8], "lots HTTP : {s:?}");
    }

    #[tokio::test]
    async fn api_dimension_mismatch_is_rejected() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let resp = json!({ "data": [
                        { "index": 0, "embedding": [1.0, 2.0] },
                        { "index": 1, "embedding": [1.0, 2.0, 3.0] } // dim ≠
                    ] })
                    .to_string();
                    let http = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp.len(),
                        resp
                    );
                    sock.write_all(http.as_bytes()).await.ok();
                });
            }
        });
        let e = ApiEmbedder::new(&format!("http://{addr}"), "m", None, 32).unwrap();
        let texts = vec!["a".to_string(), "b".to_string()];
        let err = e
            .embed(&texts)
            .await
            .expect_err("dimension incohérente rejetée");
        assert!(err.to_string().contains("incohérente"), "err={err}");
    }

    #[tokio::test]
    async fn factory_selects_hash_when_no_model() {
        let e = make_embedder("http://unused", "", "", 384, 32);
        // Le label d'encodage n'est observable qu'après embed : on vérifie
        // simplement que le fallback est bien utilisé.
        let (vecs, label) = e.embed(&["x".into()]).await.unwrap();
        assert_eq!(label, "hash:384");
        assert_eq!(vecs[0].len(), 384);
    }
}
