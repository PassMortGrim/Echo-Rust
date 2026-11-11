use anyhow::Result;
use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};

// ─── Racine ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub neo4j: Neo4jConfig,
    pub docker: DockerConfig,
    pub wasm: WasmConfig,
    pub llm: LlmConfig,
    pub vector_db: VectorDbConfig,
    pub sandbox: SandboxConfig,
    pub agent: AgentConfig,
}

// ─── Sous-configurations ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Neo4jConfig {
    pub uri: String,
    pub user: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerConfig {
    pub sandbox_image: String,
    pub network_mode: String,
    pub memory_limit: String,
    pub cpu_limit: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmConfig {
    pub enabled: bool,
    pub max_memory: u64,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub temperature: f32,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorDbConfig {
    pub backend: String,
    pub qdrant_url: String,
    pub sqlite_path: String,
    pub embedding_dim: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub mode: String,
    pub max_retries: u32,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub max_iterations: u32,
    pub enable_self_healing: bool,
    pub enable_hybrid_rag: bool,
    pub parallel_workers: usize,
}

// ─── Chargement ───────────────────────────────────────────────────────

impl Settings {
    /// Charge config.toml puis les variables ECHO_* (préfixe).
    pub fn load() -> Result<Self> {
        let figment = Figment::new()
            .merge(Toml::file("config.toml"))
            .merge(Env::prefixed("ECHO_").split("__"));
        Ok(figment.extract()?)
    }

    /// Version tolérante : retourne les défauts si le fichier est absent.
    pub fn load_or_default() -> Self {
        Self::load().unwrap_or_else(|e| {
            tracing::warn!("Config introuvable ({}), utilisation des défauts", e);
            Self::default()
        })
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            neo4j: Neo4jConfig {
                uri: "bolt://localhost:7687".into(),
                user: "neo4j".into(),
                password: "password".into(),
            },
            docker: DockerConfig {
                sandbox_image: "python:3.11-slim".into(),
                network_mode: "none".into(),
                memory_limit: "512m".into(),
                cpu_limit: 1.0,
            },
            wasm: WasmConfig {
                enabled: true,
                max_memory: 512 * 1024 * 1024,
                timeout_ms: 30_000,
            },
            llm: LlmConfig {
                api_key: String::new(),
                base_url: "https://api.openai.com/v1".into(),
                model: "gpt-4-turbo-preview".into(),
                temperature: 0.2,
                max_tokens: 4096,
            },
            vector_db: VectorDbConfig {
                backend: "sqlite".into(),
                qdrant_url: "http://localhost:6333".into(),
                sqlite_path: "echo_vectors.db".into(),
                embedding_dim: 1536,
            },
            sandbox: SandboxConfig {
                mode: "wasm".into(),
                max_retries: 3,
                timeout_seconds: 60,
            },
            agent: AgentConfig {
                max_iterations: 5,
                enable_self_healing: true,
                enable_hybrid_rag: true,
                parallel_workers: 4,
            },
        }
    }
}