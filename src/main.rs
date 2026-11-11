//! Echo-Rust — point d'entrée principal.
//!
//! Orchestration : config → acteurs → services → workflow → TUI/CLI.

mod config;
mod actors;
mod services;
mod state;
mod tui;

use anyhow::{Context, Result};
use ractor::Actor;
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

    let (ast_ref, _) = Actor::spawn(
        Some("ast".into()),
        actors::AstActor,
        (),
    )
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
        actors::SandboxActor::new(
            cfg.wasm.clone(),
            cfg.docker.clone(),
            cfg.sandbox.clone(),
        ),
        (),
    )
    .await
    .context("spawn SandboxActor")?;

    tracing::info!("4 acteurs opérationnels ✓");

    // ── Services ──────────────────────────────────────────────────────
    let graph_svc = services::GraphService::connect(&cfg.neo4j, true)
        .await
        .context("GraphService")?;
    let (n, e) = graph_svc.stats();
    tracing::info!("GraphService OK ({n} nœuds, {e} arêtes)");

    let _tool_exec = services::ToolExecutor::new(workspace.clone());
    tracing::info!("ToolExecutor OK");

    // ── Arguments ─────────────────────────────────────────────────────
    let mut args = std::env::args().skip(1);

    let repo_url = args.next().unwrap_or_else(|| {
        "https://github.com/example/target-repo.git".to_string()
    });

    let issue = args.next().unwrap_or_else(|| {
        "NullPointerError dans user_auth lors du traitement OAuth".to_string()
    });

    tracing::info!("Dépôt cible : {repo_url}");
    tracing::info!("Issue : {issue}");

    // ── État initial ──────────────────────────────────────────────────
    //let initial = AgentState::new(
    //    "NullPointerError dans user_auth lors du traitement OAuth".into(),
    //    "https://github.com/example/target-repo.git".into(),
    //);
    let initial = AgentState::new(issue, repo_url);

    let shared_state = Arc::new(Mutex::new(initial.clone()));

    // ── Workflow ──────────────────────────────────────────────────────
    let workflow = AgentWorkflow::new(
        cfg.agent.max_iterations,
        cfg.agent.enable_self_healing,
    );

    let use_tui = std::env::var("ECHO_TUI")
        .map(|v| v == "true")
        .unwrap_or(false);

    if use_tui {
        tracing::info!("Mode TUI activé");

        let st = shared_state.clone();
        let handle = tokio::spawn(async move { workflow.run(initial).await });

        let mut app = tui::TuiApp::new(st);
        app.push_log("Echo-Rust démarré".into());
        app.run().await?;

        let final_state = handle.await??;
        tracing::info!("Terminé — phase={}, iters={}", final_state.phase, final_state.iteration);
    } else {
        tracing::info!("Mode CLI");

        let final_state = workflow.run(initial).await?;

        tracing::info!("═══ Résultat ═══");
        tracing::info!("  Phase      : {}", final_state.phase);
        tracing::info!("  Itérations : {}", final_state.iteration);
        tracing::info!(
            "  Script     : {}",
            final_state.reproduction_script.as_deref().unwrap_or("aucun")
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