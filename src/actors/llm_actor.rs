//! LlmActor — appels HTTP vers l'API LLM + gestion du tool-use.

use anyhow::{Context, Result};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use crate::config::LlmConfig;

// ─── Types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt: u32,
    pub completion: u32,
    pub total: u32,
}

// ─── Messages ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum LlmMessage {
    Chat {
        messages: Vec<Value>,
        reply: RpcReplyPort<Result<LlmResponse>>,
    },
}

// ─── Acteur ───────────────────────────────────────────────────────────

pub struct LlmActor {
    cfg: LlmConfig,
    client: reqwest::Client,
}

impl LlmActor {
    pub fn new(cfg: LlmConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("client HTTP");
        Self { cfg, client }
    }

    /// Catalogue des outils exposés au LLM (function calling).
    fn tool_catalog() -> Vec<Value> {
        vec![
            json!({
                "type": "function",
                "function": {
                    "name": "grep_codebase",
                    "description": "Recherche regex dans le code source",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "pattern": { "type": "string" }
                        },
                        "required": ["pattern"]
                    }
                }
            }),
            json!({
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "Lit le contenu d'un fichier",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" }
                        },
                        "required": ["path"]
                    }
                }
            }),
            json!({
                "type": "function",
                "function": {
                    "name": "write_file",
                    "description": "Écrit du contenu dans un fichier",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path":    { "type": "string" },
                            "content": { "type": "string" }
                        },
                        "required": ["path", "content"]
                    }
                }
            }),
            json!({
                "type": "function",
                "function": {
                    "name": "execute_bash",
                    "description": "Exécute une commande shell dans le sandbox",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "command": { "type": "string" }
                        },
                        "required": ["command"]
                    }
                }
            }),
        ]
    }
}

#[async_trait::async_trait]
impl Actor for LlmActor {
    type Msg = LlmMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!("LlmActor prêt (model={})", self.cfg.model);
        Ok(())
    }

    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        msg: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            LlmMessage::Chat { messages, reply } => {
                let result = self.chat(messages).await;
                reply.send(result).ok();
            }
        }
        Ok(())
    }
}

// ─── Implémentation HTTP ──────────────────────────────────────────────

impl LlmActor {
    async fn chat(&self, messages: Vec<Value>) -> Result<LlmResponse> {
        let body = json!({
            "model":       self.cfg.model,
            "messages":    messages,
            "tools":       Self::tool_catalog(),
            "tool_choice": "auto",
            "temperature": self.cfg.temperature,
            "max_tokens":  self.cfg.max_tokens,
        });

        let url = format!("{}/chat/completions", self.cfg.base_url);

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.cfg.api_key)
            .json(&body)
            .send()
            .await
            .context("requête LLM")?;

        let status = resp.status();
        if !status.is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("LLM API {status}: {err}");
        }

        let v: Value = resp.json().await.context("JSON LLM")?;
        let choice = &v["choices"][0];
        let msg = &choice["message"];

        let content = msg["content"].as_str().unwrap_or("").to_string();

        let mut tool_calls = Vec::new();
        if let Some(arr) = msg["tool_calls"].as_array() {
            for tc in arr {
                tool_calls.push(ToolCall {
                    id: tc["id"].as_str().unwrap_or("").into(),
                    name: tc["function"]["name"].as_str().unwrap_or("").into(),
                    arguments: serde_json::from_str(
                        tc["function"]["arguments"].as_str().unwrap_or("{}"),
                    )?,
                });
            }
        }

        let usage = TokenUsage {
            prompt: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            completion: v["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32,
            total: v["usage"]["total_tokens"].as_u64().unwrap_or(0) as u32,
        };

        tracing::debug!("LLM → {} tokens", usage.total);
        Ok(LlmResponse { content, tool_calls, usage })
    }
}