//! Echo-Rust — bibliothèque (exposée pour les tests d'intégration `tests/`
//! et le futur harnais SWE-bench).
//!
//! Orchestration : config → acteurs (ractor) → services → workflow.

// Prototype : des éléments publics restent en attente d'intégration (v0.3).
#![allow(dead_code)]

pub mod actors;
pub mod config;
pub mod services;
pub mod state;
pub mod tui;
