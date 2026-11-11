#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════
#  Echo-Rust — Vérification des prérequis
#  Usage : ./check_prerequisites.sh
# ═══════════════════════════════════════════════════════════

set -euo pipefail

# ─── Couleurs ANSI ──────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# ─── Compteurs ──────────────────────────────────────────────
PASS=0
WARN=0
FAIL=0

# ─── Helpers ────────────────────────────────────────────────
ok()   { echo -e "  ${GREEN}✓${NC} $1"; ((PASS++)); }
warn() { echo -e "  ${YELLOW}⚠${NC} $1"; ((WARN++)); }
fail() { echo -e "  ${RED}✗${NC} $1"; ((FAIL++)); }
info() { echo -e "  ${CYAN}ℹ${NC} $1"; }

separator() {
    echo -e "\n${BOLD}── $1 ──${NC}"
}

# ═══════════════════════════════════════════════════════════
#  1. Rust (obligatoire, ≥ 1.75)
# ═══════════════════════════════════════════════════════════
separator "Rust (obligatoire)"

if command -v rustc &>/dev/null; then
    RUST_VERSION=$(rustc --version | sed 's/rustc \([0-9.]*\).*/\1/')
    RUST_MAJOR=$(echo "$RUST_VERSION" | cut -d. -f1)
    RUST_MINOR=$(echo "$RUST_VERSION" | cut -d. -f2)

    if (( RUST_MAJOR > 1 || (RUST_MAJOR == 1 && RUST_MINOR >= 75) )); then
        ok "rustc $RUST_VERSION (≥ 1.75 requis)"
    else
        fail "rustc $RUST_VERSION — version trop ancienne (≥ 1.75 requis)"
        info "Mettre à jour : rustup update stable"
    fi
else
    fail "rustc introuvable"
    info "Installer : curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
fi

# cargo
if command -v cargo &>/dev/null; then
    CARGO_VERSION=$(cargo --version | sed 's/cargo \([0-9.]*\).*/\1/')
    ok "cargo $CARGO_VERSION"
else
    fail "cargo introuvable"
fi

# rustup (optionnel mais recommandé)
if command -v rustup &>/dev/null; then
    ok "rustup $(rustup --version 2>/dev/null | head -1 | awk '{print $2}')"
else
    info "rustup non installé (recommandé pour gérer les toolchains)"
fi

# ═══════════════════════════════════════════════════════════
#  2. Docker (optionnel — fallback : sandbox Wasm)
# ═══════════════════════════════════════════════════════════
separator "Docker (optionnel → fallback Wasm)"

if command -v docker &>/dev/null; then
    DOCKER_VERSION=$(docker --version | sed 's/Docker version \([0-9.]*\).*/\1/')
    ok "docker $DOCKER_VERSION"

    # Vérifier que le daemon tourne
    if docker info &>/dev/null 2>&1; then
        ok "Docker daemon actif"
    else
        warn "Docker installé mais le daemon ne répond pas"
        info "Démarrer : sudo systemctl start docker  (Linux)"
        info "         ou lancer Docker Desktop        (macOS/Win)"
    fi

    # Vérifier le plugin compose (utile pour Neo4j/Qdrant)
    if docker compose version &>/dev/null 2>&1; then
        COMPOSE_VERSION=$(docker compose version --short 2>/dev/null || echo "?")
        ok "docker compose $COMPOSE_VERSION"
    else
        info "docker compose non disponible (utile pour Neo4j/Qdrant)"
    fi
else
    warn "docker non installé"
    info "Fallback : le sandbox Wasmtime sera utilisé (config : sandbox.mode = \"wasm\")"
    info "Installer : https://docs.docker.com/get-docker/"
fi

# ═══════════════════════════════════════════════════════════
#  3. Neo4j (optionnel → fallback : petgraph in-memory)
# ═══════════════════════════════════════════════════════════
separator "Neo4j (optionnel → fallback petgraph in-memory)"

NEO4J_HOST="${ECHO_NEO4J__URI:-bolt://localhost:7687}"
NEO4J_PORT=$(echo "$NEO4J_HOST" | grep -oP ':\K[0-9]+$' || echo "7687")
NEO4J_ADDR=$(echo "$NEO4J_HOST" | sed 's|bolt://||' | sed 's|:[0-9]*$||')

# Méthode 1 : vérifier le port TCP
if command -v nc &>/dev/null; then
    if nc -z "$NEO4J_ADDR" "$NEO4J_PORT" &>/dev/null; then
        ok "Neo4j accessible sur $NEO4J_HOST"

        # Tenter une requête HTTP (port 7474)
        HTTP_PORT=7474
        if curl -s "http://$NEO4J_ADDR:$HTTP_PORT" &>/dev/null; then
            ok "Interface HTTP Neo4j répondue sur :$HTTP_PORT"
        fi
    else
        warn "Neo4j non accessible sur $NEO4J_HOST"
        info "Fallback : graphe in-memory via petgraph"
        info "Démarrer : docker run -d --name neo4j \\"
        info "             -p 7474:7474 -p 7687:7687 \\"
        info "             -e NEO4J_AUTH=neo4j/password neo4j:latest"
    fi
elif command -v curl &>/dev/null; then
    # Méthode 2 : vérifier via HTTP
    if curl -sf "http://$NEO4J_ADDR:7474" &>/dev/null; then
        ok "Neo4j accessible sur $NEO4J_ADDR"
    else
        warn "Neo4j non accessible sur $NEO4J_ADDR"
        info "Fallback : graphe in-memory via petgraph"
    fi
else
    info "Impossible de vérifier Neo4j (nc et curl absents)"
fi

# ═══════════════════════════════════════════════════════════
#  4. Qdrant (optionnel → fallback : SQLite-vec)
# ═══════════════════════════════════════════════════════════
separator "Qdrant (optionnel → fallback SQLite-vec)"

QDRANT_URL="${ECHO_VECTOR_DB__QDRANT_URL:-http://localhost:6333}"

if command -v curl &>/dev/null; then
    HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$QDRANT_URL/collections" 2>/dev/null || echo "000")

    if [[ "$HTTP_CODE" == "200" ]]; then
        ok "Qdrant accessible sur $QDRANT_URL"

        # Lister les collections
        COLLECTIONS=$(curl -s "$QDRANT_URL/collections" | grep -oP '"name":"[^"]*"' | head -5)
        if [[ -n "$COLLECTIONS" ]]; then
            info "Collections existantes : $COLLECTIONS"
        fi
    else
        warn "Qdrant non accessible sur $QDRANT_URL (HTTP $HTTP_CODE)"
        info "Fallback : SQLite-vec (fichier local echo_vectors.db)"
        info "Démarrer : docker run -d --name qdrant \\"
        info "             -p 6333:6333 qdrant/qdrant:latest"
    fi
else
    info "curl absent — impossible de vérifier Qdrant"
fi

# ═══════════════════════════════════════════════════════════
#  5. Dépendances système utiles
# ═══════════════════════════════════════════════════════════
separator "Outils système (recommandés)"

for tool in git grep bash pkg-config; do
    if command -v "$tool" &>/dev/null; then
        ok "$tool"
    else
        warn "$tool non trouvé"
    fi
done

# libgit2 (nécessaire pour git2 crate)
if pkg-config --exists libgit2 2>/dev/null; then
    LIBGIT2_VER=$(pkg-config --modversion libgit2)
    ok "libgit2 $LIBGIT2_VER (pour la crate git2)"
else
    info "libgit2 non détecté — git2 compilera sa version bundled"
fi

# ═══════════════════════════════════════════════════════════
#  6. Fichier de configuration
# ═══════════════════════════════════════════════════════════
separator "Configuration"

if [[ -f "config.toml" ]]; then
    ok "config.toml présent"

    # Vérifier les champs critiques
    if grep -q 'api_key\s*=\s*""' config.toml 2>/dev/null; then
        warn "api_key LLM vide dans config.toml"
    elif grep -q 'api_key' config.toml 2>/dev/null; then
        ok "api_key LLM configurée"
    fi
elif [[ -f "config.example.toml" ]]; then
    warn "config.toml absent (config.example.toml disponible)"
    info "Créer : cp config.example.toml config.toml"
else
    warn "Aucun fichier de configuration trouvé"
fi

# ═══════════════════════════════════════════════════════════
#  Résumé
# ═══════════════════════════════════════════════════════════
echo -e "\n${BOLD}══════════════════════════════════════════════════════════${NC}"
echo -e "${BOLD}  RÉSUMÉ${NC}"
echo -e "${BOLD}══════════════════════════════════════════════════════════${NC}"
echo -e "  ${GREEN}✓ OK${NC}       : $PASS"
echo -e "  ${YELLOW}⚠ Optionnel${NC}: $WARN"
echo -e "  ${RED}✗ Manquant${NC} : $FAIL"
echo ""

if (( FAIL > 0 )); then
    echo -e "  ${RED}${BOLD}✗ Prérequis obligatoires manquants.${NC}"
    echo -e "    Corrigez les éléments ${RED}✗${NC} avant de compiler."
    exit 1
elif (( WARN > 0 )); then
    echo -e "  ${YELLOW}${BOLD}⚠ Compilation possible, mais certains services${NC}"
    echo -e "  ${YELLOW}  optionnels utiliseront leur fallback.${NC}"
    echo ""
    echo -e "  ${CYAN}Fallbacks actifs :${NC}"
    command -v docker &>/dev/null || echo "    • Docker → Sandbox Wasm (wasmtime)"
    nc -z "${NEO4J_ADDR:-localhost}" "${NEO4J_PORT:-7687}" 2>/dev/null || echo "    • Neo4j  → Graphe in-memory (petgraph)"
    curl -sf "${QDRANT_URL:-http://localhost:6333}/collections" &>/dev/null || echo "    • Qdrant → SQLite-vec (echo_vectors.db)"
    echo ""
    exit 0
else
    echo -e "  ${GREEN}${BOLD}✓ Tous les prérequis sont satisfaits !${NC}"
    echo -e "    Prêt à compiler : ${BOLD}cargo build --release${NC}"
    exit 0
fi