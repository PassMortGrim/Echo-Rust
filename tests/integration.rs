//! Tests d'intégration — Phase 1 (orchestration) + Phase 3 (RAG), 100 % hors ligne.
//!
//! Principe : dépôt git local factice + faux serveur LLM (HTTP/1.1 brut,
//! OpenAI-compatible) + acteurs RÉELS (git2 → tree-sitter → reqwest →
//! wasmtime + SQLite-vec). On vérifie le câblage public de bout en bout :
//!
//! 1. issue connue → le workflow produit un script de reproduction exécutable ;
//! 2. le self-healing réinjecte l'échec sandbox et récupère à l'itération 2 ;
//! 3. l'index vectoriel (fallback hash, dimension auto-détectée puis
//!    persistée) alimente le contexte du prompt final.
//!
//! Ces tests utilisent l'API publique de la bibliothèque (echo_rust) — pas
//! d'accès au code interne : ils valident le contrat externe du prototype.

use echo_rust::config::LlmConfig;
use echo_rust::state::graph::{AgentState, AgentWorkflow};
use echo_rust::{actors, services};

use ractor::Actor;
use serde_json::json;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Crée un dépôt git local avec les fichiers donnés (1 commit).
fn init_git_repo(base: &Path, files: &[(&str, &str)]) {
    std::fs::create_dir_all(base).unwrap();
    let repo = git2::Repository::init(base).unwrap();
    for (rel, content) in files {
        let path = base.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("test", "test@echo.local").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
        .unwrap();
}

/// Faux serveur LLM OpenAI-compatible : répond successivement avec les
/// réponses fournies (comportement LLM scripté), sans réseau réel.
async fn spawn_fake_llm(responses: Vec<String>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let queue = Arc::new(Mutex::new(VecDeque::from(responses)));

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let queue = queue.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 65536];
                let mut acc: Vec<u8> = Vec::new();
                loop {
                    // Lit jusqu'à la fin des en-têtes HTTP.
                    let header_end = loop {
                        if let Some(pos) = acc.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos + 4;
                        }
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => acc.extend_from_slice(&buf[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&acc[..header_end]).to_ascii_lowercase();
                    let content_length: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:")?.trim().parse().ok())
                        .unwrap_or(0);
                    // Lit le corps complet de la requête.
                    while acc.len() < header_end + content_length {
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => acc.extend_from_slice(&buf[..n]),
                        }
                    }
                    acc.drain(..header_end + content_length);

                    let body = queue
                        .lock()
                        .ok()
                        .and_then(|mut q| q.pop_front())
                        .unwrap_or_else(|| {
                            r#"{"choices":[{"message":{"role":"assistant","content":""}}]}"#
                                .to_owned()
                        });
                    let http = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    if sock.write_all(http.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });

    format!("http://{addr}/v1")
}

/// Spawn des 5 acteurs réels (noms None : pas d'enregistrement global ractor,
/// sinon les tests parallèles entrent en collision ActorAlreadyRegistered).
/// Index vectoriel : embedder hash (fallback déterministe) + SQLite-vec en
/// mémoire — RAG complet hors ligne.
async fn spawn_all_actors(llm_base_url: String, ws: &Path) -> ActorHandles {
    let (git_ref, _) = Actor::spawn(None, actors::GitActor::new(ws.to_path_buf()), ())
        .await
        .unwrap();
    let (ast_ref, _) = Actor::spawn(None, actors::AstActor, ()).await.unwrap();
    let llm_cfg = LlmConfig {
        base_url: llm_base_url,
        ..Default::default()
    };
    let (llm_ref, _) = Actor::spawn(None, actors::LlmActor::new(llm_cfg), ())
        .await
        .unwrap();
    let (sandbox_ref, _) = Actor::spawn(
        None,
        actors::SandboxActor::new(Default::default(), Default::default(), Default::default()),
        (),
    )
    .await
    .unwrap();

    let embedder = services::embedder::make_embedder("", "", "", 384, 32);
    let backend: Arc<dyn services::vector_store::VectorBackend> =
        Arc::new(services::vector_store::SqliteVecBackend::new(":memory:").unwrap());
    let (vector_ref, _) = Actor::spawn(None, actors::VectorActor::new(embedder, backend), ())
        .await
        .unwrap();

    ActorHandles {
        git: git_ref,
        ast: ast_ref,
        llm: llm_ref,
        sandbox: sandbox_ref,
        vector: vector_ref,
    }
}

struct ActorHandles {
    git: ractor::ActorRef<actors::GitMessage>,
    ast: ractor::ActorRef<actors::AstMessage>,
    llm: ractor::ActorRef<actors::LlmMessage>,
    sandbox: ractor::ActorRef<actors::SandboxMessage>,
    vector: ractor::ActorRef<actors::VectorMessage>,
}

impl ActorHandles {
    fn stop_all(&self) {
        self.git.stop(None);
        self.ast.stop(None);
        self.llm.stop(None);
        self.sandbox.stop(None);
        self.vector.stop(None);
    }
}

/// Répertoire de travail temporaire unique (clones git + scratch).
fn temp_ws(tag: &str) -> PathBuf {
    let ws = std::env::temp_dir().join(format!("echo-it-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&ws).unwrap();
    ws
}

// ─── Scénario 1 : issue connue → script produit et exécuté ─────────────

#[tokio::test]
async fn known_issue_produces_working_script() {
    // 1. Dépôt git local factice avec le bug décrit par l'issue.
    let base = temp_ws("repo");
    init_git_repo(
        &base,
        &[(
            "app.py",
            "def add(a, b):\n    return a + b\nprint(add(2, 2))\n",
        )],
    );

    // 2. LLM scripté : d'abord un tool-call (exploration), puis le script final.
    let fake_url = spawn_fake_llm(vec![
        json!({
            "choices": [{ "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{ "id": "call-1", "type": "function",
                    "function": { "name": "list_files", "arguments": "{}" } }]
            }}],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        })
        .to_string(),
        json!({
            "choices": [{ "message": {
                "role": "assistant",
                "content": "Voici le script de reproduction :\n```rust\nprintln!(\"bug reproduit\");\n```"
            }}],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 5 }
        })
        .to_string(),
    ])
    .await;

    // 3. Acteurs réels + workflow complet.
    let ws = temp_ws("ws");
    let h = spawn_all_actors(fake_url, &ws).await;
    let workflow = AgentWorkflow::new(
        h.git.clone(),
        h.ast.clone(),
        h.llm.clone(),
        h.sandbox.clone(),
        Some(h.vector.clone()),
        ws.clone(),
        3,
        true,
        5,
    );

    let initial = AgentState::new(
        "add(2, 2) renvoie 5 au lieu de 4".into(),
        base.to_string_lossy().into_owned(),
    );
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        workflow.run(initial, None),
    )
    .await
    .expect("workflow bloqué (timeout)")
    .expect("workflow OK");

    // 4. Assertions = contrat Phase 1 (script produit) + Phase 3 (RAG).
    assert!(
        matches!(outcome.phase, echo_rust::state::graph::Phase::Done),
        "phase finale : {:?}",
        outcome.phase
    );
    assert_eq!(
        outcome.reproduction_script.as_deref(),
        Some("println!(\"bug reproduit\");\n"),
        "script attendu (extrait du fence)"
    );
    assert!(outcome.ast_nodes > 0, "AST réel attendu (> 0 nœuds)");
    assert!(outcome.repo_path.is_some());
    assert_eq!(outcome.iteration, 0, "1er passage sans self-healing");
    assert!(
        outcome.vector_context.is_some(),
        "contexte vectoriel attendu (index actif)"
    );
    let history = outcome.history.join("\n");
    assert!(
        history.contains("encoder=hash:384"),
        "ingestion hash attendue : {history}"
    );
    assert!(
        history.contains("DÉGRADÉE"),
        "recherche explicitement marquée dégradée : {history}"
    );

    h.stop_all();
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&ws);
}

// ─── Scénario 2 : self-healing — échec sandbox → stderr réinjecté ──────

#[tokio::test]
async fn self_healing_recovers_after_failing_script() {
    let base = temp_ws("repo");
    init_git_repo(
        &base,
        &[(
            "app.py",
            "def add(a, b):\n    return a + b\nprint(add(2, 2))\n",
        )],
    );

    // LLM scripté : 1er script qui ÉCHOUE à l'exécution (panic wasm, stderr
    // + exit != 0), puis script corrigé. Le self-healing doit réinjecter
    // l'erreur et récupérer à l'itération suivante.
    let fake_url = spawn_fake_llm(vec![
        json!({
            "choices": [{ "message": {
                "role": "assistant",
                "content": "Script incorrect :\n```rust\nlet x: Option<u32> = None\nprintln!(\"never: {}\", x.unwrap())\n```"
            }}]
        })
        .to_string(),
        json!({
            "choices": [{ "message": {
                "role": "assistant",
                "content": "Corrigé :\n```rust\nprintln!(\"bug reproduit\");\n```"
            }}]
        })
        .to_string(),
    ])
    .await;

    let ws = temp_ws("ws");
    let h = spawn_all_actors(fake_url, &ws).await;
    let workflow = AgentWorkflow::new(
        h.git.clone(),
        h.ast.clone(),
        h.llm.clone(),
        h.sandbox.clone(),
        Some(h.vector.clone()),
        ws.clone(),
        5,
        true, // self-healing activé
        5,
    );

    let initial = AgentState::new(
        "add(2, 2) renvoie 5 au lieu de 4".into(),
        base.to_string_lossy().into_owned(),
    );
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        workflow.run(initial, None),
    )
    .await
    .expect("workflow bloqué (timeout)")
    .expect("workflow OK");

    assert!(
        matches!(outcome.phase, echo_rust::state::graph::Phase::Done),
        "phase finale : {:?}",
        outcome.phase
    );
    assert_eq!(
        outcome.reproduction_script.as_deref(),
        Some("println!(\"bug reproduit\");\n"),
        "le script corrigé doit être retenu"
    );
    assert_eq!(
        outcome.iteration, 1,
        "le 2e passage est attendu (self-healing)"
    );
    let history = outcome.history.join("\n");
    assert!(
        history.to_lowercase().contains("self-heal") || history.to_lowercase().contains("échec"),
        "trace du self-healing attendue : {history}"
    );

    h.stop_all();
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&ws);
}
