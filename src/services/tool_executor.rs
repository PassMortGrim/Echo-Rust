//! ToolExecutor — exécute les outils demandés par le LLM (function calling).

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;
use tokio::process::Command;

pub struct ToolExecutor {
    workspace: PathBuf,
}

impl ToolExecutor {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }

    /// Point d'entrée unique : nom d'outil + arguments JSON → sortie JSON.
    pub async fn run(&self, tool: &str, args: &Value) -> Result<Value> {
        match tool {
            "grep_codebase" => self.grep(args).await,
            "read_file"     => self.read_file(args).await,
            "write_file"    => self.write_file(args).await,
            "list_files"    => self.list_files(args).await,
            "execute_bash"  => self.bash(args).await,
            other => Ok(serde_json::json!({
                "error": format!("outil inconnu : {other}")
            })),
        }
    }

    // ── grep ──────────────────────────────────────────────────────────

    async fn grep(&self, args: &Value) -> Result<Value> {
        let pattern = args["pattern"]
            .as_str()
            .context("pattern manquant")?;

        let out = Command::new("grep")
            .args(["-r", "-n", "-E", pattern])
            .arg(&self.workspace)
            .output()
            .await
            .context("grep")?;

        Ok(serde_json::json!({
            "stdout": String::from_utf8_lossy(&out.stdout),
            "exit":   out.status.code().unwrap_or(-1),
        }))
    }

    // ── read_file ─────────────────────────────────────────────────────

    async fn read_file(&self, args: &Value) -> Result<Value> {
        let path = args["path"].as_str().context("path manquant")?;
        let full = self.workspace.join(path);
        let content = tokio::fs::read_to_string(&full).await?;

        Ok(serde_json::json!({
            "path":  path,
            "lines": content.lines().count(),
            "content": content,
        }))
    }

    // ── write_file ────────────────────────────────────────────────────

    async fn write_file(&self, args: &Value) -> Result<Value> {
        let path    = args["path"].as_str().context("path manquant")?;
        let content = args["content"].as_str().context("content manquant")?;
        let full = self.workspace.join(path);

        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        tokio::fs::write(&full, content).await?;

        Ok(serde_json::json!({
            "written": path,
            "bytes":   content.len(),
        }))
    }

    // ── list_files ────────────────────────────────────────────────────

    async fn list_files(&self, args: &Value) -> Result<Value> {
        let rel = args["path"].as_str().unwrap_or(".");
        let dir = self.workspace.join(rel);
        let mut entries = tokio::fs::read_dir(&dir).await?;
        let mut files = Vec::new();

        while let Some(e) = entries.next_entry().await? {
            let name = e.file_name().to_string_lossy().into_owned();
            let kind = if e.file_type().await?.is_dir() { "dir" } else { "file" };
            files.push(format!("{kind}:{name}"));
        }

        Ok(serde_json::json!({ "files": files }))
    }

    // ── execute_bash (sécurisé) ───────────────────────────────────────

    async fn bash(&self, args: &Value) -> Result<Value> {
        let cmd = args["command"].as_str().context("command manquant")?;

        // Liste noire minimale
        let blacklist: HashSet<&str> = [
            "rm -rf /", "mkfs", "dd if=/dev/zero", ":(){:|:&};:",
            "chmod -R 777 /", "> /dev/sda",
        ]
        .into_iter()
        .collect();

        for bad in &blacklist {
            if cmd.contains(bad) {
                return Ok(serde_json::json!({
                    "error": "commande bloquée par la politique de sécurité"
                }));
            }
        }

        let out = Command::new("bash")
            .arg("-c")
            .arg(cmd)
            .current_dir(&self.workspace)
            .output()
            .await
            .context("bash")?;

        Ok(serde_json::json!({
            "stdout": String::from_utf8_lossy(&out.stdout),
            "stderr": String::from_utf8_lossy(&out.stderr),
            "exit":   out.status.code().unwrap_or(-1),
        }))
    }
}