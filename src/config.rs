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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane_and_safe() {
        let s = Settings::default();
        assert_eq!(s.sandbox.mode, "wasm");
        // Le sandbox Docker ne doit jamais avoir de réseau par défaut.
        assert_eq!(s.docker.network_mode, "none");
        assert!(s.wasm.enabled);
        assert_eq!(s.wasm.max_memory, 512 * 1024 * 1024);
        // Jamais de clé API en dur dans les défauts.
        assert!(s.llm.api_key.is_empty());
        assert!(s.agent.max_iterations > 0);
        assert!(s.agent.parallel_workers > 0);
    }

    #[test]
    fn full_toml_is_parsed() {
        let toml = r#"
[neo4j]
uri = "bolt://db:7687"
user = "neo4j"
password = "test"

[docker]
sandbox_image = "alpine:3.19"
network_mode = "none"
memory_limit = "256m"
cpu_limit = 0.5

[wasm]
enabled = false
max_memory = 268435456
timeout_ms = 5000

[llm]
api_key = "test-key"
base_url = "http://localhost:1234/v1"
model = "test-model"
temperature = 0.7
max_tokens = 1024

[vector_db]
backend = "qdrant"
qdrant_url = "http://localhost:6333"
sqlite_path = "v.db"
embedding_dim = 384

[sandbox]
mode = "docker"
max_retries = 1
timeout_seconds = 10

[agent]
max_iterations = 2
enable_self_healing = false
enable_hybrid_rag = false
parallel_workers = 1
"#;
        let s: Settings = Figment::new().merge(Toml::string(toml)).extract().expect("toml valide");
        assert_eq!(s.llm.model, "test-model");
        assert_eq!(s.llm.temperature, 0.7);
        assert_eq!(s.sandbox.mode, "docker");
        assert!(!s.wasm.enabled);
        assert_eq!(s.wasm.max_memory, 268435456);
        assert_eq!(s.vector_db.backend, "qdrant");
        assert_eq!(s.vector_db.embedding_dim, 384);
        assert_eq!(s.agent.parallel_workers, 1);
        assert!(!s.agent.enable_self_healing);
        assert_eq!(s.docker.cpu_limit, 0.5);
    }

    #[test]
    fn env_overrides_win() {
        // Séquentiel : seule cette fonction touche aux variables ECHO_*.
        std::env::set_var("ECHO_LLM__MODEL", "env-model");
        std::env::set_var("ECHO_SANDBOX__MODE", "docker");
        let s = Settings::load().expect("config par env");
        std::env::remove_var("ECHO_LLM__MODEL");
        std::env::remove_var("ECHO_SANDBOX__MODE");
        assert_eq!(s.llm.model, "env-model");
        assert_eq!(s.sandbox.mode, "docker");
    }
}
