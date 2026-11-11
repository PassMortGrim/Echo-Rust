//! GraphService — Neo4j (distribué) + petgraph (in-memory).

use anyhow::{Context, Result};
use neo4rs::{query, Graph};
use petgraph::graph::DiGraph;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::config::Neo4jConfig;

// ─── Types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeNode {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub file: String,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
}

// ─── Service ──────────────────────────────────────────────────────────

pub struct GraphService {
    neo4j: Option<Graph>,
    local: DiGraph<CodeNode, CodeEdge>,
    idx_map: HashMap<String, petgraph::graph::NodeIndex>,
}

impl GraphService {
    /// Crée le service. Si `use_neo4j` est false, tout est in-memory.
    pub async fn connect(cfg: &Neo4jConfig, use_neo4j: bool) -> Result<Self> {
        let neo4j = if use_neo4j {
            match Graph::new(&cfg.uri, &cfg.user, &cfg.password).await {
                Ok(g) => {
                    tracing::info!("Neo4j connecté → {}", cfg.uri);
                    Some(g)
                }
                Err(e) => {
                    tracing::warn!("Neo4j indisponible ({}), fallback in-memory", e);
                    None
                }
            }
        } else {
            tracing::info!("Graphe in-memory (Neo4j désactivé)");
            None
        };

        Ok(Self {
            neo4j,
            local: DiGraph::new(),
            idx_map: HashMap::new(),
        })
    }

    // ── Écriture ──────────────────────────────────────────────────────

    pub async fn add_node(&mut self, node: CodeNode) -> Result<()> {
        if let Some(ref g) = self.neo4j {
            g.run(
                query(
                    "MERGE (n:CodeNode {id:$id})
                     SET n.kind=$kind, n.name=$name, n.file=$file, n.line=$line",
                )
                .param("id", node.id.clone())
                .param("kind", node.kind.clone())
                .param("name", node.name.clone())
                .param("file", node.file.clone())
                .param("line", node.line as i64),
            )
            .await?;
        }

        let idx = self.local.add_node(node.clone());
        self.idx_map.insert(node.id.clone(), idx);
        Ok(())
    }

    pub async fn add_edge(&mut self, edge: CodeEdge) -> Result<()> {
        if let Some(ref g) = self.neo4j {
            g.run(
                query(
                    "MATCH (a:CodeNode {id:$f}),(b:CodeNode {id:$t})
                     MERGE (a)-[:RELATES_TO {kind:$k}]->(b)",
                )
                .param("f", edge.from.clone())
                .param("t", edge.to.clone())
                .param("k", edge.kind.clone()),
            )
            .await?;
        }

        if let (Some(&a), Some(&b)) = (
            self.idx_map.get(&edge.from),
            self.idx_map.get(&edge.to),
        ) {
            self.local.add_edge(a, b, edge);
        }
        Ok(())
    }

    // ── Lecture ───────────────────────────────────────────────────────

    /// BFS jusqu'à `depth` niveaux de dépendances.
    pub fn dependencies(&self, node_id: &str, depth: usize) -> Vec<String> {
        let Some(&start) = self.idx_map.get(node_id) else {
            return vec![];
        };

        use petgraph::visit::Bfs;
        let mut bfs = Bfs::new(&self.local, start);
        let mut out = Vec::new();
        let mut d = 0;

        while let Some(idx) = bfs.next(&self.local) {
            if idx == start {
                continue;
            }
            out.push(self.local[idx].name.clone());
            d += 1;
            if d >= depth {
                break;
            }
        }
        out
    }

    /// Statistiques rapides.
    pub fn stats(&self) -> (usize, usize) {
        (self.local.node_count(), self.local.edge_count())
    }
}