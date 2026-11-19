//! Helpers partagés entre les suites de tests (integration, swebench_lite).
//! Chaque binaire de test compile ce module indépendamment.

#![allow(dead_code)]

use serde_json::json;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Crée un dépôt git local avec les fichiers donnés (1 commit initial).
pub fn init_git_repo(base: &Path, files: &[(&str, &str)]) {
    std::fs::create_dir_all(base).unwrap();
    let repo = git2::Repository::init(base).unwrap();
    for (rel, content) in files {
        let path = base.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("test", "test@echo.local").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
        .unwrap();
}

/// Réponse LLM « script final » (fence rust) — OpenAI chat.completions.
pub fn llm_script(script_body: &str, note: &str) -> String {
    json!({
        "choices": [{ "message": {
            "role": "assistant",
            "content": format!("{note}\n```rust\n{script_body}\n```")
        }}],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 5 }
    })
    .to_string()
}

/// Réponse LLM « tool-call list_files » (étape ReAct d'exploration).
pub fn llm_tool_call_list_files() -> String {
    json!({
        "choices": [{ "message": {
            "role": "assistant",
            "content": null,
            "tool_calls": [{ "id": "call-1", "type": "function",
                "function": { "name": "list_files", "arguments": "{}" } }]
        }}],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    })
    .to_string()
}

/// Faux serveur LLM OpenAI-compatible : répond successivement avec les
/// réponses fournies (comportement LLM scripté), sans réseau réel.
pub async fn spawn_fake_llm(responses: Vec<String>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let queue = Arc::new(Mutex::new(VecDeque::from(responses)));

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let queue = queue.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 65536];
                let mut acc: Vec<u8> = Vec::new();
                loop {
                    // Lit jusqu'à la fin des en-têtes HTTP.
                    let header_end = loop {
                        if let Some(pos) = acc.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos + 4;
                        }
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => acc.extend_from_slice(&buf[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&acc[..header_end]).to_ascii_lowercase();
                    let content_length: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:")?.trim().parse().ok())
                        .unwrap_or(0);
                    // Lit le corps complet de la requête.
                    while acc.len() < header_end + content_length {
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => acc.extend_from_slice(&buf[..n]),
                        }
                    }
                    acc.drain(..header_end + content_length);

                    let body = queue
                        .lock()
                        .ok()
                        .and_then(|mut q| q.pop_front())
                        .unwrap_or_else(|| {
                            r#"{"choices":[{"message":{"role":"assistant","content":""}}]}"#
                                .to_owned()
                        });
                    let http = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    if sock.write_all(http.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });

    format!("http://{addr}/v1")
}
