//! Machine à états asynchrone — orchestration du workflow de l'agent.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── État global de l'agent ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentState {
    pub issue: String,
    pub repo_url: String,
    pub repo_path: Option<String>,
    pub ast_nodes: usize,
    pub graph_edges: usize,
    pub reproduction_script: Option<String>,
    pub phase: Phase,
    pub iteration: u32,
    pub history: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Phase {
    Init,
    CloneRepo,
    ParseAst,
    BuildGraph,
    RagSearch,
    GenerateScript,
    ExecuteSandbox,
    SelfHeal,
    Done,
    Failed(String),
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Phase::Init            => write!(f, "init"),
            Phase::CloneRepo       => write!(f, "clone"),
            Phase::ParseAst        => write!(f, "parse_ast"),
            Phase::BuildGraph      => write!(f, "build_graph"),
            Phase::RagSearch       => write!(f, "rag_search"),
            Phase::GenerateScript  => write!(f, "generate"),
            Phase::ExecuteSandbox  => write!(f, "execute"),
            Phase::SelfHeal        => write!(f, "self_heal"),
            Phase::Done            => write!(f, "done"),
            Phase::Failed(e)       => write!(f, "failed({e})"),
        }
    }
}

impl AgentState {
    pub fn new(issue: String, repo_url: String) -> Self {
        Self {
            issue,
            repo_url,
            repo_path: None,
            ast_nodes: 0,
            graph_edges: 0,
            reproduction_script: None,
            phase: Phase::Init,
            iteration: 0,
            history: Vec::new(),
        }
    }

    pub fn log(&mut self, msg: &str) {
        tracing::info!("[{}] {}", self.phase, msg);
        self.history.push(format!("[{}] {}", self.phase, msg));
    }
}

// ─── Workflow ─────────────────────────────────────────────────────────

pub struct AgentWorkflow {
    max_iter: u32,
    self_heal: bool,
}

impl AgentWorkflow {
    pub fn new(max_iter: u32, self_heal: bool) -> Self {
        Self { max_iter, self_heal }
    }

    /// Boucle principale : avance l'agent de phase en phase.
    pub async fn run(&self, mut st: AgentState) -> anyhow::Result<AgentState> {
        loop {
            if st.iteration >= self.max_iter {
                st.phase = Phase::Failed("max iterations atteint".into());
                break;
            }

            match st.phase {
                Phase::Init => {
                    st.log("démarrage du workflow");
                    st.phase = Phase::CloneRepo;
                }

                Phase::CloneRepo => {
                    st.log("clonage du dépôt…");
                    // → délégué à GitActor en production
                    st.repo_path = Some("/tmp/echo-repo".into());
                    st.phase = Phase::ParseAst;
                }

                Phase::ParseAst => {
                    st.log("parsing AST (tree-sitter)…");
                    st.ast_nodes = 128; // placeholder
                    st.phase = Phase::BuildGraph;
                }

                Phase::BuildGraph => {
                    st.log("construction du graphe de connaissances…");
                    st.graph_edges = 256; // placeholder
                    st.phase = Phase::RagSearch;
                }

                Phase::RagSearch => {
                    st.log("recherche hybride RAG (vector + graph)…");
                    st.phase = Phase::GenerateScript;
                }

                Phase::GenerateScript => {
                    st.log("génération du script de reproduction…");
                    st.reproduction_script =
                        Some("import pytest\npytest.main(['-x', 'test_bug.py'])".into());
                    st.phase = Phase::ExecuteSandbox;
                }

                Phase::ExecuteSandbox => {
                    st.log("exécution dans le sandbox…");
                    let success = true; // ← résultat réel du SandboxActor

                    if success {
                        st.phase = Phase::Done;
                    } else if self.self_heal {
                        st.iteration += 1;
                        st.phase = Phase::SelfHeal;
                    } else {
                        st.phase = Phase::Failed("exécution échouée".into());
                    }
                }

                Phase::SelfHeal => {
                    st.log(&format!(
                        "auto-réparation (tentative {}/{})…",
                        st.iteration, self.max_iter
                    ));
                    st.phase = Phase::GenerateScript;
                }

                Phase::Done => {
                    st.log("workflow terminé avec succès ✓");
                    break;
                }

                Phase::Failed(ref e) => {
                    st.log(&format!("échec : {e}"));
                    break;
                }
            }
        }

        Ok(st)
    }
}