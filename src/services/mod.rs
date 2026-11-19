//! Module services — briques métier réutilisables par les acteurs.

pub mod embedder;
pub mod graph_service;
pub mod tool_executor;
pub mod vector_store;

pub use graph_service::GraphService;
pub use tool_executor::ToolExecutor;
