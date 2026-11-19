//! SandboxActor — exécution isolée : Wasm (wasmtime) pour Rust, Docker (bollard) pour Python/JS/bash.
//!
//! Stratégie (Phase 2 de la roadmap) :
//!
//! - `rust`   → compilé à chaud vers `wasm32-wasip1` puis exécuté sous
//!   wasmtime avec WASI (stdio capturé en mémoire, fuel + timeout + limite
//!   mémoire, dépôt cloné préouvert en lecture seule sur `/workspace`).
//! - `python` / `javascript` / `bash` → conteneur Docker durci (réseau coupé,
//!   limites mémoire/CPU, timeout, scratch monté en écriture, repo cloné en
//!   lecture seule sur `/workspace`).
//!
//! En mode `wasm`, tout langage non compilable bascule automatiquement vers Docker.

use crate::config::{DockerConfig, SandboxConfig, WasmConfig};
use anyhow::{Context, Result};
use bollard::container::{
    Config, CreateContainerOptions, LogOutput, LogsOptions, RemoveContainerOptions,
    WaitContainerOptions,
};
use bollard::image::CreateImageOptions;
use bollard::models::HostConfig;
use bollard::Docker;
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use std::path::{Path, PathBuf};
use std::time::Duration;
use wasmtime_wasi::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::preview1;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtxBuilder, WasiP1Ctx};

// ─── Types ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub elapsed_ms: u64,
}

impl ExecResult {
    pub fn ok(&self) -> bool {
        self.exit_code == 0
    }
}

/// Messages envoyés au SandboxActor.
#[derive(Debug)]
pub enum SandboxMessage {
    Execute {
        code: String,
        language: String,
        /// Dépôt cloné à préouvrir/mounter en lecture seule (`/workspace`).
        repo_path: Option<String>,
        reply: RpcReplyPort<Result<ExecResult>>,
    },
}

/// Contexte d'exécution choisi pour un script donné.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Wasm,
    Docker,
}

/// Capacité (1 Mio) de chaque pipe de capture stdio côté wasm.
const WASM_STDIO_CAP: usize = 1024 * 1024;
/// Budget d'instructions wasm avant interruption par fuel.
const WASM_FUEL: u64 = 2_000_000_000;

// ─── Acteur ─────────────────────────────────────────────────────────────────

pub struct SandboxActor {
    wasm_cfg: WasmConfig,
    docker_cfg: DockerConfig,
    sandbox_cfg: SandboxConfig,
}

impl SandboxActor {
    pub fn new(w: WasmConfig, d: DockerConfig, s: SandboxConfig) -> Self {
        Self {
            wasm_cfg: w,
            docker_cfg: d,
            sandbox_cfg: s,
        }
    }

    /// Routage par langage : wasm strict pour Rust, Docker sinon.
    fn route(&self, language: &str) -> Backend {
        if self.sandbox_cfg.mode == "wasm" && self.wasm_cfg.enabled {
            match language.to_lowercase().as_str() {
                "rust" | "rs" => Backend::Wasm,
                _ => Backend::Docker, // bascule décidée : py/js → Docker durci
            }
        } else {
            Backend::Docker
        }
    }
}

#[async_trait::async_trait]
impl Actor for SandboxActor {
    type Msg = SandboxMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!("SandboxActor prêt (mode={})", self.sandbox_cfg.mode);
        Ok(())
    }

    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        msg: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SandboxMessage::Execute {
                code,
                language,
                repo_path,
                reply,
            } => {
                let result = match self.route(&language) {
                    Backend::Wasm => self.run_wasm_rust(&code, repo_path.as_deref()).await,
                    Backend::Docker => {
                        self.run_docker(&code, &language, repo_path.as_deref())
                            .await
                    }
                };
                reply.send(result).ok();
            }
        }
        Ok(())
    }
}

// ─── Wasm : Rust → wasm32-wasip1 → wasmtime+WASI ────────────────────────────

impl SandboxActor {
    /// Enveloppe le snippet généré par le LLM dans un binaire Rust autonome.
    fn build_rust_source(code: &str) -> String {
        if code.contains("fn main") {
            code.to_owned()
        } else {
            format!("fn main() {{\n{code}\n}}")
        }
    }

    async fn run_wasm_rust(&self, code: &str, repo_path: Option<&str>) -> Result<ExecResult> {
        let t0 = std::time::Instant::now();
        tracing::debug!(
            "wasm sandbox : rust ({} octets), repo={repo_path:?}",
            code.len()
        );

        // 1. Dossier de travail éphémère (source + artefact wasm).
        let dir = std::env::temp_dir().join(format!("echo-wasm-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("création du dossier temporaire {}", dir.display()))?;
        let src = dir.join("repro.rs");
        let bin = dir.join("repro.wasm");
        let source = Self::build_rust_source(code);
        tokio::fs::write(&src, source).await?;

        // 2. Compilation Rust → wasm32-wasip1 (bloquant → spawn_blocking).
        let src_c = src.clone();
        let bin_c = bin.clone();
        let timeout_ms = self.wasm_cfg.timeout_ms;
        let compile =
            tokio::task::spawn_blocking(move || compile_to_wasm(&src_c, &bin_c, timeout_ms));
        let compiled = match tokio::time::timeout(Duration::from_millis(timeout_ms), compile).await
        {
            Ok(joined) => joined.map_err(|e| anyhow::anyhow!("thread de compilation : {e}"))?,
            Err(_) => {
                Self::cleanup(&dir).await;
                return Ok(ExecResult {
                    stdout: String::new(),
                    stderr: format!("timeout de compilation (> {timeout_ms} ms)"),
                    exit_code: 124,
                    elapsed_ms: t0.elapsed().as_millis() as u64,
                });
            }
        };

        if !compiled.is_wasm {
            // Erreur de compilation : renvoyée comme ExecResult (et non Err)
            // pour que le self-healing voie stdout/stderr et régénère le script.
            Self::cleanup(&dir).await;
            return Ok(ExecResult {
                stdout: compiled.stdout,
                stderr: compiled.stderr,
                exit_code: compiled.exit_code,
                elapsed_ms: t0.elapsed().as_millis() as u64,
            });
        }

        // 3. Exécution wasm sous wasmtime (WASI, stdio capturé, limites).
        let wasm_bytes = match tokio::fs::read(&bin).await {
            Ok(b) => b,
            Err(e) => {
                Self::cleanup(&dir).await;
                return Ok(ExecResult {
                    stdout: String::new(),
                    stderr: format!("lecture du module wasm : {e}"),
                    exit_code: 125,
                    elapsed_ms: t0.elapsed().as_millis() as u64,
                });
            }
        };
        let repo = repo_path.map(PathBuf::from);
        let max_memory = self.wasm_cfg.max_memory as usize;
        let run = tokio::task::spawn_blocking(move || {
            run_wasmtime(&wasm_bytes, repo.as_deref(), max_memory)
        });
        let (exit_code, stdout, stderr) =
            match tokio::time::timeout(Duration::from_millis(timeout_ms), run).await {
                Ok(joined) => {
                    let out = joined.map_err(|e| anyhow::anyhow!("thread d'exécution : {e}"))?;
                    out?
                }
                Err(_) => (
                    124,
                    String::new(),
                    format!("timeout d'exécution (> {timeout_ms} ms)"),
                ),
            };

        Self::cleanup(&dir).await;
        Ok(ExecResult {
            stdout,
            stderr,
            exit_code,
            elapsed_ms: t0.elapsed().as_millis() as u64,
        })
    }

    async fn cleanup(dir: &Path) {
        let _ = tokio::fs::remove_dir_all(dir).await;
    }
}

/// Sortie de compilation (distincte d'un Err pour alimenter le self-healing).
struct CompileOutcome {
    is_wasm: bool,
    stdout: String,
    stderr: String,
    exit_code: i32,
}

/// Compile `src.rs` vers `wasm32-wasip1` avec rustc (fonction synchrone).
fn compile_to_wasm(src: &Path, bin: &Path, timeout_ms: u64) -> CompileOutcome {
    let out = std::process::Command::new("rustc")
        .arg("--edition")
        .arg("2021")
        .arg("--target")
        .arg("wasm32-wasip1")
        .arg("-O")
        .arg("-o")
        .arg(bin)
        .arg(src)
        .output();
    match out {
        Ok(o) if o.status.success() => CompileOutcome {
            is_wasm: true,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
        },
        Ok(o) => CompileOutcome {
            is_wasm: false,
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            exit_code: o.status.code().unwrap_or(1),
        },
        Err(e) => CompileOutcome {
            is_wasm: false,
            stdout: String::new(),
            stderr: format!(
                "rustc introuvable : {e}\nPour le mode wasm, installez : rustup target add wasm32-wasip1\n(timeout configuré : {timeout_ms} ms)"
            ),
            exit_code: 127,
        },
    }
}

/// État du Store wasmtime : contexte WASI preview1 + limiteur mémoire.
struct WasmState {
    p1: WasiP1Ctx,
    limiter: MemLimiter,
}

#[derive(Debug, Clone, Copy, Default)]
struct MemLimiter {
    max_bytes: usize,
}

impl wasmtime::ResourceLimiter for MemLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        Ok(desired <= self.max_bytes)
    }

    fn table_growing(
        &mut self,
        _current: u32,
        _desired: u32,
        _maximum: Option<u32>,
    ) -> anyhow::Result<bool> {
        Ok(true)
    }
}

/// Exécute un module wasm compilé (WASI preview1) et capte stdio en mémoire.
fn run_wasmtime(
    wasm_bytes: &[u8],
    repo_path: Option<&Path>,
    max_memory: usize,
) -> Result<(i32, String, String)> {
    use cap_std::ambient_authority;

    let mut engine_cfg = wasmtime::Config::new();
    engine_cfg.consume_fuel(true);
    let engine = wasmtime::Engine::new(&engine_cfg)?;
    let module = wasmtime::Module::from_binary(&engine, wasm_bytes)?;

    let mut linker: wasmtime::Linker<WasmState> = wasmtime::Linker::new(&engine);
    preview1::add_to_linker_sync(&mut linker, |t| &mut t.p1)?;

    let stdout_pipe = MemoryOutputPipe::new(WASM_STDIO_CAP);
    let stderr_pipe = MemoryOutputPipe::new(WASM_STDIO_CAP);

    let mut builder = WasiCtxBuilder::new();
    builder
        .arg("repro")
        .stdin(MemoryInputPipe::new(Vec::new()))
        .stdout(stdout_pipe.clone())
        .stderr(stderr_pipe.clone());
    if let Some(rp) = repo_path {
        let dir = cap_std::fs::Dir::open_ambient_dir(rp, ambient_authority())
            .context("préouverture du dépôt dans WASI")?;
        builder.preopened_dir(dir, DirPerms::READ, FilePerms::READ, "/workspace");
    }

    let state = WasmState {
        p1: WasiP1Ctx::new(builder.build()),
        limiter: MemLimiter {
            max_bytes: max_memory,
        },
    };
    let mut store = wasmtime::Store::new(&engine, state);
    store
        .set_fuel(WASM_FUEL)
        .context("initialisation du fuel wasmtime")?;
    store.limiter(|s| &mut s.limiter);

    let instance = linker.instantiate(&mut store, &module)?;
    let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;

    let exit_code = match start.call(&mut store, ()) {
        Ok(()) => 0,
        Err(e) => {
            if let Some(exit) = e.downcast_ref::<wasmtime_wasi::I32Exit>() {
                exit.0 // sortie contrôlée (proc_exit) : code de l'acteur
            } else {
                return Ok((1, pipe_lines(&stdout_pipe), pipe_lines(&stderr_pipe)));
            }
        }
    };

    Ok((
        exit_code,
        pipe_lines(&stdout_pipe),
        pipe_lines(&stderr_pipe),
    ))
}

/// Convertit le contenu d'un pipe mémoire en String (UTF-8 lossy), avec borne.
fn pipe_lines(pipe: &MemoryOutputPipe) -> String {
    let bytes = pipe.contents();
    let mut s = String::from_utf8_lossy(&bytes).into_owned();
    if bytes.len() >= WASM_STDIO_CAP {
        s.push_str(" … [tronqué]");
    }
    s
}

// ─── Docker ─────────────────────────────────────────────────────────────────

/// Analyse "512m"/"1g"/"256k"/"64" → octets (helper pur, testé).
fn parse_memory_limit(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let num: u64 = num.parse().ok()?;
    match unit {
        "" => Some(num),
        "k" | "kb" => Some(num * 1024),
        "m" | "mb" => Some(num * 1024 * 1024),
        "g" | "gb" => Some(num * 1024 * 1024 * 1024),
        _ => None,
    }
}

impl SandboxActor {
    async fn run_docker(
        &self,
        code: &str,
        lang: &str,
        repo_path: Option<&str>,
    ) -> Result<ExecResult> {
        let t0 = std::time::Instant::now();
        tracing::debug!(
            "Docker sandbox: {} ({} bytes), repo={repo_path:?}",
            lang,
            code.len()
        );

        let docker = Docker::connect_with_local_defaults()
            .context("connexion Docker indisponible (mode sandbox « docker », ou bascule depuis « wasm » pour ce langage)")?;

        let (image, cmd): (&str, Vec<String>) = match lang.to_lowercase().as_str() {
            "python" => (
                &self.docker_cfg.sandbox_image,
                vec!["python".into(), "-c".into(), code.into()],
            ),
            "javascript" => (
                "node:20-slim",
                vec!["node".into(), "-e".into(), code.into()],
            ),
            "bash" | "shell" => ("alpine:3", vec!["sh".into(), "-c".into(), code.into()]),
            other => anyhow::bail!("langage non supporté : {other}"),
        };

        // Dépôt cloné en lecture seule sur /workspace + scratch en écriture.
        let scratch = std::env::temp_dir().join(format!("echo-docker-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&scratch).await?;
        let mut binds = vec![format!("{}:/scratch:rw", scratch.display())];
        if let Some(rp) = repo_path {
            binds.push(format!("{}:/workspace:ro", rp));
        }

        let name = format!("echo-{}", uuid::Uuid::new_v4());

        let cfg = Config {
            image: Some(image.to_owned()),
            cmd: Some(cmd),
            network_disabled: Some(true),
            working_dir: Some("/scratch".into()),
            host_config: Some(HostConfig {
                memory: Some(
                    parse_memory_limit(&self.docker_cfg.memory_limit).unwrap_or(512 * 1024 * 1024)
                        as i64,
                ),
                nano_cpus: Some((self.docker_cfg.cpu_limit * 1_000_000_000.0) as i64),
                binds: Some(binds),
                ..Default::default()
            }),
            ..Default::default()
        };

        let opts = CreateContainerOptions {
            name: &name,
            platform: None,
        };

        // Pull paresseux si l'image manque (sinon create_container échoue).
        if docker.inspect_image(image).await.is_err() {
            tracing::info!("image Docker absente, pull : {image}");
            use futures::StreamExt;
            let mut pull = docker.create_image(
                Some(CreateImageOptions {
                    from_image: image.to_owned(),
                    ..Default::default()
                }),
                None,
                None,
            );
            while let Some(progress) = pull.next().await {
                progress.ok(); // on ignore les erreurs individuelles de layer
            }
        }

        let ctr = docker.create_container(Some(opts), cfg).await?;
        let started = docker
            .start_container::<String>(&ctr.id, None)
            .await
            .is_ok();

        // Attente de la fin avec timeout.
        let mut exit_code: i64 = 1;
        let mut timed_out = false;
        if started {
            use futures::StreamExt;
            let mut wait =
                docker.wait_container::<String>(&ctr.id, None::<WaitContainerOptions<String>>);
            let wait_fut = async {
                while let Some(res) = wait.next().await {
                    if let Ok(wr) = res {
                        exit_code = wr.status_code;
                    }
                }
            };
            if tokio::time::timeout(
                Duration::from_secs(self.sandbox_cfg.timeout_seconds),
                wait_fut,
            )
            .await
            .is_err()
            {
                timed_out = true;
            }
        }

        // Capture stdout/stderr (les logs survivent tant que le conteneur existe).
        let mut stdout = String::new();
        let mut stderr = String::new();
        if started {
            use futures::StreamExt;
            let mut logs = docker.logs(
                &ctr.id,
                Some(LogsOptions {
                    stdout: true,
                    stderr: true,
                    tail: "all",
                    ..Default::default()
                }),
            );
            while let Some(chunk) = logs.next().await {
                match chunk {
                    Ok(LogOutput::StdOut { message }) => {
                        stdout.push_str(&String::from_utf8_lossy(&message));
                    }
                    Ok(LogOutput::StdErr { message }) => {
                        stderr.push_str(&String::from_utf8_lossy(&message));
                    }
                    _ => {}
                }
            }
        }

        // Nettoyage
        docker
            .remove_container(
                &ctr.id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .ok();
        let _ = tokio::fs::remove_dir_all(&scratch).await;

        if timed_out {
            stderr.push_str(&format!(
                "\n[sandbox] timeout d'exécution (> {} s)",
                self.sandbox_cfg.timeout_seconds
            ));
            exit_code = 124;
        }
        if !started && exit_code == 1 && stderr.is_empty() {
            stderr.push_str("\n[sandbox] démarrage impossible du conteneur");
        }

        tracing::debug!(
            "docker result : exit={} stdout={} octets stderr={} octets",
            exit_code,
            stdout.len(),
            stderr.len()
        );
        Ok(ExecResult {
            stdout,
            stderr,
            exit_code: i32::try_from(exit_code).unwrap_or(1),
            elapsed_ms: t0.elapsed().as_millis() as u64,
        })
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn host_has_wasm_target() -> bool {
        std::process::Command::new("rustc")
            .arg("--target")
            .arg("wasm32-wasip1")
            .arg("--print")
            .arg("target-libdir")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    async fn execute(code: &str, lang: &str, repo: Option<&str>) -> ExecResult {
        let actor = SandboxActor::new(
            WasmConfig::default(),
            DockerConfig::default(),
            SandboxConfig::default(),
        );
        match actor.route(lang) {
            Backend::Wasm => actor.run_wasm_rust(code, repo).await.unwrap(),
            Backend::Docker => actor.run_docker(code, lang, repo).await.unwrap(),
        }
    }

    #[test]
    fn build_rust_source_wraps_free_snippet() {
        let src = SandboxActor::build_rust_source("println!(\"a\");");
        assert!(src.contains("fn main"));
        assert!(src.contains("println!(\"a\")"));
        let src2 = SandboxActor::build_rust_source("fn main() { }");
        assert_eq!(src2, "fn main() { }"); // déjà complet : tel quel
    }

    #[test]
    fn route_wasm_for_rust_docker_for_others() {
        let actor = SandboxActor::new(
            WasmConfig::default(),
            DockerConfig::default(),
            SandboxConfig::default(),
        );
        assert_eq!(actor.route("rust"), Backend::Wasm);
        assert_eq!(actor.route("rs"), Backend::Wasm);
        assert_eq!(actor.route("python"), Backend::Docker); // bascule décidée
        assert_eq!(actor.route("javascript"), Backend::Docker);

        // mode docker explicite, ou wasm désactivé → tout va en Docker.
        let cfg = SandboxConfig {
            mode: "docker".into(),
            ..Default::default()
        };
        let actor2 = SandboxActor::new(WasmConfig::default(), DockerConfig::default(), cfg);
        assert_eq!(actor2.route("rust"), Backend::Docker);

        let w = WasmConfig {
            enabled: false,
            ..Default::default()
        };
        let actor3 = SandboxActor::new(w, DockerConfig::default(), SandboxConfig::default());
        assert_eq!(actor3.route("rust"), Backend::Docker);
    }

    #[test]
    fn parse_memory_limit_units() {
        assert_eq!(parse_memory_limit("512m"), Some(512 * 1024 * 1024));
        assert_eq!(parse_memory_limit("1g"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_memory_limit("64"), Some(64));
        assert_eq!(parse_memory_limit(""), None);
        assert_eq!(parse_memory_limit("abc"), None);
    }

    #[tokio::test]
    async fn wasm_runs_rust_and_captures_stdout() {
        if !host_has_wasm_target() {
            eprintln!(
                "cible wasm32-wasip1 absente → test ignoré (rustup target add wasm32-wasip1)"
            );
            return;
        }
        let res = execute("println!(\"marqueur-echo-42\");", "rust", None).await;
        assert!(res.ok(), "stderr={}", res.stderr);
        assert!(
            res.stdout.contains("marqueur-echo-42"),
            "stdout={}",
            res.stdout
        );
    }

    #[tokio::test]
    async fn wasm_captures_exit_code_and_stderr() {
        if !host_has_wasm_target() {
            eprintln!("cible wasm32-wasip1 absente → test ignoré");
            return;
        }
        let res = execute(
            "eprintln!(\"échec attendu\"); std::process::exit(3);",
            "rust",
            None,
        )
        .await;
        assert!(!res.ok());
        assert_eq!(res.exit_code, 3);
        assert!(
            res.stderr.contains("échec attendu"),
            "stderr={}",
            res.stderr
        );
    }

    #[tokio::test]
    async fn wasm_preopens_repo_readonly() {
        if !host_has_wasm_target() {
            eprintln!("cible wasm32-wasip1 absente → test ignoré");
            return;
        }
        // Mini « dépôt » temporaire préouvert sur /workspace, lu par le module wasm.
        let repo = std::env::temp_dir().join(format!("echo-repo-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("indice.txt"), "eureka").unwrap();

        let code = "println!(\"{}\", std::fs::read_to_string(\"/workspace/indice.txt\").unwrap());";
        let res = execute(code, "rust", Some(repo.to_str().unwrap())).await;
        std::fs::remove_dir_all(&repo).ok();
        assert!(res.ok(), "stderr={}", res.stderr);
        assert!(res.stdout.contains("eureka"), "stdout={}", res.stdout);
    }
}

#[cfg(test)]
mod docker_tests {
    use super::*;
    use crate::config::{DockerConfig, SandboxConfig, WasmConfig};

    /// Chemin Docker complet (pull si absent, limites, capture stdout/stderr).
    /// #[ignore] : nécessite un démon Docker (runner CI sans Docker → skip).
    #[tokio::test]
    #[ignore = "nécessite Docker"]
    async fn docker_runs_python_and_captures_output() {
        let actor = SandboxActor::new(
            WasmConfig::default(),
            DockerConfig::default(),
            SandboxConfig {
                mode: "docker".into(),
                ..Default::default()
            },
        );
        let res = actor
            .run_docker("print(\"dans-docker-42\")", "python", None)
            .await
            .expect("docker connect");
        assert!(res.ok(), "stderr={}", res.stderr);
        assert!(
            res.stdout.contains("dans-docker-42"),
            "stdout={}",
            res.stdout
        );
    }

    /// Repo cloné monté en lecture seule sur /workspace côté Docker.
    #[tokio::test]
    #[ignore = "nécessite Docker"]
    async fn docker_mounts_repo_readonly() {
        let repo = std::env::temp_dir().join(format!("echo-docker-repo-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("indice.txt"), "eureka-docker").unwrap();

        let actor = SandboxActor::new(
            WasmConfig::default(),
            DockerConfig::default(),
            SandboxConfig::default(),
        );
        let code = "open(\"/workspace/indice.txt\")";
        let res = actor
            .run_docker(code, "python", Some(repo.to_str().unwrap()))
            .await
            .expect("docker connect");
        std::fs::remove_dir_all(&repo).ok();
        assert!(res.ok(), "stderr={}", res.stderr);
        assert!(
            res.stdout.contains("eureka-docker"),
            "stdout={}",
            res.stdout
        );
    }
}
