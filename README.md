# Echo-Rust

**Echo-Rust** is an automated bug-reproduction agent written in Rust — a from-scratch rewrite of the original *Echo* Python agent.

> ⚠️ **Status: working prototype.** The CLI pipeline starts and produces a reproduction script; full integration of actors, RAG and real sandboxes is still under development.

## Overview

Echo-Rust analyzes a GitHub repository, builds an AST-based knowledge graph, retrieves relevant context, generates a bug-reproduction script with an LLM, then executes it in an isolated sandbox (WebAssembly, with a Docker fallback).

## Features

- **Multi-agent architecture** based on [Ractor](https://docs.rs/ractor): `GitActor`, `AstActor`, `LlmActor` and `SandboxActor` communicate through async messages.
- **Reproduction pipeline**: init, clone, AST analysis, graph building, RAG search, generation, execution.
- **AST analysis** with Tree-sitter for Python, JavaScript/TypeScript and Rust.
- **Knowledge graph** in memory via `petgraph`, with optional persistent Neo4j backend.
- **Hybrid search** combining graph and vectors: SQLite-Vec (embedded) or Qdrant (server).
- **Wasmtime sandbox** for isolated execution, with a Docker fallback via Bollard.
- **TUI** (Ratatui/Crossterm) enabled with `ECHO_TUI=true`.
- **Flexible configuration** through `config.toml` and `ECHO_*` environment variables.

## Architecture

```text
main.rs
 └── Workflow
      ├── GitActor       → cloning / repository access
      ├── AstActor       → Tree-sitter parsing and code graph
      ├── LlmActor       → generation via LLM API
      └── SandboxActor   → Wasm or Docker execution

Services
 ├── GraphService   → Neo4j / in-memory graph
 ├── VectorStore    → Qdrant / SQLite-Vec
 └── ToolExecutor   → read, search and execution tools
```

## Prerequisites

- Rust 1.75+, Cargo, Git
- Docker (only for the `docker` sandbox mode)
- Neo4j and Qdrant (optional, fallbacks included)

On Windows with the `x86_64-pc-windows-msvc` target, native dependencies may require Visual Studio Build Tools / MSVC, CMake, and NASM depending on TLS configuration.

## Installation

```bash
git clone https://github.com/cedric626/Echo-Rust.git
cd Echo-Rust
cp config.example.toml config.toml
./check_prerequis_echo_rust.sh
cargo run --release
```

Windows (PowerShell):

```powershell
git clone https://github.com/cedric626/Echo-Rust.git
cd Echo-Rust
Copy-Item config.example.toml config.toml
.\check_prerequis_echo_rust.ps1
cargo run --release
```

## Configuration

Echo-Rust loads `config.toml` first, then applies `ECHO_*` environment variables (section separator: `__`). See [`config.example.toml`](config.example.toml) for a full annotated example.

```powershell
$env:ECHO_LLM__API_KEY   = "sk-..."
$env:ECHO_SANDBOX__MODE  = "wasm"
$env:ECHO_VECTOR_DB__BACKEND = "sqlite"
cargo run --release
```

> 🔐 Never commit `config.toml`, `.env` or any API key. Use `config.example.toml` as a template.

## Usage

CLI mode (repository URL as argument — handy to test several repos):

```powershell
cargo run --release -- https://github.com/user/repo.git "Error during OAuth authentication"
```

TUI mode:

```powershell
$env:ECHO_TUI = "true"
cargo run --release -- https://github.com/user/repo.git
```

Environment-variable mode (suited for Docker and CI):

```powershell
$env:ECHO_REPO_URL = "https://github.com/user/repo.git"
cargo run --release
```

## Docker

```bash
docker compose up --build
```

The provided `docker-compose.yml` runs Echo-Rust with Neo4j and Qdrant. Secrets are injected through a `.env` file (see `docker-compose.yml`); database ports are bound to `127.0.0.1` so nothing is exposed to the network.

## Sandbox

| Mode | Description | Recommended usage |
|---|---|---|
| `wasm` | Isolated execution via Wasmtime | Default, fast startup |
| `docker` | Execution inside a container | Code requiring a full OS environment |

The current Wasm mode runs a minimal module and serves as the sandbox foundation. The Docker mode uses the image configured in `[docker].sandbox_image`, disables networking and removes the container after execution.

## Roadmap

- [ ] Full integration of real cloning into the workflow
- [ ] Effective user-code execution in the Wasm sandbox
- [ ] LLM generation connected to the complete workflow
- [ ] Self-healing loop from execution errors
- [ ] Real vector indexing with SQLite-Vec and Qdrant
- [ ] Multi-architecture Docker images (`amd64`, `arm64`)
- [ ] Integration tests and SWE-bench datasets
- [ ] Published container images

## Contributing

1. Fork the repository.
2. Create a branch: `git checkout -b feature/my-feature`.
3. Commit: `git commit -m "feat: add my feature"`.
4. Push: `git push origin feature/my-feature`.
5. Open a Pull Request.

Before submitting: `cargo fmt && cargo clippy && cargo test`.

///

# Echo-Rust (Français)

**Echo-Rust** est un agent Rust de reproduction automatisée de bugs — réécriture complète de l'agent *Echo* original en Python.

> ⚠️ **Statut : prototype fonctionnel.** Le pipeline CLI démarre et produit un script de reproduction ; l'intégration complète des acteurs, du RAG et des sandbox réels est en cours.

## Aperçu

Echo-Rust analyse un dépôt GitHub, construit un graphe de connaissances à partir de l'AST, recherche le contexte pertinent, génère un script de reproduction du bug via un LLM, puis l'exécute dans un sandbox isolé (WebAssembly, avec repli Docker).

## Fonctionnalités

- **Architecture multi-agents** basée sur [Ractor](https://docs.rs/ractor) : `GitActor`, `AstActor`, `LlmActor` et `SandboxActor` communiquent par messages asynchrones.
- **Pipeline de reproduction** : initialisation, clonage, analyse AST, construction de graphe, recherche RAG, génération, exécution.
- **Analyse AST** avec Tree-sitter pour Python, JavaScript/TypeScript et Rust.
- **Graphe de connaissances** en mémoire via `petgraph`, avec option Neo4j persistant.
- **Recherche hybride** graphe + vecteurs : SQLite-Vec (embarqué) ou Qdrant (serveur).
- **Sandbox Wasmtime** pour l'exécution isolée, avec mode Docker de secours via Bollard.
- **TUI** (Ratatui/Crossterm) activable avec `ECHO_TUI=true`.
- **Configuration flexible** par `config.toml` et variables `ECHO_*`.

## Prérequis

- Rust 1.75+, Cargo, Git
- Docker (uniquement pour le mode sandbox `docker`)
- Neo4j et Qdrant (facultatifs, replis inclus)

Sous Windows avec la cible `x86_64-pc-windows-msvc`, les dépendances natives peuvent nécessiter Visual Studio Build Tools / MSVC, CMake et NASM.

## Installation

```powershell
git clone https://github.com/cedric626/Echo-Rust.git
cd Echo-Rust
Copy-Item config.example.toml config.toml
.\check_prerequis_echo_rust.ps1
cargo run --release
```

Linux/macOS :

```bash
git clone https://github.com/cedric626/Echo-Rust.git
cd Echo-Rust
cp config.example.toml config.toml
./check_prerequis_echo_rust.sh
cargo run --release
```

## Configuration

Echo-Rust charge d'abord `config.toml`, puis applique les variables `ECHO_*` (séparateur de sections : `__`). Voir [`config.example.toml`](config.example.toml) pour un exemple complet annoté.

```powershell
$env:ECHO_LLM__API_KEY = "sk-..."
$env:ECHO_SANDBOX__MODE = "wasm"
$env:ECHO_VECTOR_DB__BACKEND = "sqlite"
cargo run --release
```

> 🔐 Ne jamais commiter `config.toml`, `.env` ou une clé API. Utilisez `config.example.toml` comme modèle.

## Utilisation

Mode CLI (URL en argument — pratique pour tester plusieurs dépôts) :

```powershell
cargo run --release -- https://github.com/utilisateur/depot.git "Erreur lors de l'authentification OAuth"
```

Mode TUI :

```powershell
$env:ECHO_TUI = "true"
cargo run --release -- https://github.com/utilisateur/depot.git
```

Mode variable d'environnement (adapté à Docker et CI) :

```powershell
$env:ECHO_REPO_URL = "https://github.com/utilisateur/depot.git"
cargo run --release
```

## Docker

```bash
docker compose up --build
```

Le `docker-compose.yml` fourni lance Echo-Rust avec Neo4j et Qdrant. Les secrets passent par un fichier `.env` ; les ports des bases sont liés à `127.0.0.1` pour qu'aucun ne soit exposé au réseau.

## Sandbox

| Mode | Description | Usage recommandé |
|---|---|---|
| `wasm` | Exécution isolée via Wasmtime | Par défaut, démarrage rapide |
| `docker` | Exécution dans un conteneur | Code nécessitant un OS complet |

Le mode Wasm actuel exécute un module minimal et sert de fondation au sandbox. Le mode Docker utilise l'image de `[docker].sandbox_image`, désactive le réseau et supprime le conteneur après exécution.

## Feuille de route

- [ ] Intégration complète du clonage réel dans le workflow
- [ ] Exécution effective du code utilisateur dans le sandbox Wasm
- [ ] Génération LLM connectée au workflow complet
- [ ] Boucle d'auto-correction à partir des erreurs d'exécution
- [ ] Indexation vectorielle réelle avec SQLite-Vec et Qdrant
- [ ] Images Docker multi-architecture (`amd64`, `arm64`)
- [ ] Tests d'intégration et jeux d'essai SWE-bench
- [ ] Publication d'images conteneurisées

## Contribution

1. Forkez le dépôt.
2. Créez une branche : `git checkout -b feature/ma-fonctionnalite`.
3. Commitez : `git commit -m "feat: ajoute ma fonctionnalite"`.
4. Poussez : `git push origin feature/ma-fonctionnalite`.
5. Ouvrez une Pull Request.

Avant de soumettre : `cargo fmt && cargo clippy && cargo test`.

## Licence

Distribué sous licence [MIT](LICENSE).
