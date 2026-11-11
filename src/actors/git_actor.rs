//! GitActor — clone, checkout et lit les fichiers du dépôt cible.

use anyhow::{Context, Result};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use std::path::PathBuf;
use uuid::Uuid;

// ─── Messages ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum GitMessage {
    /// Clone un dépôt et retourne le chemin local.
    Clone {
        url: String,
        reply: RpcReplyPort<Result<String>>,
    },
    /// Checkout un commit précis.
    Checkout {
        repo_path: String,
        commit: String,
        reply: RpcReplyPort<Result<()>>,
    },
    /// Liste les fichiers d'un répertoire.
    ListFiles {
        repo_path: String,
        reply: RpcReplyPort<Result<Vec<String>>>,
    },
}

// ─── Acteur ───────────────────────────────────────────────────────────

pub struct GitActor {
    workspace: PathBuf,
}

impl GitActor {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

#[async_trait::async_trait]
impl Actor for GitActor {
    type Msg = GitMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        tokio::fs::create_dir_all(&self.workspace)
            .await
            .context("création du workspace")?;
        tracing::info!("GitActor prêt (workspace={:?})", self.workspace);
        Ok(())
    }

    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        msg: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            GitMessage::Clone { url, reply } => {
                let result = self.clone_repo(&url).await;
                reply.send(result).ok();
            }
            GitMessage::Checkout { repo_path, commit, reply } => {
                let result = self.checkout(&repo_path, &commit).await;
                reply.send(result).ok();
            }
            GitMessage::ListFiles { repo_path, reply } => {
                let result = self.list_files(&repo_path).await;
                reply.send(result).ok();
            }
        }
        Ok(())
    }
}

// ─── Implémentation ───────────────────────────────────────────────────

impl GitActor {
    async fn clone_repo(&self, url: &str) -> Result<String> {
        let dest = self.workspace.join(Uuid::new_v4().to_string());
        let url = url.to_owned();
        let path = dest.clone();

        tokio::task::spawn_blocking(move || -> Result<String> {
            let repo = git2::Repository::clone(&url, &path)
                .context("git clone")?;
            tracing::info!("Cloné {:?} → {:?}", url, path);
            let _ = repo;
            Ok(path.to_string_lossy().into_owned())
        })
        .await?
    }

    async fn checkout(&self, repo_path: &str, commit: &str) -> Result<()> {
        let path = repo_path.to_owned();
        let hash = commit.to_owned();

        tokio::task::spawn_blocking(move || -> Result<()> {
            let repo = git2::Repository::open(&path)?;
            let oid = git2::Oid::from_str(&hash)?;
            let obj = repo.find_object(oid, None)?;
            repo.reset(&obj, git2::ResetType::Hard, None)?;
            tracing::info!("Checkout {} dans {}", hash, path);
            Ok(())
        })
        .await?
    }

    async fn list_files(&self, repo_path: &str) -> Result<Vec<String>> {
        let path = PathBuf::from(repo_path);
        let mut files = Vec::new();
        let mut stack = vec![path];

        while let Some(dir) = stack.pop() {
            let mut entries = tokio::fs::read_dir(&dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let p = entry.path();
                if p.is_dir() {
                    if p.file_name().map_or(false, |n| n != ".git") {
                        stack.push(p);
                    }
                } else {
                    files.push(p.to_string_lossy().into_owned());
                }
            }
        }
        Ok(files)
    }
}