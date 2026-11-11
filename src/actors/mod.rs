//! Module actors — système multi-agents basé sur Ractor.
//!
//! Chaque acteur possède sa propre boîte aux lettres et son état.
//! Ils communiquent exclusivement par messages asynchrones.

pub mod git_actor;
pub mod ast_actor;
pub mod llm_actor;
pub mod sandbox_actor;

// Ré-exports pour simplifier les imports côté main.rs
pub use git_actor::GitActor;
pub use ast_actor::AstActor;
pub use llm_actor::LlmActor;
pub use sandbox_actor::SandboxActor;