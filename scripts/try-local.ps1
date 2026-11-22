# Starts Deskway on this computer for a local trial: server, account, and the host's two
# processes as the signed-in user (the network side and the agent, as the services would run).
#   .\scripts\try-local.ps1             listen on this computer only
#   .\scripts\try-local.ps1 -Lan        also allow phones and tablets on the same network (plain HTTP)
#   .\scripts\try-local.ps1 -ViewOnly   the browser can watch but not control
#   .\scripts\try-local.ps1 -Stop       stop everything it started
param(
    [switch]$Lan,
    [switch]$ViewOnly,
    [switch]$Stop,
    [string]$DataDir = (Join-Path $PSScriptRoot '..\deskway-data'),
    [int]$Port = 18080
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$server = Join-Path $root 'bin\deskway-server.exe'
$hostExe = Join-Path $root 'target\release\deskway-host.exe'

function Stop-Deskway {
    Get-Process deskway-host, deskway-server -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -in @($server, $hostExe) } |
        Stop-Process -Force
}

if ($Stop) {
    Stop-Deskway
    Write-Output 'Stopped.'
    return
}

Stop-Deskway
New-Item -ItemType Directory -Force $DataDir | Out-Null
$DataDir = (Resolve-Path $DataDir).Path
$state = Join-Path $DataDir 'state.json'

Push-Location $root
go build -o $server .\cmd\deskway-server
Pop-Location
Push-Location (Join-Path $root 'host')
cargo build --release --quiet 2>$null
Pop-Location

# The admin account is made once, while the server is stopped (the admin command needs the
# state file to itself). It prints "Admin: me  Initial password: …".
$password = $null
if (-not (Test-Path $state)) {
    $created = (& $server admin me -data $state) -join ' '
    if ($created -cmatch 'Admin: \S+\s+Initial password: (\S+)') {
        $password = $Matches[1]
    } else {
        throw "Unexpected output from admin: $created"
    }
}

$listen = if ($Lan) { "0.0.0.0:$Port" } else { "127.0.0.1:$Port" }
Start-Process -FilePath $server -ArgumentList '-listen', $listen, '-data', $state `
    -RedirectStandardError (Join-Path $DataDir 'server.log') -RedirectStandardOutput (Join-Path $DataDir 'server.out') -WindowStyle Hidden
Start-Sleep -Seconds 1

$env:DESKWAY_HOME = $DataDir
# Earlier versions kept the host's settings one folder deeper.
$oldConfig = Join-Path $DataDir 'Deskway\host.json'
$config = Join-Path $DataDir 'host.json'
if ((Test-Path $oldConfig) -and -not (Test-Path $config)) {
    Move-Item $oldConfig $config
    Remove-Item (Join-Path $DataDir 'Deskway') -ErrorAction SilentlyContinue
}
if ($password) {
    $env:DESKWAY_PASSWORD = $password
    & $hostExe enroll --server "http://127.0.0.1:$Port" --user me --name $env:COMPUTERNAME
    Remove-Item Env:DESKWAY_PASSWORD
}
# The same two processes as the installed services, in development mode: they share this data
# folder and pipe names of their own, so they never meet an installed copy.
$netArgs = @('net', '--dev')
if ($ViewOnly) { $netArgs += '--view-only' }
Start-Process -FilePath $hostExe -ArgumentList $netArgs -WindowStyle Hidden `
    -RedirectStandardError (Join-Path $DataDir 'host.log') -RedirectStandardOutput (Join-Path $DataDir 'host.out')
Start-Sleep -Milliseconds 500
Start-Process -FilePath $hostExe -ArgumentList 'agent', '--dev' -WindowStyle Hidden `
    -RedirectStandardError (Join-Path $DataDir 'agent.log') -RedirectStandardOutput (Join-Path $DataDir 'agent.out')

Write-Output ''
Write-Output "Open: http://127.0.0.1:$Port"
if ($Lan) {
    Get-NetIPAddress -AddressFamily IPv4 -AddressState Preferred |
        Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254.*' } |
        ForEach-Object { Write-Output "On your network: http://$($_.IPAddress):$Port" }
}
if ($password) {
    Write-Output "Account: me   Initial password: $password   (shown only once)"
    Write-Output 'Sign in with it in the browser, choose your own password and super password, then approve this computer when the browser asks.'
} else {
    Write-Output 'Account: me   Password: the one you chose at the first sign-in'
}
Write-Output "Stop with: .\scripts\try-local.ps1 -Stop"
