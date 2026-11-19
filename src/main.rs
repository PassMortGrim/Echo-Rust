//! Echo-Rust — point d'entrée principal.
//!
//! Orchestration : config → acteurs → services → workflow → TUI/CLI.
//! La bibliothèque (lib.rs) est réutilisée par les tests d'intégration.

use echo_rust::{actors, config, services, state, tui};

use anyhow::{Context, Result};
use ractor::{Actor, ActorRef};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing_subscriber::EnvFilter;

use config::Settings;
use state::graph::{AgentState, AgentWorkflow};

#[tokio::main]
async fn main() -> Result<()> {
    // ── Logging ───────────────────────────────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tracing::info!("🚀 Echo-Rust v0.2.0 — démarrage");

    // ── Configuration ─────────────────────────────────────────────────
    let cfg = Settings::load_or_default();
    tracing::info!(
        "Config OK — model={}, sandbox={}, workers={}",
        cfg.llm.model,
        cfg.sandbox.mode,
        cfg.agent.parallel_workers,
    );

    // ── Workspace ─────────────────────────────────────────────────────
    let workspace = PathBuf::from("./workspace");
    tokio::fs::create_dir_all(&workspace).await?;

    // ── Spawn des acteurs ─────────────────────────────────────────────
    let (git_ref, _) = Actor::spawn(
        Some("git".into()),
        actors::GitActor::new(workspace.clone()),
        (),
    )
    .await
    .context("spawn GitActor")?;

    let (ast_ref, _) = Actor::spawn(Some("ast".into()), actors::AstActor, ())
        .await
        .context("spawn AstActor")?;

    let (llm_ref, _) = Actor::spawn(
        Some("llm".into()),
        actors::LlmActor::new(cfg.llm.clone()),
        (),
    )
    .await
    .context("spawn LlmActor")?;

    let (sandbox_ref, _) = Actor::spawn(
        Some("sandbox".into()),
        actors::SandboxActor::new(cfg.wasm.clone(), cfg.docker.clone(), cfg.sandbox.clone()),
        (),
    )
    .await
    .context("spawn SandboxActor")?;

    tracing::info!("4 acteurs opérationnels ✓");

    // ─── Index vectoriel (RAG hybride) ──────────────
    // Optionnel : activé si [agent] enable_hybrid_rag = true et [vector_db]
    // backend != "off". Embedder API OpenAI-compatible si embeddings_model
    // est défini, sinon fallback hash déterministe (recherche dégradée).
    let vector_ref: Option<ActorRef<crate::actors::VectorMessage>> =
        if cfg.agent.enable_hybrid_rag && cfg.vector_db.backend != "off" {
            let backend_res: anyhow::Result<Arc<dyn services::vector_store::VectorBackend>> =
                if cfg.vector_db.backend == "qdrant" {
                    services::vector_store::QdrantBackend::new(&cfg.vector_db.qdrant_url)
                        .map(|b| Arc::new(b) as Arc<dyn services::vector_store::VectorBackend>)
                } else {
                    services::vector_store::SqliteVecBackend::new(&cfg.vector_db.sqlite_path)
                        .map(|b| Arc::new(b) as Arc<dyn services::vector_store::VectorBackend>)
                };
            match backend_res {
                Ok(backend) => {
                    let embedder = services::embedder::make_embedder(
                        &cfg.llm.base_url,
                        &cfg.llm.api_key,
                        &cfg.llm.embeddings_model,
                        cfg.vector_db.embedding_dim,
                        cfg.vector_db.batch_size,
                    );
                    match Actor::spawn(
                        Some("vector".into()),
                        actors::VectorActor::new(embedder, backend),
                        (),
                    )
                    .await
                    {
                        Ok((r, _)) => Some(r),
                        Err(e) => {
                            tracing::error!("spawn VectorActor : {e} — index vectoriel désactivé");
                            None
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("backend vectoriel : {e} — index vectoriel désactivé");
                    None
                }
            }
        } else {
            tracing::info!("RAG vectoriel désactivé (config)");
            None
        };

    // ── Services ──────────────────────────────────────────────────────
    let graph_svc = services::GraphService::connect(&cfg.neo4j, true)
        .await
        .context("GraphService")?;
    let (n, e) = graph_svc.stats();
    tracing::info!("GraphService OK ({n} nœuds, {e} arêtes)");

    // ToolExecutor : instancié PAR le workflow, raciné sur chaque dépôt cloné
    // (voir state/graph.rs — génération ReAct et self-healing).
    tracing::info!("ToolExecutor OK (branché au workflow, par dépôt cloné)");

    // ── Arguments ─────────────────────────────────────────────────────
    let mut args = std::env::args().skip(1);

    let repo_url = args
        .next()
        .unwrap_or_else(|| "https://github.com/example/target-repo.git".to_string());

    let issue = args
        .next()
        .unwrap_or_else(|| "NullPointerError dans user_auth lors du traitement OAuth".to_string());

    tracing::info!("Dépôt cible : {repo_url}");
    tracing::info!("Issue : {issue}");

    // ── État initial ──────────────────────────────────────────────────
    let initial = AgentState::new(issue, repo_url);

    let shared_state = Arc::new(Mutex::new(initial.clone()));

    // ── Workflow ──────────────────────────────────────────────────────
    // Phase 1 : orchestration réelle — le workflow délègue à chaque acteur
    // (clone → AST → LLM + tools → sandbox) puis boucle en self-healing.
    let workflow = AgentWorkflow::new(
        git_ref.clone(),
        ast_ref.clone(),
        llm_ref.clone(),
        sandbox_ref.clone(),
        vector_ref.clone(),
        workspace.clone(),
        cfg.agent.max_iterations,
        cfg.agent.enable_self_healing,
        cfg.vector_db.top_k,
    );

    let use_tui = std::env::var("ECHO_TUI")
        .map(|v| v == "true")
        .unwrap_or(false);

    if use_tui {
        tracing::info!("Mode TUI activé");

        let st = shared_state.clone();
        let handle = tokio::spawn(async move { workflow.run(initial, Some(st)).await });

        let mut app = tui::TuiApp::new(shared_state);
        app.push_log("Echo-Rust démarré".into());
        app.run().await?;

        let final_state = handle.await??;
        tracing::info!(
            "Terminé — phase={}, iters={}",
            final_state.phase,
            final_state.iteration
        );
    } else {
        tracing::info!("Mode CLI");

        let final_state = workflow.run(initial, Some(shared_state)).await?;

        tracing::info!("═══ Résultat ═══");
        tracing::info!("  Phase      : {}", final_state.phase);
        tracing::info!("  Itérations : {}", final_state.iteration);
        tracing::info!(
            "  Script     : {}",
            final_state
                .reproduction_script
                .as_deref()
                .unwrap_or("aucun")
        );
    }

    // ── Arrêt propre ──────────────────────────────────────────────────
    git_ref.stop(Some("git shutdown".into()));
    ast_ref.stop(Some("ast shutdown".into()));
    llm_ref.stop(Some("llm shutdown".into()));
    sandbox_ref.stop(Some("sandbox shutdown".into()));

    tracing::info!("👋 Echo-Rust arrêté proprement");
    Ok(())
}
