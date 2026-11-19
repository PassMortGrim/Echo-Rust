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
use echo_rust::state::graph::AgentState;
use echo_rust::{actors, services};

mod common;
use common::{init_git_repo, llm_script, llm_tool_call_list_files, spawn_fake_llm};

use echo_rust::state::graph::AgentWorkflow;
use ractor::Actor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
        llm_tool_call_list_files(),
        llm_script(
            "println!(\"bug reproduit\");",
            "Voici le script de reproduction :",
        ),
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
        llm_script(
            "let x: Option<u32> = None\nprintln!(\"never: {}\", x.unwrap())",
            "Script incorrect :",
        ),
        llm_script("println!(\"bug reproduit\");", "Corrigé :"),
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
