# =======================================================
#  Echo-Rust - Verification des prerequis (PowerShell)
#  Usage : .\check_prerequis_echo_rust.ps1
# =======================================================

$ErrorActionPreference = "Continue"

$script:Pass = 0
$script:Warn = 0
$script:Fail = 0

function Write-Ok($msg)   { Write-Host "  [OK] $msg" -ForegroundColor Green;    $script:Pass++ }
function Write-Warn($msg) { Write-Host "  [!!] $msg" -ForegroundColor Yellow;  $script:Warn++ }
function Write-Fail($msg) { Write-Host "  [XX] $msg" -ForegroundColor Red;     $script:Fail++ }
function Write-Info($msg) { Write-Host "  [i]  $msg" -ForegroundColor Cyan }

function Write-Section($title) {
    Write-Host ""
    Write-Host "-- $title --" -ForegroundColor White -BackgroundColor DarkBlue
}

# =======================================================
#  1. Rust (obligatoire, >= 1.75)
# =======================================================
Write-Section "Rust (obligatoire)"

$rustFound = $false
try {
    $rustOutput = & rustc --version 2>&1
    if ($LASTEXITCODE -eq 0) {
        $rustFound = $true
        # Extraire la version avec regex .NET
        $match = [regex]::Match($rustOutput, 'rustc (\d+)\.(\d+)')
        if ($match.Success) {
            $major = [int]$match.Groups[1].Value
            $minor = [int]$match.Groups[2].Value
            $verStr = "$major.$minor"
            
            if ($major -gt 1 -or ($major -eq 1 -and $minor -ge 75)) {
                Write-Ok "rustc $verStr (>= 1.75 requis)"
            }
            else {
                Write-Fail "rustc $verStr - version trop ancienne (>= 1.75 requis)"
                Write-Info "Mettre a jour : rustup update stable"
            }
        }
        else {
            Write-Warn "Impossible de parser la version rustc"
        }
    }
}
catch {
    $rustFound = $false
}

if (-not $rustFound) {
    Write-Fail "rustc introuvable"
    Write-Info "Installer : https://rustup.rs"
}

# Cargo
$cargoFound = $false
try {
    $cargoOutput = & cargo --version 2>&1
    if ($LASTEXITCODE -eq 0) {
        $cargoFound = $true
        $match = [regex]::Match($cargoOutput, 'cargo (\d+\.\d+)')
        if ($match.Success) {
            Write-Ok "cargo $($match.Groups[1].Value)"
        }
        else {
            Write-Ok "cargo installe"
        }
    }
}
catch {
    $cargoFound = $false
}

if (-not $cargoFound) {
    Write-Fail "cargo introuvable"
}

# Rustup
try {
    $rustupOutput = & rustup --version 2>&1
    if ($LASTEXITCODE -eq 0) {
        $match = [regex]::Match($rustupOutput, 'rustup (\d+\.\d+)')
        if ($match.Success) {
            Write-Ok "rustup $($match.Groups[1].Value)"
        }
        else {
            Write-Ok "rustup installe"
        }
    }
}
catch {
    Write-Info "rustup non installe (recommande)"
}

# =======================================================
#  2. Docker (optionnel - fallback Wasm)
# =======================================================
Write-Section "Docker (optionnel - fallback Wasm)"

$dockerFound = $false
try {
    $dockerOutput = & docker --version 2>&1
    if ($LASTEXITCODE -eq 0) {
        $dockerFound = $true
        $match = [regex]::Match($dockerOutput, 'Docker version (\d+\.\d+)')
        if ($match.Success) {
            Write-Ok "docker $($match.Groups[1].Value)"
        }
        else {
            Write-Ok "docker installe"
        }
        
        # Verifier le daemon
        try {
            $null = & docker info 2>&1
            if ($LASTEXITCODE -eq 0) {
                Write-Ok "Docker daemon actif"
            }
            else {
                Write-Warn "Docker daemon ne repond pas"
                Write-Info "Demarrer Docker Desktop"
            }
        }
        catch {
            Write-Warn "Docker daemon ne repond pas"
        }
        
        # Docker Compose
        try {
            $composeOutput = & docker compose version 2>&1
            if ($LASTEXITCODE -eq 0) {
                $match = [regex]::Match($composeOutput, 'v(\d+\.\d+)')
                if ($match.Success) {
                    Write-Ok "docker compose v$($match.Groups[1].Value)"
                }
                else {
                    Write-Ok "docker compose installe"
                }
            }
        }
        catch {
            Write-Info "docker compose non disponible"
        }
    }
}
catch {
    $dockerFound = $false
}

if (-not $dockerFound) {
    Write-Warn "docker non installe"
    Write-Info "Fallback : sandbox Wasmtime (config: sandbox.mode = wasm)"
    Write-Info "Installer : https://docs.docker.com/get-docker/"
}

# =======================================================
#  3. Neo4j (optionnel - fallback petgraph in-memory)
# =======================================================
Write-Section "Neo4j (optionnel - fallback petgraph in-memory)"

$neo4jUri = "bolt://localhost:7687"
if ($env:ECHO_NEO4J__URI) {
    $neo4jUri = $env:ECHO_NEO4J__URI
}

# Extraire host et port
$neo4jHost = "localhost"
$neo4jPort = 7687
$uriMatch = [regex]::Match($neo4jUri, 'bolt://([^:]+):(\d+)')
if ($uriMatch.Success) {
    $neo4jHost = $uriMatch.Groups[1].Value
    $neo4jPort = [int]$uriMatch.Groups[2].Value
}

$neo4jOk = $false
try {
    $tcp = New-Object System.Net.Sockets.TcpClient
    $tcp.Connect($neo4jHost, $neo4jPort)
    $tcp.Close()
    $neo4jOk = $true
    Write-Ok "Neo4j accessible sur $neo4jUri"
    
    # Verifier interface HTTP
    try {
        $null = Invoke-WebRequest -Uri "http://${neo4jHost}:7474" -TimeoutSec 5 -ErrorAction Stop
        Write-Ok "Interface HTTP Neo4j sur :7474"
    }
    catch {
        Write-Info "Interface HTTP Neo4j non accessible"
    }
}
catch {
    Write-Warn "Neo4j non accessible sur $neo4jUri"
    Write-Info "Fallback : graphe in-memory via petgraph"
    Write-Info "Demarrer : docker run -d --name neo4j -p 7474:7474 -p 7687:7687 -e NEO4J_AUTH=neo4j/password neo4j:latest"
}

# =======================================================
#  4. Qdrant (optionnel - fallback SQLite-vec)
# =======================================================
Write-Section "Qdrant (optionnel - fallback SQLite-vec)"

$qdrantUrl = "http://localhost:6333"
if ($env:ECHO_VECTOR_DB__QDRANT_URL) {
    $qdrantUrl = $env:ECHO_VECTOR_DB__QDRANT_URL
}

$qdrantOk = $false
try {
    $response = Invoke-WebRequest -Uri "$qdrantUrl/collections" -TimeoutSec 5 -ErrorAction Stop
    if ($response.StatusCode -eq 200) {
        $qdrantOk = $true
        Write-Ok "Qdrant accessible sur $qdrantUrl"
        
        # Lister les collections
        try {
            $json = $response.Content | ConvertFrom-Json
            if ($json.result.collections) {
                $names = @()
                foreach ($col in $json.result.collections) {
                    $names += $col.name
                }
                if ($names.Count -gt 0) {
                    Write-Info "Collections : $($names -join ', ')"
                }
            }
        }
        catch {
            # Ignore
        }
    }
}
catch {
    Write-Warn "Qdrant non accessible sur $qdrantUrl"
    Write-Info "Fallback : SQLite-vec (fichier local echo_vectors.db)"
    Write-Info "Demarrer : docker run -d --name qdrant -p 6333:6333 qdrant/qdrant:latest"
}

# =======================================================
#  5. Outils systeme recommandes
# =======================================================
Write-Section "Outils systeme (recommandes)"

$tools = @('git', 'grep', 'bash')
foreach ($tool in $tools) {
    try {
        $cmd = Get-Command $tool -ErrorAction Stop
        Write-Ok "$tool - $($cmd.Source)"
    }
    catch {
        Write-Warn "$tool non trouve"
    }
}

# =======================================================
#  6. Configuration
# =======================================================
Write-Section "Configuration"

if (Test-Path "config.toml") {
    Write-Ok "config.toml present"
    
    $content = Get-Content "config.toml" -Raw
    if ($content -match 'api_key\s*=\s*""') {
        Write-Warn "api_key LLM vide dans config.toml"
    }
    elseif ($content -match 'api_key') {
        Write-Ok "api_key LLM configuree"
    }
}
elseif (Test-Path "config.example.toml") {
    Write-Warn "config.toml absent (config.example.toml disponible)"
    Write-Info "Creer : Copy-Item config.example.toml config.toml"
}
else {
    Write-Warn "Aucun fichier de configuration trouve"
}

# =======================================================
#  Resume
# =======================================================
Write-Host ""
Write-Host "======================================================" -ForegroundColor White
Write-Host "  RESUME" -ForegroundColor White
Write-Host "======================================================" -ForegroundColor White
Write-Host "  [OK] : $Pass" -ForegroundColor Green
Write-Host "  [!!] : $Warn" -ForegroundColor Yellow
Write-Host "  [XX] : $Fail" -ForegroundColor Red
Write-Host ""

if ($Fail -gt 0) {
    Write-Host "  [XX] Prequis obligatoires manquants." -ForegroundColor Red
    Write-Host "       Corrigez les elements [XX] avant de compiler." -ForegroundColor Red
    exit 1
}
elseif ($Warn -gt 0) {
    Write-Host "  [!!] Compilation possible avec fallbacks." -ForegroundColor Yellow
    Write-Host ""
    Write-Host "  Fallbacks actifs :" -ForegroundColor Cyan
    
    if (-not $dockerFound) {
        Write-Host "    - Docker -> Sandbox Wasm (wasmtime)"
    }
    if (-not $neo4jOk) {
        Write-Host "    - Neo4j  -> Graphe in-memory (petgraph)"
    }
    if (-not $qdrantOk) {
        Write-Host "    - Qdrant -> SQLite-vec (echo_vectors.db)"
    }
    
    Write-Host ""
    exit 0
}
else {
    Write-Host "  [OK] Tous les prerequis sont satisfaits !" -ForegroundColor Green
    Write-Host "       Pret a compiler : cargo build --release" -ForegroundColor Green
    exit 0
}