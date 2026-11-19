//! VectorStore — recherche sémantique (SQLite-vec locale ou Qdrant distant).
//!
//! Règles Phase 3 :
//! - la **dimension est auto-détectée** à la 1re réponse de l'API d'embeddings
//!   (elle varie selon le modèle) et stockée en **métadonnées** de la base
//!   (table `vector_meta`) ; la table de vecteurs n'est créée qu'au 1er
//!   insert, une fois la dimension connue ;
//! - si la dimension/encoder persistés changent (modèle différent), les
//!   vecteurs existants sont invalidés et recréés — jamais mélangés ;
//! - aucun contenu de code n'est journalisé.
//!
//! Qdrant est parlé via son API REST (HTTP/JSON) : stable et testable hors
//! ligne avec un serveur HTTP factice ; évite la dépendance gRPC dont l'API
//! haute-niveau est en refonte dans la v1.19.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

// ─── Types ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    pub id: String,
    pub content: String,
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub embedding: Vec<f32>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub chunk: CodeChunk,
    pub score: f32,
}

/// Métadonnées persistées (dimension auto-détectée + encodage effectif).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreMeta {
    pub dim: usize,
    pub encoder: String,
}

// ─── Trait commun ───────────────────────────────────────────────────────────

#[async_trait::async_trait]
pub trait VectorBackend: Send + Sync {
    /// Insère (ou remplace si l'id existe) un chunk déjà encodé.
    /// Insère/met à jour un chunk. `encoder` = étiquette de l'encodeur
    /// (ex. "hash:384", "api:nomic-embed-text") — persistée en métadonnées
    /// lors du premier insert pour invalider l'index si l'encoder change.
    async fn upsert(&self, chunk: &CodeChunk, encoder: &str) -> Result<()>;
    /// Recherche les chunks les plus proches du vecteur fourni.
    async fn search(&self, vec: &[f32], top_k: usize) -> Result<Vec<Hit>>;
    /// Nom d'encodage effectif (métadonnées), ex. "hash:384".
    async fn encoder(&self) -> Result<String>;
    /// Métadonnées persistées (dimension + encoder).
    async fn meta(&self) -> Result<StoreMeta>;
}

// ─── SQLite-vec (rusqlite + extension statique) ─────────────────────────────

pub struct SqliteVecBackend {
    conn: Mutex<rusqlite::Connection>,
}

/// Enregistre l'extension sqlite-vec (statique) une seule fois par processus :
/// chaque connexion ouverte ensuite l'obtient automatiquement.
#[allow(clippy::missing_transmute_annotations)] // doc sqlite-vec : pattern officiel
fn ensure_vec_extension() {
    static VEC_INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    VEC_INIT.get_or_init(|| {
        use rusqlite::ffi::sqlite3_auto_extension;
        unsafe {
            // pointeur d'entrée compatible sqlite3_auto_extension (doc sqlite-vec)
            let init = sqlite_vec::sqlite3_vec_init as *const ();
            let _ = sqlite3_auto_extension(Some(std::mem::transmute(init)));
        }
    });
}

impl SqliteVecBackend {
    pub fn new(path: &str) -> Result<Self> {
        ensure_vec_extension();
        let conn = if path == ":memory:" {
            rusqlite::Connection::open_in_memory().context("ouverture SQLite (mémoire)")?
        } else {
            std::fs::create_dir_all(
                std::path::Path::new(path)
                    .parent()
                    .unwrap_or(std::path::Path::new(".")),
            )
            .ok();
            rusqlite::Connection::open(path)
                .with_context(|| format!("ouverture SQLite : {path}"))?
        };
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vector_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS chunks (
                id TEXT PRIMARY KEY,
                file TEXT NOT NULL,
                start_line INTEGER NOT NULL,
                end_line INTEGER NOT NULL,
                content TEXT NOT NULL,
                kind TEXT NOT NULL DEFAULT 'symbol',
                name TEXT NOT NULL DEFAULT ''
             );",
        )
        .context("schéma SQLite")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn with_lock<T>(&self, f: impl FnOnce(&rusqlite::Connection) -> Result<T>) -> Result<T> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("lock SQLite"))?;
        f(&conn)
    }

    /// Crée la table virtuelle vec0 une fois la dimension connue.
    /// En cas de changement de dimension/encoder : invalide les vecteurs
    /// existants (jamais deux encodages mélangés dans la même base).
    fn ensure_vec_table(conn: &rusqlite::Connection, dim: usize, encoder: &str) -> Result<()> {
        let stored_dim: Option<String> = conn
            .query_row("SELECT value FROM vector_meta WHERE key = 'dim'", [], |r| {
                r.get(0)
            })
            .ok();
        let stored_encoder: Option<String> = conn
            .query_row(
                "SELECT value FROM vector_meta WHERE key = 'encoder'",
                [],
                |r| r.get(0),
            )
            .ok();
        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE name = 'chunks_vec'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(false);

        let same = stored_dim.as_deref() == Some(dim.to_string().as_str())
            && stored_encoder.as_deref() == Some(encoder)
            && table_exists;
        if same {
            return Ok(());
        }
        if table_exists && stored_dim.is_some() {
            tracing::warn!(
                "dimension/encodage changés ({}:{} → {}:{}) : index vectoriel régénéré",
                stored_dim.unwrap_or_default(),
                stored_encoder.unwrap_or_default(),
                dim,
                encoder
            );
        }
        conn.execute_batch("DROP TABLE IF EXISTS chunks_vec;")
            .context("suppression de l'ancienne table vec0")?;
        conn.execute_batch("DELETE FROM vector_meta;")
            .context("reset des métadonnées")?;
        conn.execute_batch(&format!(
            "INSERT INTO vector_meta(key, value) VALUES ('dim', '{dim}'), ('encoder', '{encoder}');
             CREATE VIRTUAL TABLE chunks_vec USING vec0(embedding float[{dim}]);"
        ))
        .context("création de la table vec0 (dimension mémorisée)")?;
        Ok(())
    }

    /// f32 → blob little-endian (format binaire attendu par sqlite-vec).
    fn vec_to_blob(vec: &[f32]) -> Vec<u8> {
        vec.iter().flat_map(|x| x.to_le_bytes()).collect()
    }
}

#[async_trait::async_trait]
impl VectorBackend for SqliteVecBackend {
    async fn upsert(&self, chunk: &CodeChunk, encoder: &str) -> Result<()> {
        use rusqlite::params;
        let kind = chunk
            .metadata
            .get("kind")
            .cloned()
            .unwrap_or_else(|| "symbol".into());
        let name = chunk.metadata.get("symbol").cloned().unwrap_or_default();
        let encoder = encoder.to_owned();

        self.with_lock(move |conn| {
            conn.execute(
                "INSERT INTO chunks (id, file, start_line, end_line, content, kind, name)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                   file = ?2, start_line = ?3, end_line = ?4, content = ?5,
                   kind = ?6, name = ?7",
                params![
                    chunk.id,
                    chunk.file,
                    chunk.start_line as i64,
                    chunk.end_line as i64,
                    chunk.content,
                    kind,
                    name
                ],
            )
            .context("insertion du chunk")?;
            let rowid = conn.last_insert_rowid();

            Self::ensure_vec_table(conn, chunk.embedding.len(), &encoder)?;

            // Replace : retire un éventuel ancien vecteur pour ce rowid.
            conn.execute("DELETE FROM chunks_vec WHERE rowid = ?1", params![rowid])
                .ok();
            conn.execute(
                "INSERT INTO chunks_vec(rowid, embedding) VALUES (?1, ?2)",
                params![rowid, Self::vec_to_blob(&chunk.embedding)],
            )
            .context("insertion du vecteur (dimension incompatible ?)")?;
            Ok(())
        })
    }

    async fn search(&self, vec: &[f32], top_k: usize) -> Result<Vec<Hit>> {
        use rusqlite::params;
        self.with_lock(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT c.id, c.file, c.start_line, c.end_line, c.content, c.kind, c.name, v.distance
                     FROM chunks_vec v JOIN chunks c ON c.rowid = v.rowid
                     WHERE v.embedding MATCH ?1 AND k = ?2
                     ORDER BY v.distance",
                )
                .context("requête vec0 (extension sqlite-vec chargée ?)")?;
            let rows = stmt
                .query_map(params![Self::vec_to_blob(vec), top_k as i64], |r| {
                    let id: String = r.get(0)?;
                    let file: String = r.get(1)?;
                    let start: i64 = r.get(2)?;
                    let end: i64 = r.get(3)?;
                    let content: String = r.get(4)?;
                    let kind: String = r.get(5)?;
                    let name: String = r.get(6)?;
                    let distance: f32 = r.get(7)?;
                    let mut metadata = HashMap::new();
                    metadata.insert("kind".to_string(), kind);
                    metadata.insert("symbol".to_string(), name);
                    Ok(Hit {
                        chunk: CodeChunk {
                            id,
                            content,
                            file,
                            start_line: start.max(0) as usize,
                            end_line: end.max(0) as usize,
                            // vecteur non rechargé (économie mémoire)
                            embedding: Vec::new(),
                            metadata,
                        },
                        score: distance,
                    })
                })
                .context("exécution de la recherche vec0")?;
            let mut hits = Vec::new();
            for row in rows {
                hits.push(row.context("ligne de résultat vec0")?);
            }
            Ok(hits)
        })
    }

    async fn encoder(&self) -> Result<String> {
        self.with_lock(|conn| {
            Ok(conn
                .query_row(
                    "SELECT value FROM vector_meta WHERE key = 'encoder'",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or_default())
        })
    }

    async fn meta(&self) -> Result<StoreMeta> {
        let encoder = self.encoder().await?;
        self.with_lock(|conn| {
            let dim: String = conn
                .query_row("SELECT value FROM vector_meta WHERE key = 'dim'", [], |r| {
                    r.get(0)
                })
                .unwrap_or_default();
            Ok(StoreMeta {
                dim: dim.parse().unwrap_or(0),
                encoder,
            })
        })
    }
}

// ─── Qdrant (REST) ──────────────────────────────────────────────────────────

/// Qdrant via son API REST (HTTP/JSON), avec timeout et sans journalisation
/// du contenu (codes/tailles uniquement).
pub struct QdrantBackend {
    base: String,
    collection: String,
    http: reqwest::Client,
}

impl QdrantBackend {
    pub fn new(url: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .context("client HTTP Qdrant")?;
        Ok(Self {
            base: url.trim_end_matches('/').to_owned(),
            collection: "echo_chunks".to_string(),
            http,
        })
    }

    async fn decode(res: reqwest::Response) -> Result<Value> {
        let status = res.status();
        let body: Value = res
            .json()
            .await
            .with_context(|| format!("réponse Qdrant (HTTP {})", status.as_u16()))?;
        if !status.is_success() {
            anyhow::bail!("Qdrant a refusé la requête (HTTP {})", status.as_u16());
        }
        Ok(body)
    }

    /// Crée (ou recrée si la dimension change) la collection Qdrant.
    async fn ensure_collection(&self, dim: usize) -> Result<()> {
        let url = self.get_collection_url("");
        let collection_exists = self
            .http
            .get(&url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        match collection_exists {
            true => {
                // Collection existante : lire la dimension configurée.
                let body = self
                    .http
                    .get(&url)
                    .send()
                    .await
                    .context("lecture collection Qdrant")?
                    .json::<Value>()
                    .await
                    .context("JSON collection Qdrant")?;
                let existing = body
                    .pointer("/result/config/params/vectors_config/params/size")
                    .map(|v| v.as_u64().unwrap_or(0));
                if existing == Some(dim as u64) {
                    Ok(())
                } else {
                    tracing::warn!(
                        "Qdrant : dimension changée ({:?} → {}) : collection recréée",
                        existing,
                        dim
                    );
                    self.http
                        .delete(self.get_collection_url(""))
                        .send()
                        .await
                        .and_then(|r| r.error_for_status())
                        .context("suppression collection Qdrant")?;
                    self.create_collection(dim).await
                }
            }
            false => self.create_collection(dim).await,
        }
    }

    fn get_collection_url(&self, suffix: &str) -> String {
        format!("{}/collections/{}{}", self.base, self.collection, suffix)
    }

    async fn create_collection(&self, dim: usize) -> Result<()> {
        let body = json!({
            "vectors": { "size": dim, "distance": "Cosine" }
        });
        let res = self
            .http
            .put(self.get_collection_url(""))
            .json(&body)
            .send()
            .await
            .context("création collection Qdrant")?;
        Self::decode(res).await.map(|_| ())
    }
}

#[async_trait::async_trait]
impl VectorBackend for QdrantBackend {
    async fn upsert(&self, chunk: &CodeChunk, _encoder: &str) -> Result<()> {
        let dim = chunk.embedding.len();
        self.ensure_collection(dim).await?;
        // id textuel stable → u64 Qdrant
        let id: u64 = chunk.id.bytes().fold(0x_cbf2_9ce4_8422_2325u64, |acc, b| {
            (acc ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
        let mut payload = json!({
            "file": chunk.file,
            "start_line": chunk.start_line,
            "end_line": chunk.end_line,
            "content": chunk.content,
        });
        for (k, v) in &chunk.metadata {
            payload[k.as_str()] = json!(v);
        }
        let body = json!({
            "points": [{
                "id": id,
                "vector": chunk.embedding,
                "payload": payload,
            }]
        });
        let url = format!(
            "{}/collections/{}/points?wait=true",
            self.base, self.collection
        );
        let res = self
            .http
            .put(&url)
            .json(&body)
            .send()
            .await
            .context("upsert Qdrant")?;
        Self::decode(res).await.map(|_| ())
    }

    async fn search(&self, vec: &[f32], top_k: usize) -> Result<Vec<Hit>> {
        let body = json!({
            "vector": vec,
            "limit": top_k,
            "with_payload": true,
        });
        let url = format!(
            "{}/collections/{}/points/search",
            self.base, self.collection
        );
        let res = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("recherche Qdrant")?;
        let body = Self::decode(res).await?;
        let points = body
            .pointer("/result")
            .and_then(Value::as_array)
            .context("résultat Qdrant inattendu")?;

        let mut hits = Vec::new();
        for p in points {
            let payload = p.get("payload").cloned().unwrap_or(json!({}));
            let get = |k: &str| {
                payload
                    .get(k)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            let mut metadata = HashMap::new();
            for k in ["kind", "symbol"] {
                let v = get(k);
                if !v.is_empty() {
                    metadata.insert(k.to_string(), v);
                }
            }
            hits.push(Hit {
                chunk: CodeChunk {
                    id: p
                        .get("id")
                        .and_then(Value::as_u64)
                        .map(|v| format!("{v:x}"))
                        .unwrap_or_default(),
                    content: get("content"),
                    file: get("file"),
                    start_line: payload
                        .get("start_line")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as usize,
                    end_line: payload.get("end_line").and_then(Value::as_u64).unwrap_or(0) as usize,
                    embedding: Vec::new(),
                    metadata,
                },
                score: p.get("score").and_then(Value::as_f64).unwrap_or(0.0) as f32,
            });
        }
        Ok(hits)
    }

    async fn encoder(&self) -> Result<String> {
        // L'encodage n'est pas géré par Qdrant : il dépend de l'embedder
        // choisi côté application (étiquette fournie par l'embedder actif).
        Ok("remote:not-managed".to_string())
    }

    async fn meta(&self) -> Result<StoreMeta> {
        Ok(StoreMeta {
            dim: 0,
            encoder: self.encoder().await?,
        })
    }
}

// ─── Façade ─────────────────────────────────────────────────────────────────

pub struct VectorStore {
    backend: Box<dyn VectorBackend>,
}

impl VectorStore {
    pub fn sqlite(path: &str) -> Result<Self> {
        Ok(Self {
            backend: Box::new(SqliteVecBackend::new(path)?),
        })
    }

    pub fn qdrant(url: &str) -> Result<Self> {
        Ok(Self {
            backend: Box::new(QdrantBackend::new(url)?),
        })
    }

    pub async fn upsert(&self, chunk: &CodeChunk, encoder: &str) -> Result<()> {
        self.backend.upsert(chunk, encoder).await
    }

    pub async fn search(&self, vec: &[f32], top_k: usize) -> Result<Vec<Hit>> {
        self.backend.search(vec, top_k).await
    }

    pub async fn encoder(&self) -> Result<String> {
        self.backend.encoder().await
    }

    pub async fn meta(&self) -> Result<StoreMeta> {
        self.backend.meta().await
    }
}

// ─── Tests (hors ligne) ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::embedder::{Embedder, HashEmbedder};

    fn chunk(id: &str, content: &str, e: &[f32]) -> CodeChunk {
        let mut metadata = HashMap::new();
        metadata.insert("kind".to_string(), "function".to_string());
        metadata.insert("symbol".to_string(), id.to_string());
        CodeChunk {
            id: id.to_string(),
            content: content.to_string(),
            file: "src/test.py".to_string(),
            start_line: 1,
            end_line: 2,
            embedding: e.to_vec(),
            metadata,
        }
    }

    #[tokio::test]
    async fn sqlite_vec_roundtrip_and_ranking() {
        let e = HashEmbedder::new(384);
        let backend = SqliteVecBackend::new(":memory:").unwrap();

        let texts = [
            "fonction d'authentification utilisateur oauth",
            "graphe de dépendances tree-sitter ast",
            "authentification oauth2 session utilisateur",
        ];
        let owned: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
        let (vecs, encoder) = e.embed(&owned).await.unwrap();
        assert_eq!(encoder, "hash:384");

        for (i, t) in texts.iter().enumerate() {
            backend
                .upsert(&chunk(&format!("c{i}"), t, &vecs[i]), "hash:384")
                .await
                .unwrap();
        }

        let meta = backend.meta().await.unwrap();
        assert_eq!(meta.dim, 384);
        assert_eq!(meta.encoder, "hash:384");

        let (q, _) = e
            .embed(&["authentification oauth utilisateur".to_string()])
            .await
            .unwrap();
        let hits = backend.search(&q[0], 2).await.unwrap();
        assert_eq!(hits.len(), 2);
        let first = &hits[0].chunk.content;
        assert!(
            first.contains("authentification") && first.contains("oauth"),
            "1er résultat incohérent : {first}"
        );
        assert!(hits[0].score <= hits[1].score, "tri par distance");
        assert_eq!(hits[0].chunk.metadata.get("kind").unwrap(), "function");
    }

    #[tokio::test]
    async fn sqlite_vec_recreates_table_on_dimension_change() {
        let backend = SqliteVecBackend::new(":memory:").unwrap();
        backend
            .upsert(&chunk("a", "alpha", &[0.1f32; 384]), "hash:384")
            .await
            .unwrap();
        // Nouveau modèle → dimension différente : la table est régénérée.
        backend
            .upsert(&chunk("b", "beta", &[0.2f32; 128]), "hash:128")
            .await
            .unwrap();
        let meta = backend.meta().await.unwrap();
        assert_eq!(meta.dim, 128);
        let hits = backend.search(&[0.2f32; 128], 5).await.unwrap();
        assert_eq!(hits.len(), 1, "seul le nouveau vecteur subsiste");
        assert_eq!(hits[0].chunk.id, "b");
    }

    #[tokio::test]
    async fn sqlite_vec_persists_meta_on_reopen() {
        let dir = std::env::temp_dir().join(format!("echo-vec-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("test.db");
        {
            let backend = SqliteVecBackend::new(db.to_str().unwrap()).unwrap();
            backend
                .upsert(&chunk("a", "alpha", &[0.1f32; 384]), "hash:384")
                .await
                .unwrap();
        }
        let backend = SqliteVecBackend::new(db.to_str().unwrap()).unwrap();
        let meta = backend.meta().await.unwrap();
        assert_eq!(meta.dim, 384);
        std::fs::remove_dir_all(&dir).ok();
    }
}
