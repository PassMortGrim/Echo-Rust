use anyhow::Result;
use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};

// ─── Racine ───────────────────────────────────────────────────────────

/// Configuration racine. `#[serde(default)]` autorise les overrides partiels
/// (TOML incomplet ou variables ECHO_* isolées) sans perdre les défauts sûrs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub neo4j: Neo4jConfig,
    pub docker: DockerConfig,
    pub wasm: WasmConfig,
    pub llm: LlmConfig,
    pub vector_db: VectorDbConfig,
    pub sandbox: SandboxConfig,
    pub agent: AgentConfig,
}

// ─── Sous-configurations (chaque Default implémente des défauts sûrs) ─

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Neo4jConfig {
    pub uri: String,
    pub user: String,
    pub password: String,
}

impl Default for Neo4jConfig {
    fn default() -> Self {
        Self {
            uri: "bolt://localhost:7687".into(),
            user: "neo4j".into(),
            password: "password".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DockerConfig {
    pub sandbox_image: String,
    pub network_mode: String,
    pub memory_limit: String,
    pub cpu_limit: f32,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            sandbox_image: "python:3.11-slim".into(),
            // Aucun réseau par défaut : sécurité d'abord.
            network_mode: "none".into(),
            memory_limit: "512m".into(),
            cpu_limit: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WasmConfig {
    pub enabled: bool,
    pub max_memory: u64,
    pub timeout_ms: u64,
}

impl Default for WasmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_memory: 512 * 1024 * 1024,
            timeout_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub temperature: f32,
    pub max_tokens: u32,
    /// Modèle d'embeddings OpenAI-compatible (ex. "nomic-embed-text" côté
    /// LM Studio). Vide → fallback déterministe « hash » (tests/CI, hors
    /// ligne, recherche explicitement marquée dégradée).
    pub embeddings_model: String,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            // Jamais de clé API en dur dans les défauts.
            // Jamais de clé API en dur ici : uniquement ECHO_LLM__API_KEY (env,
            // jamais committée dans config.toml).
            api_key: String::new(),
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4-turbo-preview".into(),
            temperature: 0.2,
            max_tokens: 4096,
            embeddings_model: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VectorDbConfig {
    /// "sqlite" (défaut, locale via sqlite-vec), "qdrant" (serveur distant)
    /// ou "off" pour désactiver l'indexation vectorielle.
    pub backend: String,
    pub qdrant_url: String,
    pub sqlite_path: String,
    /// Dimension utilisée par le fallback hash (l'API auto-détecte la
    /// sienne à la 1re réponse et la stocke en métadonnées de la base).
    pub embedding_dim: usize,
    /// Taille des lots envoyés à l'API d'embeddings (limite les allers-retours).
    pub batch_size: usize,
    /// Nombre de résultats renvoyés par la recherche vectorielle.
    pub top_k: usize,
}

impl Default for VectorDbConfig {
    fn default() -> Self {
        Self {
            backend: "sqlite".into(),
            qdrant_url: "http://localhost:6333".into(),
            sqlite_path: "echo_vectors.db".into(),
            embedding_dim: 384,
            batch_size: 32,
            top_k: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxConfig {
    pub mode: String,
    pub max_retries: u32,
    pub timeout_seconds: u64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            mode: "wasm".into(),
            max_retries: 3,
            timeout_seconds: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub max_iterations: u32,
    pub enable_self_healing: bool,
    pub enable_hybrid_rag: bool,
    pub parallel_workers: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 5,
            enable_self_healing: true,
            enable_hybrid_rag: true,
            parallel_workers: 4,
        }
    }
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
        let s: Settings = Figment::new()
            .merge(Toml::string(toml))
            .extract()
            .expect("toml valide");
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
