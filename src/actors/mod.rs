//! Module actors — système multi-agents basé sur Ractor.
//!
//! Chaque acteur possède sa propre boîte aux lettres et son état.
//! Ils communiquent exclusivement par messages asynchrones.

pub mod ast_actor;
pub mod git_actor;
pub mod llm_actor;
pub mod sandbox_actor;
pub mod vector_actor;

// Ré-exports pour simplifier les imports côté main.rs et state/graph.rs
pub use ast_actor::{AstActor, AstMessage};
pub use git_actor::{GitActor, GitMessage};
pub use llm_actor::{LlmActor, LlmMessage};
pub use sandbox_actor::{SandboxActor, SandboxMessage};
pub use vector_actor::{SearchOutcome, VectorActor, VectorMessage};
