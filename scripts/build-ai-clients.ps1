# Builds the AI client program (deskway) for Windows into the folder the server hands it out
# from (deskway-server -clients; by default "clients" beside its state file), under the names
# internal/server/clientprograms.go lists. x64 always; ARM64 too when its Rust target is installed
# (rustup target add aarch64-pc-windows-msvc). macOS: scripts/build-ai-clients.sh.
param([string]$Out = (Join-Path $PSScriptRoot '..\deskway-data\clients'))
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
New-Item -ItemType Directory -Force $Out | Out-Null
Push-Location $root
try {
    $targets = @(@{ Triple = 'x86_64-pc-windows-msvc'; Name = 'deskway-windows-x64.exe' })
    if ((rustup target list --installed) -contains 'aarch64-pc-windows-msvc') {
        $targets += @{ Triple = 'aarch64-pc-windows-msvc'; Name = 'deskway-windows-arm64.exe' }
    } else {
        Write-Warning 'aarch64-pc-windows-msvc is not installed: no Windows ARM64 program'
    }
    foreach ($t in $targets) {
        cargo build --locked --release -p deskway --target $t.Triple
        if ($LASTEXITCODE) { throw "build for $($t.Triple) failed" }
        Copy-Item (Join-Path $root "target\$($t.Triple)\release\deskway.exe") (Join-Path $Out $t.Name) -Force
        Write-Output "$($t.Name)  $((Get-FileHash (Join-Path $Out $t.Name) -Algorithm SHA256).Hash.ToLower())"
    }
} finally {
    Pop-Location
}
