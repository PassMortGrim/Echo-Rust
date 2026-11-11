//! VectorStore — recherche sémantique (Qdrant ou SQLite-vec).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    pub id: String,
    pub content: String,
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub embedding: Vec<f32>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub chunk: CodeChunk,
    pub score: f32,
}

// ─── Trait commun ─────────────────────────────────────────────────────

#[async_trait::async_trait]
pub trait VectorBackend: Send + Sync {
    async fn upsert(&self, chunk: &CodeChunk) -> Result<()>;
    async fn search(&self, vec: &[f32], top_k: usize) -> Result<Vec<Hit>>;
}

// ─── Implémentation SQLite (placeholder) ─────────────────────────────

pub struct SqliteVecBackend {
    path: String,
}

impl SqliteVecBackend {
    pub fn new(path: &str) -> Result<Self> {
        Ok(Self { path: path.into() })
    }
}

#[async_trait::async_trait]
impl VectorBackend for SqliteVecBackend {
    async fn upsert(&self, chunk: &CodeChunk) -> Result<()> {
        tracing::debug!("sqlite-vec upsert {}", chunk.id);
        // TODO : INSERT INTO vec_chunks …
        Ok(())
    }

    async fn search(&self, _vec: &[f32], top_k: usize) -> Result<Vec<Hit>> {
        tracing::debug!("sqlite-vec search top_k={top_k}");
        // TODO : SELECT … ORDER BY distance LIMIT top_k
        Ok(vec![])
    }
}

// ─── Implémentation Qdrant (placeholder) ──────────────────────────────

pub struct QdrantBackend {
    url: String,
}

impl QdrantBackend {
    pub fn new(url: &str) -> Self {
        Self { url: url.into() }
    }
}

#[async_trait::async_trait]
impl VectorBackend for QdrantBackend {
    async fn upsert(&self, chunk: &CodeChunk) -> Result<()> {
        tracing::debug!("qdrant upsert {} → {}", chunk.id, self.url);
        Ok(())
    }

    async fn search(&self, _vec: &[f32], top_k: usize) -> Result<Vec<Hit>> {
        tracing::debug!("qdrant search top_k={top_k}");
        Ok(vec![])
    }
}

// ─── Façade ───────────────────────────────────────────────────────────

pub struct VectorStore {
    backend: Box<dyn VectorBackend>,
}

impl VectorStore {
    pub fn sqlite(path: &str) -> Result<Self> {
        Ok(Self {
            backend: Box::new(SqliteVecBackend::new(path)?),
        })
    }

    pub fn qdrant(url: &str) -> Self {
        Self {
            backend: Box::new(QdrantBackend::new(url)),
        }
    }

    pub async fn upsert(&self, chunk: &CodeChunk) -> Result<()> {
        self.backend.upsert(chunk).await
    }

    pub async fn search(&self, vec: &[f32], top_k: usize) -> Result<Vec<Hit>> {
        self.backend.search(vec, top_k).await
    }
}