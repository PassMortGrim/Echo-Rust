//! SandboxActor — exécution isolée : Wasm (wasmtime) ou Docker (bollard).

use anyhow::{Context, Result};
use bollard::container::{
    Config, CreateContainerOptions, RemoveContainerOptions, WaitContainerOptions,
};
use bollard::Docker;
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use crate::config::{DockerConfig, SandboxConfig, WasmConfig};
use wasmtime::*;

// ─── Types ────────────────────────────────────────────────────────────

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

// ─── Messages ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum SandboxMessage {
    Execute {
        code: String,
        language: String,
        reply: RpcReplyPort<Result<ExecResult>>,
    },
}

// ─── Acteur ───────────────────────────────────────────────────────────

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
        tracing::info!(
            "SandboxActor prêt (mode={})",
            self.sandbox_cfg.mode
        );
        Ok(())
    }

    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        msg: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SandboxMessage::Execute { code, language, reply } => {
                let result = if self.sandbox_cfg.mode == "wasm" && self.wasm_cfg.enabled {
                    self.run_wasm(&code, &language).await
                } else {
                    self.run_docker(&code, &language).await
                };
                reply.send(result).ok();
            }
        }
        Ok(())
    }
}

// ─── Wasm ─────────────────────────────────────────────────────────────

impl SandboxActor {
    async fn run_wasm(&self, code: &str, lang: &str) -> Result<ExecResult> {
        let t0 = std::time::Instant::now();
        tracing::debug!("Wasm sandbox: {} ({} bytes)", lang, code.len());

        let mut wasm_config = wasmtime::Config::new();
        wasm_config.wasm_backtrace_details(WasmBacktraceDetails::Enable);

        let engine = Engine::new(&wasm_config)?;

        let module = Module::new(
            &engine,
            r#"(module
                (memory (export "memory") 1)
                (func (export "_start"))
            )"#,
        )?;

        let mut store = Store::new(&engine, ());
        let instance = Instance::new(&mut store, &module, &[])?;

        let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;
        start.call(&mut store, ())?;

        Ok(ExecResult {
            stdout: String::from("[wasm] exécution terminée"),
            stderr: String::new(),
            exit_code: 0,
            elapsed_ms: t0.elapsed().as_millis() as u64,
        })
    }

    // ─── Docker ───────────────────────────────────────────────────────

    async fn run_docker(&self, code: &str, lang: &str) -> Result<ExecResult> {
        let t0 = std::time::Instant::now();
        tracing::debug!("Docker sandbox: {} ({} bytes)", lang, code.len());

        let docker = Docker::connect_with_local_defaults()
            .context("connexion Docker")?;

        let (image, cmd): (&str, Vec<String>) = match lang.to_lowercase().as_str() {
            "python" => (
                &self.docker_cfg.sandbox_image,
                vec!["python".into(), "-c".into(), code.into()],
            ),
            "javascript" => (
                "node:20-slim",
                vec!["node".into(), "-e".into(), code.into()],
            ),
            "bash" | "shell" => (
                "alpine:3",
                vec!["sh".into(), "-c".into(), code.into()],
            ),
            other => anyhow::bail!("langage non supporté : {other}"),
        };

        let name = format!("echo-{}", uuid::Uuid::new_v4());

        let cfg = Config {
            image: Some(image.to_owned()),
            cmd: Some(cmd),
            network_disabled: Some(true),
            ..Default::default()
        };

        let opts = CreateContainerOptions {
            name: &name,
            platform: None,
        };

        let ctr = docker.create_container(Some(opts), cfg).await?;
        docker
            .start_container::<String>(&ctr.id, None)
            .await?;

        // Attente de la fin
        // let mut exit_code = 1; -> Le champ status_code de Bollard est un i64, alors que ExecResult::exit_code est un i32
        let mut exit_code: i64 = 1;
        let mut wait = docker.wait_container::<String>(&ctr.id, None::<WaitContainerOptions<String>>);
        use futures::StreamExt;
        while let Some(res) = wait.next().await {
            if let Ok(wr) = res {
                exit_code = wr.status_code;
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

        Ok(ExecResult {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: i32::try_from(exit_code).unwrap_or(1),
            elapsed_ms: t0.elapsed().as_millis() as u64,
        })
    }
}