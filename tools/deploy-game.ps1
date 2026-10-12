# Build the release DLL and deploy it to the game folder.
#
#   powershell -ExecutionPolicy Bypass -File tools\deploy-game.ps1            build, then deploy
#   powershell -ExecutionPolicy Bypass -File tools\deploy-game.ps1 -SkipBuild deploy target\release\hachimi.dll as is
#
# Every deploy keeps a copy named by the build's SHA256 prefix, `cri_mana_vpx-<hash7>.dll`, and installs the same
# bytes as `cri_mana_vpx.dll`. The DLL it replaces is backed up under its own hash first, unless a copy with that
# hash is already there. The live DLL is only replaced when the game is not running: Windows keeps a loaded DLL
# locked, so with the game open the script still writes the hash-named copy and says the live one was skipped.
param(
    [switch]$SkipBuild,
    [string]$GameDir = "C:\Program Files (x86)\Steam\steamapps\common\UmamusumePrettyDerby"
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$src = Join-Path $repo "target\release\hachimi.dll"

if (-not $SkipBuild) {
    Push-Location $repo
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed" }
    }
    finally {
        Pop-Location
    }
}

if (-not (Test-Path $src)) { throw "missing $src, build first" }
if (-not (Test-Path $GameDir)) { throw "game folder not found: $GameDir" }

function Get-Hash7($path) {
    return (Get-FileHash -Algorithm SHA256 -Path $path).Hash.Substring(0, 7).ToLower()
}

$newHash = Get-Hash7 $src
$live = Join-Path $GameDir "cri_mana_vpx.dll"
$named = Join-Path $GameDir ("cri_mana_vpx-" + $newHash + ".dll")

Copy-Item -Force $src $named
Write-Output ("kept " + (Split-Path -Leaf $named))

$running = [bool](Get-Process -Name "UmamusumePrettyDerby" -ErrorAction SilentlyContinue)
if ($running) {
    Write-Warning "the game is running, so cri_mana_vpx.dll was not replaced. Close the game and run this again."
    exit 0
}

if (Test-Path $live) {
    $liveHash = Get-Hash7 $live
    $liveBackup = Join-Path $GameDir ("cri_mana_vpx-" + $liveHash + ".dll")
    if (-not (Test-Path $liveBackup)) {
        Copy-Item -Force $live $liveBackup
        Write-Output ("backed up previous DLL as " + (Split-Path -Leaf $liveBackup))
    }
}

Copy-Item -Force $src $live
Write-Output ("installed cri_mana_vpx.dll, hash " + $newHash)
