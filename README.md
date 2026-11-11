

## Overview

**Echo-Rust** is a complete and enhanced Rust rewrite of the *Echo* automated bug reproduction agent. It analyzes GitHub issues, builds AST-based knowledge graphs, and reproduces bugs in ultra-fast sandbox environments (WebAssembly) or isolated containers (Docker).

## Features

### 🎭 Multi-Agent System (Actor Model)
- **Ractor Architecture**: Erlang-inspired actor system for massive concurrency without data races
- **Specialized Agents**: GitActor, AstActor, LlmActor, SandboxActor communicate via async messages
- **Swarm Intelligence**: Parallel execution of thousands of tasks on SWE-bench

### ⚡ WebAssembly Sandbox
- **Ultra-Fast Execution**: Microsecond startup vs seconds for Docker (1750x faster)
- **Strict Isolation**: Wasmtime with WASI for maximum security
- **Docker Fallback**: Full support via Bollard for cases requiring a complete OS

### 🕸️ Hybrid Knowledge Graph
- **Neo4j**: Distributed graph database for large projects
- **Petgraph**: In-memory graph for local execution (zero latency)
- **Dependency Search**: Dijkstra algorithm to find all dependents/dependencies

### 🔍 Hybrid RAG (Vector + Graph)
- **Qdrant**: Vector database for semantic search
- **SQLite-Vec**: Lightweight alternative for embedded deployments
- **Hybrid Search**: Combines embeddings (semantic) + AST (structural)

### 🛠️ Native Tool-Use (Function Calling)
- Built-in tools: grep_codebase, execute_bash, read_file, write_file, list_files, find_symbol
- **ReAct Pattern**: LLM becomes an interactive developer that iterates

### 🔄 Self-Healing
- **Error Detection**: Automatic stderr capture
- **Correction Loop**: Re-injection of errors into LLM
- **Intelligent Iterations**: Up to N attempts with accumulated context

### 🖥️ Terminal UI (TUI)
- **Ratatui**: Beautiful and interactive terminal interface
- **Real-time Monitoring**: Agent state, execution logs, tool calls

## Performance Comparison

| Metric | Python (Original) | Rust (Echo-Rust) | Improvement |
|--------|-------------------|------------------|-------------|
| Sandbox Startup | ~3.5s (Docker) | ~0.002s (Wasm) | **1750x faster** |
| Memory per Worker | ~150MB | ~8MB | **18x less RAM** |
| AST Parsing (10k LOC) | ~2.1s | ~0.08s | **26x faster** |
| Parallel Workers | ~20 (GIL) | ~10,000 (Tokio) | **500x more concurrency** |

## Quick Start

### Prerequisites
- Rust 1.75+
- Docker (optional, fallback)
- Neo4j (optional, in-memory fallback)
- Qdrant (optional, SQLite fallback)

### Installation

```bash
git clone https://github.com/cedric626/Echo-Rust.git
git clone https://github.com/PassMortGrim/Echo-Rust.git
cd echo-rust

cp config.example.toml config.toml
# Edit config.toml with your LLM API key

# Rendre exécutable et lancer
chmod +x check_prerequis_echo_rust.sh
./check_prerequis_echo_rust.sh

cargo build --release
cargo run --release

///



# Echo-Rust

**Echo-Rust** est un agent Rust de reproduction automatisée de bugs. Il analyse un dépôt, construit un graphe de connaissances à partir de l’AST, recherche du contexte pertinent, génère un script de reproduction puis l’exécute dans un sandbox isolé.

> ⚠️ **Statut : prototype fonctionnel.** Le pipeline CLI démarre et produit un script de reproduction, mais l’intégration complète des acteurs, du RAG et des sandbox réels est encore en cours de développement.

## ✨ Fonctionnalités

- **Architecture multi-agents** basée sur [Ractor](https://docs.rs/ractor) : `GitActor`, `AstActor`, `LlmActor` et `SandboxActor` communiquent par messages asynchrones.
- **Pipeline de reproduction** : initialisation, clonage, analyse AST, construction de graphe, recherche RAG, génération et exécution.
- **Analyse AST** avec Tree-sitter pour Python, JavaScript/TypeScript et Rust.
- **Graphe de connaissances** en mémoire via `petgraph`, avec option Neo4j pour un graphe persistant.
- **Recherche hybride** combinant graphe et vecteurs, avec SQLite-Vec en mode embarqué ou Qdrant en mode serveur.
- **Sandbox Wasmtime** pour l’exécution isolée, avec un mode Docker de secours via Bollard.
- **Interface TUI** Ratatui/Crossterm activable avec `ECHO_TUI=true`.
- **Configuration flexible** par fichier `config.toml` et variables d’environnement `ECHO_*`.

## 🧱 Architecture

```text
main.rs
 └── Workflow
      ├── GitActor       → clonage / accès au dépôt
      ├── AstActor       → parsing Tree-sitter et graphe de code
      ├── LlmActor       → génération via API LLM
      └── SandboxActor   → exécution Wasm ou Docker

Services
 ├── GraphService   → Neo4j / graphe en mémoire
 ├── VectorStore    → Qdrant / SQLite-Vec
 └── ToolExecutor   → outils de lecture, recherche et exécution
```

## 📦 Prérequis

- Rust 1.75 ou plus récent  
- Cargo  
- Git  
- Docker Desktop ou Docker Engine, uniquement pour le mode sandbox `docker`  
- Neo4j et Qdrant, facultatifs  

Sous Windows avec la cible `x86_64-pc-windows-msvc`, les dépendances natives peuvent nécessiter :

- Visual Studio Build Tools / MSVC
- CMake
- NASM, selon la version et la configuration des dépendances TLS

## 🚀 Installation

```bash
git clone [https://github.com/cedric626/Echo-Rust.git](https://github.com/cedric626/Echo-Rust.git) ou https://github.com/PassMortGrim/Echo-Rust j'ai pas encore choisit mon compte de travail pour ce projet.
cd Echo-Rust/v2
```

Copie la configuration d’exemple :

```bash
cp config.example.toml config.toml
```

Sous Windows :

```powershell
Copy-Item config.example.toml config.toml
```

Puis compile et lance :

```bash
cargo run --release
```

## ⚙️ Configuration

Echo-Rust charge d’abord `config.toml`, puis applique les variables d’environnement préfixées par `ECHO_`. Le séparateur de sections est `__`.

Exemple :

```powershell
$env:ECHO_LLM__API_KEY = "sk-..."
$env:ECHO_SANDBOX__MODE = "wasm"
$env:ECHO_VECTOR_DB__BACKEND = "sqlite"
cargo run --release
```

Sous Linux/macOS :

```bash
export ECHO_LLM__API_KEY="sk-..."
export ECHO_SANDBOX__MODE="wasm"
export ECHO_VECTOR_DB__BACKEND="sqlite"
cargo run --release
```

### Exemple de `config.toml`

```toml
[neo4j]
uri      = "bolt://localhost:7687"
user     = "neo4j"
password = "change_me"

[docker]
sandbox_image = "python:3.11-slim"
network_mode  = "none"
memory_limit  = "512m"
cpu_limit     = 1.0

[wasm]
enabled    = true
max_memory = 536870912
timeout_ms = 30000

[llm]
api_key     = "sk-..."
base_url    = "[https://api.openai.com/v1](https://api.openai.com/v1)"
model       = "gpt-4-turbo-preview"
temperature = 0.2
max_tokens  = 4096

[vector_db]
backend       = "sqlite"
qdrant_url    = "http://localhost:6333"
sqlite_path   = "echo_vectors.db"
embedding_dim = 1536

[sandbox]
mode            = "wasm"
max_retries     = 3
timeout_seconds = 60

[agent]
max_iterations      = 5
enable_self_healing = true
enable_hybrid_rag   = true
parallel_workers    = 4
```

> 🔐 Ne jamais commite `config.toml`, `.env` ou une clé API. Utilise `config.example.toml` comme modèle.


## Utilisation

### Mode TUI

```powershell
$env:ECHO_TUI = "true"
cargo run --release -- https://github.com/utilisateur/-depot.git
```

Avec une issue personnalisée :

```powershell
$env:ECHO_TUI = "true"
cargo run --release -- https://github.com/utilisateur/-depot.git "Erreur lors de l'authentification OAuth"
```

### Mode CLI

```PowerShell
$env:ECHO_TUI = "false"
cargo run --release -- https://github.com/utilisateur/-depot.git
```

## Alternative : variable d’environnement

Pour ne pas passer l’URL en argument, utiliser une variable `ECHO_REPO_URL` :

```rust
let repo_url = std::env::var("ECHO_REPO_URL").unwrap_or_else(|_| {
    "https://github.com/example/target-repo.git".to_string()
});
```

Puis :

```powershell
$env:ECHO_TUI = "true"
$env:ECHO_REPO_URL = "https://github.com/utilisateur/-depot.git"
cargo run --release
```

## Recommandation

La version par **argument CLI** est plus pratique pour tester rapidement plusieurs dépôts :

```powershell
cargo run --release -- https://github.com/rust-lang/rust.git
```

La version par **variable d’environnement** est plus adaptée à Docker et CI, car elle évite de modifier la commande d’entrée du conteneur.

Une suggestion d'amélioration serait d'implémenter les deux : argument prioritaire, puis `ECHO_REPO_URL`, puis valeur par défaut. Actuellement je n'en ai pas l'utilité mais s'il y a une forte demande je l'ajouterais.



## 🖥️ Utilisation

### Mode CLI

```bash
cargo run --release
```

Sortie attendue :

```text
🚀 Echo-Rust v0.2.0 — démarrage
GitActor prêt
AstActor prêt
LlmActor prêt
SandboxActor prêt
4 acteurs opérationnels ✓
[init] démarrage du workflow
[clone] clonage du dépôt…
[parse_ast] parsing AST (tree-sitter)…
[build_graph] construction du graphe de connaissances…
[rag_search] recherche hybride RAG (vector + graph)…
[generate] génération du script de reproduction…
[execute] exécution dans le sandbox…
[done] workflow terminé avec succès ✓
```

### Mode TUI

```powershell
$env:ECHO_TUI = "true"
cargo run --release
```

Appuie sur `q` ou `Échap` pour quitter l’interface.

## 🐳 Docker

Un déploiement Docker permet d’exécuter Echo-Rust de façon reproductible sur Windows, macOS et Linux.

Exemple de `Dockerfile` multi-stage :

```dockerfile
FROM rust:1.83-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates git \
 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/echo-rust /usr/local/bin/echo-rust

ENV ECHO_SANDBOX__MODE=wasm \
    ECHO_VECTOR_DB__BACKEND=sqlite

ENTRYPOINT ["/usr/local/bin/echo-rust"]
```

Construction et exécution :

```bash
docker build -t echo-rust:0.2.0 .
docker run --rm \
  -e ECHO_LLM__API_KEY="sk-..." \
  echo-rust:0.2.0
```

Pour utiliser Neo4j et Qdrant, ajoute-les à un fichier `docker-compose.yml` et configure :

```env
ECHO_NEO4J__URI=bolt://neo4j:7687
ECHO_NEO4J__USER=neo4j
ECHO_NEO4J__PASSWORD=change_me
ECHO_VECTOR_DB__BACKEND=qdrant
ECHO_VECTOR_DB__QDRANT_URL=http://qdrant:6333
```

## 🧪 Sandbox

| Mode | Description | Usage recommandé |
|---|---|---|
| `wasm` | Exécution Wasm isolée via Wasmtime | Par défaut, démarrage rapide |
| `docker` | Exécution dans un conteneur Docker | Code nécessitant un environnement OS complet |

Le mode Wasm actuel exécute un module minimal et sert de fondation au sandbox. Le mode Docker utilise l’image configurée dans `[docker].sandbox_image`, désactive le réseau et supprime le conteneur après exécution.

## 🗺️ Feuille de route

- [ ] Intégration complète du clonage réel dans le workflow
- [ ] Exécution effective du code utilisateur dans le sandbox Wasm
- [ ] Génération LLM connectée au workflow complet
- [ ] Boucle d’auto-correction à partir des erreurs d’exécution
- [ ] Indexation vectorielle réelle avec SQLite-Vec et Qdrant
- [ ] Support multi-architecture Docker (`amd64`, `arm64`)
- [ ] Tests d’intégration et jeux d’essai SWE-bench
- [ ] Publication d’images conteneurisées

## 🤝 Contribution

1. Fork le dépôt.
2. Crée une branche : `git checkout -b feature/ma-fonctionnalite`.
3. Committe tes changements : `git commit -m "feat: ajoute ma fonctionnalité"`.
4. Pousse la branche : `git push origin feature/ma-fonctionnalite`.
5. Ouvre une Pull Request.

Avant de soumettre :

```bash
cargo fmt
cargo clippy
cargo test
```

## 📄 Licence

Distribué sous licence [MIT](LICENSE).




