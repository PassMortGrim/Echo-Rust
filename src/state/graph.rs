//! Machine à états asynchrone — orchestration RÉELLE du workflow de l'agent.
//!
//! Phase 1 (orchestration) : AgentWorkflow ne simule plus les phases — il
//! délègue chaque étape aux acteurs via leurs messages Ractor :
//!
//! `GitMessage::Clone` → `AstMessage::ParseFile` (graphe AST) → boucle ReAct
//! sur `LlmMessage::Chat` avec les outils de `ToolExecutor` →
//! `SandboxMessage::Execute`, puis auto-correction (self-healing) en
//! réinjectant `stdout`/`stderr` de l'exécution dans les messages LLM.

use crate::actors::ast_actor::CodeGraph;
use crate::actors::llm_actor::LlmResponse;
use crate::actors::sandbox_actor::ExecResult;
use crate::actors::{AstMessage, GitMessage, LlmMessage, SandboxMessage};
use crate::services::ToolExecutor;
use anyhow::{Context, Result};
use ractor::ActorRef;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

// ─── État global de l'agent ───────────────────────────────────────────────

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
            Phase::Init => write!(f, "init"),
            Phase::CloneRepo => write!(f, "clone"),
            Phase::ParseAst => write!(f, "parse_ast"),
            Phase::BuildGraph => write!(f, "build_graph"),
            Phase::RagSearch => write!(f, "rag_search"),
            Phase::GenerateScript => write!(f, "generate"),
            Phase::ExecuteSandbox => write!(f, "execute"),
            Phase::SelfHeal => write!(f, "self_heal"),
            Phase::Done => write!(f, "done"),
            Phase::Failed(e) => write!(f, "failed({e})"),
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

// ─── Limites de sûreté du prototype ───────────────────────────────────────

/// Nombre maximal de fichiers sources analysés par le parsing AST.
const MAX_FILES_PARSED: usize = 40;
/// Taille maximale (octets) d'un fichier source soumis à tree-sitter.
const MAX_FILE_BYTES: usize = 200_000;
/// Nombre maximal de tours [LLM → tool_calls → résultats] dans la boucle ReAct.
const MAX_TOOL_ROUNDS: usize = 8;
/// Nombre de fichiers listés dans le prompt initial.
const MAX_LISTED_FILES_IN_PROMPT: usize = 100;
/// Troncature des sorties d'exécution réinjectées au LLM (self-healing).
const MAX_FEEDBACK_CHARS: usize = 1500;

const SYSTEM_PROMPT: &str = "Tu es un agent de reproduction automatisée de bugs. \
Tu peux explorer le dépôt cloné avec les outils fournis (grep_codebase, read_file, \
write_file, list_files, execute_bash) avant de répondre. Ta réponse finale DOIT \
contenir exactement un bloc de code clôturé : ```rust de préférence (le sandbox \
WebAssembly l'exécute en isolation), sinon ```python, ```javascript ou ```bash \
(exécution Docker durcie). Le script doit être auto-suffisant, sans interaction \
utilisateur, et afficher clairement le problème en sortie. Réponds en français.";

// ─── Helpers purs (testables sans acteurs) ────────────────────────────────

/// Détecte le langage d'un fichier à partir de son extension.
fn infer_language(path: &str) -> Option<&'static str> {
    let ext = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "py" => Some("python"),
        "js" | "mjs" | "jsx" | "ts" | "tsx" => Some("javascript"),
        "rs" => Some("rust"),
        _ => None,
    }
}

/// Fichier source analysable, hors répertoires de build/VCS.
fn is_source_file(path: &str) -> bool {
    const SKIP: [&str; 4] = ["/.git/", "/target/", "/node_modules/", "/__pycache__/"];
    let normalized = path.replace('\\', "/");
    !SKIP.iter().any(|dir| normalized.contains(dir)) && infer_language(path).is_some()
}

/// Extrait le DERNIER bloc de code clôturé ```lang\n…``` d'une réponse LLM.
fn extract_code_block(content: &str) -> Option<(String, String)> {
    let mut last: Option<(String, String)> = None;
    let mut current: Option<(String, String)> = None; // (langage, corps)

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some((_, body)) = &mut current {
            if trimmed.starts_with("```") {
                last = current.take();
            } else {
                body.push_str(line);
                body.push('\n');
            }
        } else if trimmed.starts_with("```") {
            let lang = trimmed.trim_start_matches('`').trim().to_lowercase();
            current = Some((lang, String::new()));
        }
    }

    last.filter(|(_, body)| !body.trim().is_empty())
}

/// Troncature sûre (frontières de caractères) pour le feedback LLM.
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_owned();
    }
    let out: String = s.chars().take(max_chars).collect();
    format!("{out} … [tronqué]")
}

// ─── Workflow ─────────────────────────────────────────────────────────────

/// Index consolidé du dépôt cloné, produit par la phase ParseAst.
struct RepositoryIndex {
    files: Vec<String>,
    node_count: usize,
    edge_count: usize,
    symbol_summary: String,
}

pub struct AgentWorkflow {
    git: ActorRef<GitMessage>,
    ast: ActorRef<AstMessage>,
    llm: ActorRef<LlmMessage>,
    sandbox: ActorRef<SandboxMessage>,
    workspace: PathBuf,
    max_iter: u32,
    self_heal: bool,
}

impl AgentWorkflow {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        git: ActorRef<GitMessage>,
        ast: ActorRef<AstMessage>,
        llm: ActorRef<LlmMessage>,
        sandbox: ActorRef<SandboxMessage>,
        workspace: PathBuf,
        max_iter: u32,
        self_heal: bool,
    ) -> Self {
        Self {
            git,
            ast,
            llm,
            sandbox,
            workspace,
            max_iter,
            self_heal,
        }
    }

    /// Publie l'état courant vers l'éventuel observateur partagé (TUI).
    fn sync(&self, shared: &Option<Arc<Mutex<AgentState>>>, st: &AgentState) {
        if let Some(shared) = shared {
            if let Ok(mut guard) = shared.lock() {
                *guard = st.clone();
            }
        }
    }

    /// Boucle principale : avance l'agent de phase en phase, avec de vrais
    /// échanges d'acteurs à chaque étape.
    pub async fn run(
        &self,
        mut st: AgentState,
        shared: Option<Arc<Mutex<AgentState>>>,
    ) -> Result<AgentState> {
        let mut index: Option<RepositoryIndex> = None;
        let mut script_lang = String::from("python");
        let mut last_failure: Option<ExecResult> = None;

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
                    st.log("clonage réel du dépôt (GitActor)…");
                    match self.git_clone(&st.repo_url).await {
                        Ok(path) => {
                            st.repo_path = Some(path.clone());
                            st.log(&format!("dépôt cloné → {path}"));
                            st.phase = Phase::ParseAst;
                        }
                        Err(e) => {
                            st.phase = Phase::Failed(format!("clonage : {e}"));
                        }
                    }
                }

                Phase::ParseAst => {
                    st.log("parsing AST (AstActor, tree-sitter)…");
                    match self.parse_repository(&st).await {
                        Ok(idx) => {
                            st.ast_nodes = idx.node_count;
                            st.graph_edges = idx.edge_count;
                            st.log(&format!(
                                "{} fichiers analysés → {} nœuds, {} arêtes",
                                idx.files.len().min(MAX_FILES_PARSED),
                                idx.node_count,
                                idx.edge_count
                            ));
                            index = Some(idx);
                            st.phase = Phase::BuildGraph;
                        }
                        Err(e) => {
                            st.phase = Phase::Failed(format!("analyse AST : {e}"));
                        }
                    }
                }

                Phase::BuildGraph => {
                    let (n, e) = match &index {
                        Some(idx) => (idx.node_count, idx.edge_count),
                        None => (st.ast_nodes, st.graph_edges),
                    };
                    st.log(&format!("graphe de connaissances : {n} nœuds, {e} arêtes"));
                    st.phase = Phase::RagSearch;
                }

                Phase::RagSearch => {
                    // Phase 1 : contexte = symboles AST + arborescence.
                    // L'index vectoriel (SQLite-Vec / Qdrant) arrive en Phase 3.
                    st.log("contexte constitué depuis le graphe (RAG vectoriel : Phase 3)");
                    st.phase = Phase::GenerateScript;
                }

                Phase::GenerateScript => {
                    st.log("génération du script (LlmActor + tools)…");
                    let index_ref = match &index {
                        Some(idx) => idx,
                        None => {
                            st.phase = Phase::Failed("index du dépôt absent".into());
                            self.sync(&shared, &st);
                            continue;
                        }
                    };
                    match self
                        .generate_script(&st, index_ref, last_failure.as_ref())
                        .await
                    {
                        Ok((code, lang)) => {
                            st.log(&format!(
                                "script généré ({} octets, langage « {} »)",
                                code.len(),
                                lang
                            ));
                            st.reproduction_script = Some(code);
                            script_lang = lang;
                            st.phase = Phase::ExecuteSandbox;
                        }
                        Err(e) => {
                            st.phase = Phase::Failed(format!("génération : {e}"));
                        }
                    }
                }

                Phase::ExecuteSandbox => {
                    st.log("exécution dans le sandbox (SandboxActor)…");
                    let script = st.reproduction_script.clone().unwrap_or_default();
                    match self
                        .sandbox_execute(script, script_lang.clone(), st.repo_path.clone())
                        .await
                    {
                        Ok(exec) if exec.ok() => {
                            st.log(&format!(
                                "script exécuté avec succès (exit {}, {} ms)",
                                exec.exit_code, exec.elapsed_ms
                            ));
                            last_failure = None;
                            st.phase = Phase::Done;
                        }
                        Ok(exec) => {
                            last_failure = Some(exec.clone());
                            if self.self_heal {
                                st.iteration += 1;
                                st.log(&format!(
                                    "exécution échouée (exit {}) stderr={} octets → self-healing",
                                    exec.exit_code,
                                    exec.stderr.len()
                                ));
                                st.phase = Phase::SelfHeal;
                            } else {
                                st.phase = Phase::Failed(format!("exit code {}", exec.exit_code));
                            }
                        }
                        Err(e) => {
                            if self.self_heal {
                                st.iteration += 1;
                                last_failure = Some(ExecResult {
                                    stdout: String::new(),
                                    stderr: e.to_string(),
                                    exit_code: 125,
                                    elapsed_ms: 0,
                                });
                                st.log(&format!("sandbox indisponible → self-healing ({e})"));
                                st.phase = Phase::SelfHeal;
                            } else {
                                st.phase = Phase::Failed(format!("sandbox : {e}"));
                            }
                        }
                    }
                }

                Phase::SelfHeal => {
                    st.log(&format!(
                        "auto-réparation (tentative {}/{}) : réinjection des erreurs au LLM",
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

            self.sync(&shared, &st);
        }

        self.sync(&shared, &st);
        Ok(st)
    }

    // ─── Phase ParseAst : indexation AST de tout le dépôt cloné ───────────

    async fn parse_repository(&self, st: &AgentState) -> Result<RepositoryIndex> {
        let repo_path = st.repo_path.clone().context("dépôt non cloné")?;

        let files = self.git_list_files(&repo_path).await?;
        let sources: Vec<&String> = files
            .iter()
            .filter(|f| is_source_file(f))
            .take(MAX_FILES_PARSED)
            .collect();

        let mut merged = CodeGraph {
            nodes: HashMap::new(),
            edges: Vec::new(),
        };

        for file in &sources {
            let Ok(content) = tokio::fs::read_to_string(file).await else {
                continue;
            };
            if content.len() > MAX_FILE_BYTES {
                tracing::debug!("fichier ignoré (trop gros) : {file}");
                continue;
            }
            let Some(lang) = infer_language(file) else {
                continue;
            };
            let rel = Path::new(file)
                .strip_prefix(&repo_path)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| (*file).clone());

            let graph = self
                .ast_parse_file(content, lang.to_owned(), rel)
                .await
                .with_context(|| format!("parse AST de {file}"))?;

            merged.nodes.extend(graph.nodes);
            merged.edges.extend(graph.edges);
        }

        if merged.nodes.is_empty() {
            anyhow::bail!("aucun symbole détecté (py/js/ts/rs) dans {repo_path}");
        }

        // Résumé des symboles pour le contexte LLM (RagSearch de Phase 1).
        let mut symbols: Vec<String> = merged
            .nodes
            .values()
            .filter(|n| !n.name.is_empty() && n.name != "<anon>")
            .map(|n| format!("{} {} ({}:{})", n.kind, n.name, n.file, n.start_line))
            .collect();
        symbols.sort();
        symbols.dedup();

        let mut symbol_summary = String::new();
        for sym in symbols.iter().take(120) {
            symbol_summary.push_str(sym);
            symbol_summary.push('\n');
        }
        if symbols.len() > 120 {
            symbol_summary.push_str(&format!("… (+{} autres symboles)\n", symbols.len() - 120));
        }

        Ok(RepositoryIndex {
            files,
            node_count: merged.nodes.len(),
            edge_count: merged.edges.len(),
            symbol_summary,
        })
    }

    // ─── Phase GenerateScript : boucle ReAct (LLM + ToolExecutor) ─────────

    async fn generate_script(
        &self,
        st: &AgentState,
        index: &RepositoryIndex,
        feedback: Option<&ExecResult>,
    ) -> Result<(String, String)> {
        let repo_path = st.repo_path.clone().context("dépôt non cloné")?;
        let tools = ToolExecutor::new(PathBuf::from(&repo_path));

        let mut user = format!(
            "Issue à reproduire :\n{}\n\n=== Symboles détectés dans le dépôt (AST) ===\n{}\n=== Arborescence ({} premiers fichiers) ===\n{}\n\nMission : explore le dépôt avec les outils si nécessaire, puis ta réponse finale doit contenir UN SEUL bloc de code clôturé qui reproduit le bug, auto-suffisant et sans interaction utilisateur.",
            st.issue,
            index.symbol_summary,
            MAX_LISTED_FILES_IN_PROMPT,
            index
                .files
                .iter()
                .take(MAX_LISTED_FILES_IN_PROMPT)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
        );

        if let Some(fb) = feedback {
            user.push_str(&format!(
                "\n\n=== Tentative précédente (exit={}, {} ms) ===\nstdout:\n{}\nstderr:\n{}\nCorrige le script à partir de cette erreur.",
                fb.exit_code,
                fb.elapsed_ms,
                truncate(&fb.stdout, MAX_FEEDBACK_CHARS),
                truncate(&fb.stderr, MAX_FEEDBACK_CHARS),
            ));
        }

        let mut messages = vec![
            json!({ "role": "system", "content": SYSTEM_PROMPT }),
            json!({ "role": "user", "content": user }),
        ];

        for round in 0..MAX_TOOL_ROUNDS {
            let reply = self.llm_chat(messages.clone()).await?;

            // Réponse finale : extraction du bloc de code.
            if reply.tool_calls.is_empty() {
                if let Some((lang, code)) = extract_code_block(&reply.content) {
                    tracing::debug!("script extrait après {round} tour(s) d'outils");
                    let lang = if lang.is_empty() {
                        "python".into()
                    } else {
                        lang
                    };
                    return Ok((code, lang));
                }
                if !reply.content.trim().is_empty() {
                    // Pas de clôture : le contenu brut EST le script.
                    return Ok((reply.content.trim().to_owned(), "python".into()));
                }
                anyhow::bail!("le LLM a renvoyé une réponse vide");
            }

            // Boucle tool-use : sérialise l'assistant puis exécute chaque outil.
            messages.push(json!({
                "role": "assistant",
                "content": reply.content,
                "tool_calls": reply.tool_calls.iter().map(|tc| json!({
                    "id": tc.id,
                    "type": "function",
                    "function": { "name": tc.name, "arguments": tc.arguments.to_string() }
                })).collect::<Vec<Value>>(),
            }));

            for tc in &reply.tool_calls {
                tracing::info!("[tool] {} {}", tc.name, tc.arguments);
                let out = tools
                    .run(&tc.name, &tc.arguments)
                    .await
                    .unwrap_or_else(|e| json!({ "error": e.to_string() }));
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": tc.id,
                    "content": out.to_string(),
                }));
            }
        }

        anyhow::bail!("trop de tours d'outils ({MAX_TOOL_ROUNDS} appels LLM)")
    }

    // ─── Passerelles vers les acteurs (RPC typé) ──────────────────────────

    /// Envoie un message à un acteur et attend la réponse (port `reply`).
    async fn rpc_call<TMsg, T>(
        actor: &ActorRef<TMsg>,
        build: impl FnOnce(oneshot::Sender<Result<T>>) -> TMsg,
    ) -> Result<T>
    where
        TMsg: ractor::Message,
    {
        let (tx, rx) = oneshot::channel();
        actor
            .cast(build(tx))
            .map_err(|e| anyhow::anyhow!("envoi au canal de l'acteur : {e}"))?;
        let reply = rx
            .await
            .map_err(|e| anyhow::anyhow!("réponse de l'acteur perdue : {e}"))?;
        reply
    }

    async fn git_clone(&self, url: &str) -> Result<String> {
        let url = url.to_owned();
        Self::rpc_call(&self.git, move |reply| GitMessage::Clone {
            url,
            reply: reply.into(),
        })
        .await
    }

    async fn git_list_files(&self, repo_path: &str) -> Result<Vec<String>> {
        let repo_path = repo_path.to_owned();
        Self::rpc_call(&self.git, move |reply| GitMessage::ListFiles {
            repo_path,
            reply: reply.into(),
        })
        .await
    }

    async fn ast_parse_file(
        &self,
        code: String,
        language: String,
        file_path: String,
    ) -> Result<CodeGraph> {
        Self::rpc_call(&self.ast, move |reply| AstMessage::ParseFile {
            code,
            language,
            file_path,
            reply: reply.into(),
        })
        .await
    }

    async fn llm_chat(&self, messages: Vec<Value>) -> Result<LlmResponse> {
        Self::rpc_call(&self.llm, move |reply| LlmMessage::Chat {
            messages,
            reply: reply.into(),
        })
        .await
    }

    async fn sandbox_execute(
        &self,
        code: String,
        language: String,
        repo_path: Option<String>,
    ) -> Result<ExecResult> {
        Self::rpc_call(&self.sandbox, move |reply| SandboxMessage::Execute {
            code,
            language,
            repo_path,
            reply: reply.into(),
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actors::{AstActor, GitActor, LlmActor, SandboxActor};
    use std::collections::VecDeque;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // ─── Helpers purs ─────────────────────────────────────────────────────

    #[test]
    fn extracts_last_fenced_block() {
        let content =
            "Explication.\n\n```text\npseudo-code à ignorer\n```\n\nScript :\n\n```Python\nprint('hello')\n```\n";
        let (lang, code) = extract_code_block(content).expect("bloc trouvé");
        assert_eq!(lang, "python");
        assert_eq!(code, "print('hello')\n");
    }

    #[test]
    fn extracts_bare_fence_as_python_default() {
        let (_, code) = extract_code_block("```\nprint(1)\n```").expect("bloc trouvé");
        assert_eq!(code, "print(1)\n");
    }

    #[test]
    fn extracts_nothing_without_fence() {
        assert!(extract_code_block("pas de code ici").is_none());
        assert!(extract_code_block("```\n\n```").is_none());
    }

    #[test]
    fn infer_language_maps_extensions() {
        assert_eq!(infer_language("src/main.py"), Some("python"));
        assert_eq!(infer_language("lib/codec.rs"), Some("rust"));
        assert_eq!(infer_language("web/app.tsx"), Some("javascript"));
        assert_eq!(infer_language("README.md"), None);
    }

    #[test]
    fn source_filter_skips_build_dirs() {
        assert!(is_source_file("/repo/src/main.rs"));
        assert!(!is_source_file("/repo/target/release/out.rs"));
        assert!(!is_source_file("/repo/.git/config.rs"));
        assert!(!is_source_file("/repo/README.md"));
    }

    #[test]
    fn truncate_is_char_boundary_safe() {
        let s = "é".repeat(10);
        let out = truncate(&s, 3);
        // 3 chars de contenu + " … [tronqué]" (12 chars) = 15, et jamais de panique
        // même si max_chars tombait au milieu d'un caractère multi-octets.
        assert_eq!(out.chars().count(), 15);
        assert!(out.starts_with("ééé"));
        assert!(out.ends_with("[tronqué]"));
        assert_eq!(truncate(&s, 20), s); // court-circuit sous la limite
    }

    // ─── Montage des acteurs réels (hors ligne, sans réseau) ─────────────

    type ActorHandles = (
        ractor::ActorRef<GitMessage>,
        ractor::ActorRef<AstMessage>,
        ractor::ActorRef<LlmMessage>,
        ractor::ActorRef<SandboxMessage>,
    );

    async fn spawn_actors(llm_base_url: String, ws: &Path) -> ActorHandles {
        // Noms à None : pas d'enregistrement dans le registre global ractor,
        // sinon les tests exécutés en parallèle entrent en collision
        // (ActorAlreadyRegistered).
        let (git_ref, _) = ractor::Actor::spawn(None, GitActor::new(ws.to_path_buf()), ())
            .await
            .unwrap();
        let (ast_ref, _) = ractor::Actor::spawn(None, AstActor, ()).await.unwrap();
        let llm_cfg = crate::config::LlmConfig {
            base_url: llm_base_url,
            ..Default::default()
        };
        let (llm_ref, _) = ractor::Actor::spawn(None, LlmActor::new(llm_cfg), ())
            .await
            .unwrap();
        let (sandbox_ref, _) = ractor::Actor::spawn(
            None,
            SandboxActor::new(Default::default(), Default::default(), Default::default()),
            (),
        )
        .await
        .unwrap();
        (git_ref, ast_ref, llm_ref, sandbox_ref)
    }

    #[tokio::test]
    async fn zero_max_iterations_fails_immediately() {
        // L'échec est détecté avant le premier message réseau : les acteurs
        // sont montés uniquement pour prouver que rien d'autre ne démarre.
        let ws = std::env::temp_dir().join(format!("echo-it-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&ws).await.unwrap();

        let (git_ref, ast_ref, llm_ref, sandbox_ref) =
            spawn_actors("http://127.0.0.1:1/v1".into(), &ws).await;

        let workflow = AgentWorkflow::new(
            git_ref.clone(),
            ast_ref.clone(),
            llm_ref.clone(),
            sandbox_ref.clone(),
            ws.clone(),
            0,
            true,
        );
        let st = AgentState::new("bug".into(), "https://example.com/repo.git".into());
        let st = workflow.run(st, None).await.expect("workflow OK");

        assert!(matches!(st.phase, Phase::Failed(ref e) if e.contains("max iterations")));

        // Les ActorRef sont monomorphes : arrêts séparés par type de message.
        git_ref.stop(None);
        ast_ref.stop(None);
        llm_ref.stop(None);
        sandbox_ref.stop(None);
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ─── Faux serveur LLM (HTTP/1.1 minimal, OpenAI-compatible) ──────────

    async fn spawn_fake_llm(responses: Vec<String>) -> String {
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

    /// Test d'intégration HORS LIGNE : dépôt git local + faux serveur LLM +
    /// acteurs réels. Valide le câblage complet : clone → AST → ReAct
    /// (tool-call list_files) → extraction du script.
    #[tokio::test]
    async fn full_pipeline_with_fake_llm_reaches_done() {
        // 1. Dépôt git local factice.
        let base = std::env::temp_dir().join(format!("echo-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let repo = git2::Repository::init(&base).unwrap();
        std::fs::write(
            base.join("app.py"),
            "def add(a, b):\n    return a + b\nprint(add(2, 2))\n",
        )
        .unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("app.py")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("test", "test@echo.local").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        // 2. Faux serveur LLM : 1re réponse = tool-call, 2e = script final.
        let fake_url = spawn_fake_llm(vec![
            json!({
                "choices": [{ "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{ "id": "call-1", "type": "function",
                        "function": { "name": "list_files", "arguments": "{}" } }]
                }}],
                "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
            })
            .to_string(),
            json!({
                "choices": [{ "message": {
                    "role": "assistant",
                    "content": "Voici le script de reproduction :\n```rust\nprintln!(\"bug reproduit\");\n```"
                }}],
                "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 5 }
            })
            .to_string(),
        ])
        .await;

        // 3. Acteurs réels (git, ast, llm pointé sur le faux serveur,
        //    sandbox wasm réel : le script rust est compilé vers wasm32-wasip1
        //    puis exécuté sous wasmtime — pas de Docker requis en CI).
        let ws = std::env::temp_dir().join(format!("echo-e2e-ws-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&ws).await.unwrap();
        let (git_ref, ast_ref, llm_ref, sandbox_ref) = spawn_actors(fake_url, &ws).await;

        // 4. Workflow complet.
        let workflow = AgentWorkflow::new(
            git_ref.clone(),
            ast_ref.clone(),
            llm_ref.clone(),
            sandbox_ref.clone(),
            ws.clone(),
            3,
            true,
        );
        let st = AgentState::new(
            "add(2, 2) renvoie 5 au lieu de 4".into(),
            base.to_string_lossy().into_owned(),
        );

        let outcome =
            tokio::time::timeout(std::time::Duration::from_secs(60), workflow.run(st, None))
                .await
                .expect("workflow bloqué (timeout)")
                .expect("workflow OK");

        assert!(
            matches!(outcome.phase, Phase::Done),
            "phase finale : {:?}",
            outcome.phase
        );
        assert_eq!(
            outcome.reproduction_script.as_deref(),
            Some("println!(\"bug reproduit\");\n")
        );
        assert!(outcome.ast_nodes > 0, "AST réel attendu (> 0 nœuds)");
        assert!(outcome.repo_path.is_some());
        assert_eq!(outcome.iteration, 0);
        assert!(!outcome.history.is_empty());

        // Les ActorRef sont monomorphes : arrêts séparés par type de message.
        git_ref.stop(None);
        ast_ref.stop(None);
        llm_ref.stop(None);
        sandbox_ref.stop(None);
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&ws);
    }
}
