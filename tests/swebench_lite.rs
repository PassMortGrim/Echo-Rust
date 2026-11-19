//! Harnais SWE-bench-lite — Echo-Rust (offline-first, heavy = nightly).
//!
//! Chaque instance (`bench/instances.json`, ou `ECHO_SWEBENCH_INSTANCES=path`
//! pour tester N'IMPORTE QUEL dépôt) est un mini-SWE-bench :
//!
//! 1. **Vrai clonage git** du dépôt cible (GitActor — chemin local ou URL) ;
//! 2. génération du script de reproduction : LLM **réel** (clé uniquement
//!    via `ECHO_LLM__API_KEY`) ou LLM **scripté** (mode hors ligne, mécanique) ;
//! 3. exécution réelle dans le sandbox : **wasm** pour Rust (wasmtime, offline),
//!    **Docker** pour Python/JS (conteneur durci, réseau coupé) ;
//! 4. **verdict** façon SWE-bench : PASS si `Phase::Done` + script exécutable
//!    ET sortie attendue (`expect.stdout_contains`) ; SKIP si prérequis
//!    manquants (jamais bloquant) ; FAIL sinon.
//!
//! Modes :
//! - `cargo test --test swebench_lite`              → instances hors ligne
//! - `cargo test --test swebench_lite -- --ignored` → clones réels + LLM réel
//!   (nightly/manuel, non bloquant — le verdict est un rapport, pas une assertion).
//!
//! Rapport : tableau lisible + `bench/report.json` (artefact nightly).

mod common;

use common::{llm_script, llm_tool_call_list_files};

use echo_rust::config::{LlmConfig, Settings};
use echo_rust::state::graph::{AgentState, AgentWorkflow, Phase};
use echo_rust::{actors, services};

use anyhow::Result;
use ractor::Actor;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

// ───────────── Modèle d'instance (bench/instances.json) ─────────────────

#[derive(Debug, Deserialize)]
struct InstancesFile {
    instances: Vec<Instance>,
}

#[derive(Debug, Deserialize)]
struct Instance {
    instance_id: String,
    /// URL git distante, chemin local, ou "LOCAL:CARGO_MANIFEST_DIR"
    /// (= ce dépôt : vrai code, vrai clone, zéro réseau).
    repo_url: String,
    #[serde(default)]
    base_commit: Option<String>,
    /// Langage attendu du script ("rust" → wasm, sinon Docker).
    language: String,
    issue: String,
    /// Mode hors ligne : suite de réponses LLM simulées —
    /// `"TOOL_CALL: <nom>"`, `"SCRIPT: <corps>"` ou JSON brut.
    #[serde(default)]
    scripted_llm: Option<Vec<String>>,
    expect: Expect,
    /// Prérequis d'exécution : "docker", "network", "llm".
    #[serde(default)]
    requires: Vec<String>,
    /// Motif documenté de non-exécution en CI ordinaire.
    #[serde(default)]
    ignore_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Expect {
    #[serde(default = "default_exit_code")]
    exit_code: i32,
    #[serde(default)]
    stdout_contains: Vec<String>,
}

fn default_exit_code() -> i32 {
    0
}

// ───────────── Verdicts + rapport ────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct VerdictReportEntry {
    status: String, // PASS | FAIL | SKIP
    detail: String,
}

#[derive(Debug, Default, serde::Serialize)]
struct Report {
    #[serde(rename = "resolution_rate")]
    resolution_rate: f64,
    entries: BTreeMap<String, VerdictReportEntry>,
}

// ───────────── Harnais ───────────────────────────────────────────────────

/// Charge le jeu d'instances (par défaut bench/instances.json, surchargeable).
fn load_instances() -> Vec<Instance> {
    let path = std::env::var("ECHO_SWEBENCH_INSTANCES")
        .unwrap_or_else(|_| "bench/instances.json".to_string());
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("jeu d'instances introuvable ({path}) : {e}"));
    let parsed: InstancesFile = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("instance JSON invalide ({path}) : {e}"));
    parsed.instances
}

/// Prérequis absents ? → SKIP (une instance non exécutable n'est jamais un FAIL).
fn missing_requirement(inst: &Instance) -> Option<String> {
    for r in &inst.requires {
        let ok = match r.as_str() {
            "docker" => {
                Path::new("/var/run/docker.sock").exists()
                    || std::env::var("DOCKER_HOST")
                        .map(|v| !v.is_empty())
                        .unwrap_or(false)
            }
            "network" => std::net::ToSocketAddrs::to_socket_addrs(&("github.com", 443))
                .map(|mut a| a.next().is_some())
                .unwrap_or(false),
            "llm" => std::env::var("ECHO_LLM__API_KEY")
                .map(|k| !k.trim().is_empty())
                .unwrap_or(false),
            "local_repo" => true,
            other => {
                println!("  (prérequis inconnu « {other} » traité comme satisfait)");
                true
            }
        };
        if !ok {
            return Some(r.clone());
        }
    }
    None
}

/// Résout la spécification de dépôt : "LOCAL:CARGO_MANIFEST_DIR" = ce dépôt.
fn resolve_repo_url(spec: &str) -> String {
    if let Some(rest) = spec.strip_prefix("LOCAL:CARGO_MANIFEST_DIR") {
        format!("{}{}", env!("CARGO_MANIFEST_DIR"), rest)
    } else {
        spec.to_owned()
    }
}

/// Traduit les réponses scriptées en réponses du faux serveur LLM.
fn scripted_responses(inst: &Instance) -> Vec<String> {
    inst.scripted_llm
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|entry| {
            if let Some(_name) = entry.strip_prefix("TOOL_CALL:") {
                llm_tool_call_list_files()
            } else if let Some(body) = entry.strip_prefix("SCRIPT:") {
                llm_script(body.trim(), "Script de reproduction :")
            } else {
                entry.to_owned() // JSON brut OpenAI-compatible
            }
        })
        .collect()
}

/// Configuration LLM du harnais : réel (config + env) ou scripté (faux serveur).
fn llm_config_for(inst: &Instance, use_real_llm: bool, base_url: String) -> LlmConfig {
    if use_real_llm && inst.scripted_llm.is_none() {
        let cfg = Settings::load_or_default();
        LlmConfig {
            api_key: std::env::var("ECHO_LLM__API_KEY").unwrap_or_default(),
            base_url: cfg.llm.base_url,
            model: cfg.llm.model,
            temperature: cfg.llm.temperature,
            max_tokens: cfg.llm.max_tokens,
            embeddings_model: cfg.llm.embeddings_model,
        }
    } else {
        LlmConfig {
            base_url,
            ..Default::default()
        }
    }
}

/// RPC générique : envoie un message typé et attend la réponse.
async fn call<TMsg: ractor::Message, T>(
    actor: &ractor::ActorRef<TMsg>,
    build: impl FnOnce(oneshot::Sender<Result<T>>) -> TMsg,
) -> Result<T> {
    let (tx, rx) = oneshot::channel();
    actor
        .cast(build(tx))
        .map_err(|e| anyhow::anyhow!("envoi à l'acteur : {e}"))?;
    rx.await
        .map_err(|e| anyhow::anyhow!("réponse perdue : {e}"))?
}

/// Exécute une instance de bout en bout — **vrai clonage git, vrai sandbox**.
async fn run_instance(inst: &Instance, use_real_llm: bool) -> VerdictReportEntry {
    // SKIP : prérequis jamais bloquants.
    if let Some(missing) = missing_requirement(inst) {
        let reason = inst.ignore_reason.clone().unwrap_or_default();
        return VerdictReportEntry {
            status: "SKIP".into(),
            detail: format!("prérequis manquant : {missing}. {reason}"),
        };
    }

    let repo_url = resolve_repo_url(&inst.repo_url);
    let repo_url_derived = repo_url.clone();
    let ws: PathBuf = std::env::temp_dir().join(format!("echo-swe-{}", uuid::Uuid::new_v4()));
    if std::fs::create_dir_all(&ws).is_err() {
        return VerdictReportEntry {
            status: "FAIL".into(),
            detail: "workspace temporaire non créé".into(),
        };
    }

    // ── 1. Clonage git réel (GitActor) + checkout éventuel ───────────────
    let (git_ref, _) = match Actor::spawn(None, actors::GitActor::new(ws.clone()), ()).await {
        Ok(r) => r,
        Err(e) => {
            return VerdictReportEntry {
                status: "FAIL".into(),
                detail: format!("spawn GitActor : {e}"),
            }
        }
    };
    let repo_path = match call(&git_ref, move |reply| actors::GitMessage::Clone {
        url: repo_url_derived.clone(),
        reply: reply.into(),
    })
    .await
    {
        Ok(p) => p,
        Err(e) => {
            git_ref.stop(None);
            return VerdictReportEntry {
                status: "FAIL".into(),
                detail: format!("clone {repo_url} : {e}"),
            };
        }
    };
    if let Some(commit) = inst.base_commit.clone() {
        let repo_path_for_checkout = repo_path.clone();
        let _ = call(&git_ref, move |reply| actors::GitMessage::Checkout {
            repo_path: repo_path_for_checkout.clone(),
            commit,
            reply: reply.into(),
        })
        .await;
    }

    // ── 2. LLM : réel (env/config) ou scripté (faux serveur offline) ─────
    let scripted = scripted_responses(inst);
    let real_llm = use_real_llm && scripted.is_empty();
    let base_url = if real_llm {
        Settings::load_or_default().llm.base_url
    } else {
        common::spawn_fake_llm(scripted).await
    };
    let llm_cfg = llm_config_for(inst, use_real_llm, base_url);

    // ── 3. Acteurs réels (AST, LLM, sandbox, RAG hash) + workflow ────────
    let (ast_ref, _) = Actor::spawn(None, actors::AstActor, ()).await.unwrap();
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

    let workflow = AgentWorkflow::new(
        git_ref.clone(),
        ast_ref,
        llm_ref,
        sandbox_ref.clone(),
        Some(vector_ref.clone()),
        ws.clone(),
        5,
        true,
        5,
    );

    let initial = AgentState::new(inst.issue.clone(), repo_url.clone());
    let outcome = tokio::time::timeout(Duration::from_secs(300), workflow.run(initial, None)).await;

    git_ref.stop(None);
    vector_ref.stop(None);

    let outcome = match outcome {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return VerdictReportEntry {
                status: "FAIL".into(),
                detail: format!("workflow : {e}"),
            }
        }
        Err(_) => {
            return VerdictReportEntry {
                status: "FAIL".into(),
                detail: "timeout 300 s (workflow bloqué)".into(),
            }
        }
    };

    if !matches!(outcome.phase, Phase::Done) {
        return VerdictReportEntry {
            status: "FAIL".into(),
            detail: format!("phase finale : {:?}", outcome.phase),
        };
    }
    let Some(script) = outcome.reproduction_script.clone() else {
        return VerdictReportEntry {
            status: "FAIL".into(),
            detail: "le workflow n'a pas produit de script".into(),
        };
    };

    // ── 4. Vérification indépendante : exécution directe dans le sandbox ─
    let exec = call(&sandbox_ref, move |reply| actors::SandboxMessage::Execute {
        code: script,
        language: inst.language.clone(),
        repo_path: Some(repo_path),
        reply: reply.into(),
    })
    .await;
    sandbox_ref.stop(None);
    let exec = match exec {
        Ok(r) => r,
        Err(e) => {
            return VerdictReportEntry {
                status: "FAIL".into(),
                detail: format!("sandbox : {e}"),
            }
        }
    };

    if exec.exit_code != inst.expect.exit_code {
        return VerdictReportEntry {
            status: "FAIL".into(),
            detail: format!(
                "exit_code {} ≠ {}, stderr : {}",
                exec.exit_code,
                inst.expect.exit_code,
                exec.stderr.chars().take(200).collect::<String>()
            ),
        };
    }
    for expected in &inst.expect.stdout_contains {
        if !exec.stdout.contains(expected.as_str()) {
            return VerdictReportEntry {
                status: "FAIL".into(),
                detail: format!(
                    "sortie sans « {expected} » (stdout : {} octets)",
                    exec.stdout.len()
                ),
            };
        }
    }

    let _ = std::fs::remove_dir_all(&ws);
    VerdictReportEntry {
        status: "PASS".into(),
        detail: format!("exit {} en {} ms", exec.exit_code, exec.elapsed_ms),
    }
}

/// Exécute les instances filtrées, écrit le rapport, affiche le tableau.
async fn run_bench(all: bool) -> Report {
    let instances = load_instances();
    let mut report = Report::default();

    println!(
        "\n════ SWE-bench-lite : {} instance(s) (mode {}) ════",
        instances.len(),
        if all {
            "complet (clones/LLM réels)"
        } else {
            "hors ligne (mécanique)"
        }
    );
    for inst in &instances {
        // Mode mécanique : uniquement les instances scriptées hors ligne.
        if !all && inst.scripted_llm.is_none() {
            report.entries.insert(
                inst.instance_id.clone(),
                VerdictReportEntry {
                    status: "SKIP".into(),
                    detail: "hors ligne : instance non scriptée".into(),
                },
            );
            continue;
        }
        println!("  ▶ {}", inst.instance_id);
        let verdict = run_instance(inst, all).await;
        println!("    → {} ({})", verdict.status, verdict.detail);
        report.entries.insert(inst.instance_id.clone(), verdict);
    }

    // Taux de résolution façon SWE-bench : PASS / (PASS + FAIL), SKIP hors dénominateur.
    let (pass, fail) = (
        report
            .entries
            .values()
            .filter(|v| v.status == "PASS")
            .count(),
        report
            .entries
            .values()
            .filter(|v| v.status == "FAIL")
            .count(),
    );
    report.resolution_rate = if pass + fail > 0 {
        pass as f64 / (pass + fail) as f64
    } else {
        0.0
    };

    println!(
        "\n  {pass} PASS / {fail} FAIL — taux de résolution : {:.1} %",
        report.resolution_rate * 100.0
    );

    if let Err(e) = std::fs::write(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/report.json"),
        serde_json::to_string_pretty(&report).unwrap_or_default(),
    ) {
        println!("  (rapport non écrit : {e})");
    }
    report
}

// ───────────── Suite 1 : mécanique, hors ligne (CI) ─────────────────────

#[tokio::test]
async fn swebench_lite_offline_mechanics() {
    let report = run_bench(false).await;

    // Contrat du harnais : au moins une instance hors ligne exécutée de bout
    // en bout (vrai clone local, vrai sandbox wasm, RAG hash) et en PASS.
    let (pass, fail, skip) =
        report
            .entries
            .iter()
            .fold((0, 0, 0), |(p, f, s), (_, v)| match v.status.as_str() {
                "PASS" => (p + 1, f, s),
                "FAIL" => (p, f + 1, s),
                _ => (p, f, s + 1),
            });
    assert!(pass >= 1, "aucune instance hors ligne : {report:?}");
    assert_eq!(fail, 0, "instances en échec : {report:?}");
    // Les SKIP sont acceptés (prérequis), mais tous les non-scriptés doivent l'être.
    let _ = skip;
}

// ───────────── Suite 2 : clones réels + LLM réel (nightly, non bloquant) ─

#[tokio::test]
#[ignore = "clonages réels + LLM réel (Docker selon instances) — nightly/manuel"]
async fn swebench_lite_real_repos() {
    // Jamais bloquant : le verdict est un rapport (paper-trail), pas une exception.
    let _report = run_bench(true).await;
}
