# Runs OrangeLink with its own APPDATA and LOCALAPPDATA under _test_sandbox\dev,
# so a development or test run never reads or writes the real config, logs,
# tools, update folder or last-opened library.
#
#   .\scripts\dev-sandbox.ps1                 # debug build, sandbox library
#   .\scripts\dev-sandbox.ps1 -Release        # release build
#   .\scripts\dev-sandbox.ps1 -- <args>       # extra arguments for OrangeLink
#
# Never start OrangeLink any other way while testing (for example
# computer use's open_application): that instance uses the real %APPDATA%.
param(
    [switch]$Release,
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$AppArgs
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$sandbox = Join-Path $root '_test_sandbox\dev'
$roaming = Join-Path $sandbox 'appdata\Roaming'
$local = Join-Path $sandbox 'appdata\Local'
$library = Join-Path $sandbox 'library'
New-Item -ItemType Directory -Force $roaming, $local | Out-Null

$buildDir = if ($Release) { 'release' } else { 'debug' }
$cargoArgs = @('build', '--locked')
if ($Release) { $cargoArgs += '--release' }
Push-Location $root
# Cargo reports progress on stderr, which Windows PowerShell turns into errors under Stop.
$ErrorActionPreference = 'Continue'
try { & cargo @cargoArgs; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE } } finally { Pop-Location }
$ErrorActionPreference = 'Stop'
$exe = Join-Path $root "target\$buildDir\orangelink.exe"

$running = Get-Process orangelink -ErrorAction SilentlyContinue
if ($running) {
    Write-Warning "OrangeLink is already running (PID $($running.Id -join ', ')). The sandbox copy gets its own config, but MCP and extension ports may clash."
}

# A first run has no config, so open (or create) the sandbox library.
if (-not $AppArgs -and -not (Test-Path (Join-Path $roaming 'OrangeLink\config.json'))) {
    $AppArgs = @($library)
}

$psi = New-Object System.Diagnostics.ProcessStartInfo $exe
# Windows PowerShell 5.1 has no ArgumentList: quote each argument ourselves.
$psi.Arguments = (@($AppArgs | Where-Object { $_ -ne '--' }) | ForEach-Object { '"' + ($_ -replace '"', '\"') + '"' }) -join ' '
$psi.UseShellExecute = $false
$psi.Environment['APPDATA'] = $roaming
$psi.Environment['LOCALAPPDATA'] = $local
$p = [System.Diagnostics.Process]::Start($psi)
Write-Host "OrangeLink (sandbox) PID $($p.Id)"
Write-Host "  APPDATA      = $roaming"
Write-Host "  LOCALAPPDATA = $local"
