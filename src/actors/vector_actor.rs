//! VectorActor — encodage + stockage des chunks (embeddings → backend vectoriel).
//!
//! Détient un `Embedder` (API OpenAI-compatible ou fallback hash) et un
//! `VectorStore` (SQLite-vec local ou Qdrant). Le workflow lui envoie des
//! chunks code et lui demande des recherches ; la dimension est auto-détectée
//! à la 1re réponse de l'API et persistée en métadonnées de la base.

use crate::services::embedder::Embedder;
use crate::services::vector_store::{CodeChunk, Hit, VectorBackend};
use anyhow::Result;
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use std::sync::Arc;

// ─── Types ──────────────────────────────────────────────────────────────────

/// Résultat de recherche avec l'encodage effectif (flag « dégradé » si hash).
#[derive(Debug, Clone)]
pub struct SearchOutcome {
    pub hits: Vec<Hit>,
    /// ex. "hash:384" ou "api:nomic-embed-text"
    pub encoder: String,
}

impl SearchOutcome {
    /// Recherche dégradée = encodage de test (hash), pas un encodage de prod.
    pub fn degraded(&self) -> bool {
        self.encoder.starts_with("hash:")
    }
}

#[derive(Debug)]
pub enum VectorMessage {
    /// Encode (batching intégré à l'embedder) puis upsert les chunks.
    /// Renvoie le nombre de chunks indexés et l'étiquette d'encodage.
    Ingest {
        chunks: Vec<CodeChunk>,
        reply: RpcReplyPort<Result<(usize, String)>>,
    },
    /// Encode la requête puis recherche les chunks les plus proches.
    Search {
        query: String,
        top_k: usize,
        reply: RpcReplyPort<Result<SearchOutcome>>,
    },
}

// ─── Acteur ─────────────────────────────────────────────────────────────────

pub struct VectorActor {
    embedder: Arc<dyn Embedder>,
    backend: Arc<dyn VectorBackend>,
}

impl VectorActor {
    pub fn new(embedder: Arc<dyn Embedder>, backend: Arc<dyn VectorBackend>) -> Self {
        Self { embedder, backend }
    }
}

#[async_trait::async_trait]
impl Actor for VectorActor {
    type Msg = VectorMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!("VectorActor prêt");
        Ok(())
    }

    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        msg: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            VectorMessage::Ingest { chunks, reply } => {
                let result = self.ingest(&chunks).await;
                reply.send(result).ok();
            }
            VectorMessage::Search {
                query,
                top_k,
                reply,
            } => {
                let result = self.search(&query, top_k).await;
                reply.send(result).ok();
            }
        }
        Ok(())
    }
}

impl VectorActor {
    async fn ingest(&self, chunks: &[CodeChunk]) -> Result<(usize, String)> {
        if chunks.is_empty() {
            return Ok((0, self.embedder_label().await.unwrap_or_default()));
        }
        // Aucun contenu de code journalisé : seuls les volumes le sont.
        tracing::debug!("VectorActor : encodage de {} chunk(s)", chunks.len());
        let texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
        let (vectors, encoder) = self.embedder.embed(&texts).await?;
        if vectors.len() != chunks.len() {
            anyhow::bail!(
                "encodage : {} vecteurs pour {} chunks",
                vectors.len(),
                chunks.len()
            );
        }
        for (chunk, vec) in chunks.iter().zip(vectors) {
            let mut filled = chunk.clone();
            filled.embedding = vec;
            self.backend.upsert(&filled, &encoder).await?;
        }
        tracing::info!(
            "VectorActor : {} chunk(s) indexés (encoder={encoder})",
            chunks.len()
        );
        Ok((chunks.len(), encoder))
    }

    async fn search(&self, query: &str, top_k: usize) -> Result<SearchOutcome> {
        let (vectors, encoder) = self.embedder.embed(&[query.to_owned()]).await?;
        let hits = self.backend.search(&vectors[0], top_k).await?;
        tracing::debug!(
            "VectorActor : recherche → {} hit(s) (encoder={encoder})",
            hits.len()
        );
        Ok(SearchOutcome { hits, encoder })
    }

    async fn embedder_label(&self) -> Result<String> {
        self.backend.encoder().await
    }
}
