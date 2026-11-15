//! AstActor — parsing AST via tree-sitter et construction du graphe.

use anyhow::{Context, Result};
use petgraph::graph::DiGraph;
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tree_sitter::{Node, Parser};

// ─── Types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstNode {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeGraph {
    pub nodes: HashMap<String, AstNode>,
    pub edges: Vec<(String, String, String)>, // (from, to, kind)
}

impl CodeGraph {
    pub fn to_petgraph(&self) -> DiGraph<AstNode, String> {
        let mut g = DiGraph::new();
        let mut idx = HashMap::new();

        for (id, node) in &self.nodes {
            idx.insert(id.clone(), g.add_node(node.clone()));
        }
        for (from, to, kind) in &self.edges {
            if let (Some(&a), Some(&b)) = (idx.get(from), idx.get(to)) {
                g.add_edge(a, b, kind.clone());
            }
        }
        g
    }
}

// ─── Messages ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum AstMessage {
    ParseFile {
        code: String,
        language: String,
        file_path: String,
        reply: RpcReplyPort<Result<CodeGraph>>,
    },
    QueryDependencies {
        graph: CodeGraph,
        symbol: String,
        reply: RpcReplyPort<Result<Vec<String>>>,
    },
}

// ─── Acteur ───────────────────────────────────────────────────────────

pub struct AstActor;

#[async_trait::async_trait]
impl Actor for AstActor {
    type Msg = AstMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!("AstActor prêt");
        Ok(())
    }

    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        msg: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            AstMessage::ParseFile {
                code,
                language,
                file_path,
                reply,
            } => {
                let result = Self::parse(&code, &language, &file_path);
                reply.send(result).ok();
            }
            AstMessage::QueryDependencies {
                graph,
                symbol,
                reply,
            } => {
                let result = Self::find_deps(&graph, &symbol);
                reply.send(result).ok();
            }
        }
        Ok(())
    }
}

// ─── Implémentation ───────────────────────────────────────────────────

impl AstActor {
    fn parse(code: &str, lang: &str, file: &str) -> Result<CodeGraph> {
        let mut parser = Parser::new();

        match lang.to_lowercase().as_str() {
            "python" => parser.set_language(&tree_sitter_python::language())?,
            "javascript" | "typescript" => {
                parser.set_language(&tree_sitter_javascript::language())?
            }
            "rust" => parser.set_language(&tree_sitter_rust::language())?,
            other => anyhow::bail!("langage non supporté : {other}"),
        }

        let tree = parser.parse(code, None).context("tree-sitter parse")?;

        let mut graph = CodeGraph {
            nodes: HashMap::new(),
            edges: Vec::new(),
        };

        Self::walk(tree.root_node(), code, file, None, &mut graph);
        tracing::debug!(
            "AST {}: {} nœuds, {} arêtes",
            file,
            graph.nodes.len(),
            graph.edges.len()
        );
        Ok(graph)
    }

    fn walk(node: Node, src: &str, file: &str, parent: Option<String>, graph: &mut CodeGraph) {
        let kind = node.kind().to_string();

        if Self::is_significant(&kind) {
            let id = uuid::Uuid::new_v4().to_string();
            let name = Self::extract_name(node, src);

            graph.nodes.insert(
                id.clone(),
                AstNode {
                    id: id.clone(),
                    kind: kind.clone(),
                    name,
                    file: file.to_owned(),
                    start_line: node.start_position().row + 1,
                    end_line: node.end_position().row + 1,
                },
            );

            if let Some(p) = parent {
                graph.edges.push((p, id.clone(), "contains".into()));
            }

            let mut cur = node.walk();
            for child in node.children(&mut cur) {
                Self::walk(child, src, file, Some(id.clone()), graph);
            }
        } else {
            let mut cur = node.walk();
            for child in node.children(&mut cur) {
                Self::walk(child, src, file, parent.clone(), graph);
            }
        }
    }

    fn is_significant(kind: &str) -> bool {
        matches!(
            kind,
            "function_definition"
                | "class_definition"
                | "import_statement"
                | "import_from_statement"
                | "function_declaration"
                | "class_declaration"
                | "method_definition"
        )
    }

    fn extract_name(node: Node, src: &str) -> String {
        let mut cur = node.walk();
        for child in node.children(&mut cur) {
            if matches!(child.kind(), "identifier" | "name" | "type_identifier") {
                return src[child.byte_range()].to_string();
            }
        }
        "<anon>".into()
    }

    fn find_deps(graph: &CodeGraph, symbol: &str) -> Result<Vec<String>> {
        let target = graph
            .nodes
            .values()
            .find(|n| n.name == symbol)
            .context("symbole introuvable")?;

        let mut deps = Vec::new();
        for (from, to, _) in &graph.edges {
            if from == &target.id {
                if let Some(n) = graph.nodes.get(to) {
                    deps.push(format!("{}:{}", n.kind, n.name));
                }
            }
        }
        Ok(deps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PY: &str = "import os

class Greeter:
    def greet(self, name):
        return f\"hello {name}\"

def main():
    g = Greeter()
    print(g.greet(\"world\"))
";

    #[test]
    fn parses_python_functions_classes_imports() {
        let g = AstActor::parse(PY, "python", "app.py").expect("parse python");
        let kinds: Vec<&str> = g.nodes.values().map(|n| n.kind.as_str()).collect();
        assert!(kinds.contains(&"function_definition"));
        assert!(kinds.contains(&"class_definition"));
        assert!(kinds.contains(&"import_statement"));
        let names: Vec<&str> = g.nodes.values().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"greet"));
        assert!(names.contains(&"main"));
    }

    #[test]
    fn line_numbers_are_one_based() {
        let g = AstActor::parse(PY, "python", "app.py").unwrap();
        let main = g
            .nodes
            .values()
            .find(|n| n.name == "main")
            .expect("main trouvé");
        assert_eq!(main.start_line, 7);
        let greet = g
            .nodes
            .values()
            .find(|n| n.name == "greet")
            .expect("greet trouvé");
        assert_eq!(greet.start_line, 4);
    }

    #[test]
    fn contains_edges_link_class_to_method() {
        let g = AstActor::parse(PY, "python", "app.py").unwrap();
        let greet = g.nodes.values().find(|n| n.name == "greet").unwrap();
        let linked = g.edges.iter().any(|(from, to, kind)| {
            kind == "contains"
                && to == &greet.id
                && g.nodes
                    .get(from)
                    .map(|p| p.name == "Greeter")
                    .unwrap_or(false)
        });
        assert!(
            linked,
            "greet doit être rattaché à Greeter par une arête contains"
        );
    }

    #[test]
    fn unsupported_language_is_rejected() {
        assert!(AstActor::parse("int main() { return 0; }", "cobol", "x.c").is_err());
    }

    #[test]
    fn parses_rust_functions() {
        let code = "fn compute(a: u32) -> u32 { a + 1 }\n";
        let g = AstActor::parse(code, "rust", "lib.rs").expect("parse rust");
        assert!(g
            .nodes
            .values()
            .any(|n| n.kind == "function_declaration" && n.name == "compute"));
    }

    #[test]
    fn find_deps_returns_contained_symbols() {
        let g = AstActor::parse(PY, "python", "app.py").unwrap();
        let deps = AstActor::find_deps(&g, "Greeter").expect("deps de Greeter");
        assert!(deps.iter().any(|d| d.contains("greet")));
        assert!(AstActor::find_deps(&g, "symbole_inexistant").is_err());
    }

    #[test]
    fn petgraph_conversion_preserves_counts() {
        let g = AstActor::parse(PY, "python", "app.py").unwrap();
        let pg = g.to_petgraph();
        assert_eq!(pg.node_count(), g.nodes.len());
        assert_eq!(pg.edge_count(), g.edges.len());
    }
}
