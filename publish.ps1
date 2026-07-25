#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Publishes MachineVisionFabric CLI + integration modules to a single output directory.

.PARAMETER Output
    Target publish directory. Defaults to publish/mvf.

.PARAMETER Runtime
    RID override (e.g. win-x64, linux-x64, osx-arm64). Defaults to current OS.

.PARAMETER SelfContained
    When true, bundles the .NET runtime into the CLI. When false (default), requires dotnet
    on PATH. Only the CLI carries the runtime — integration modules are always published
    framework-dependent (just their DLLs) because the CLI host loads them into its own
    runtime, so bundling a runtime per module would duplicate it several times over.

.PARAMETER SingleFile
    When true (requires -Runtime), the CLI is published as a single self-extracting
    executable (PublishSingleFile). Implies a self-contained CLI.

.PARAMETER NoClean
    Keeps the existing output directory. By default the output is wiped first, because
    a stale module left behind from an earlier publish is still discovered at runtime.

.EXAMPLE
    ./publish.ps1
    ./publish.ps1 -Output dist/mvf -Runtime linux-x64 -SelfContained $true
    ./publish.ps1 -Output dist/mvf -Runtime win-x64 -SingleFile $true
#>
param(
    [string]$Output = "publish/mvf",
    [string]$Runtime = "",
    [bool]$SelfContained = $false,
    [bool]$SingleFile = $false,
    [switch]$NoClean
)

$ErrorActionPreference = "Stop"
$Root = $PSScriptRoot

if ($SingleFile -and -not $Runtime) {
    throw "-SingleFile requires -Runtime (single-file publish is runtime-specific)."
}

function Invoke-Publish([string]$project, [string]$target, [bool]$selfContained, [bool]$singleFile) {
    $ridArgs = if ($Runtime) { @("-r", $Runtime) } else { @() }
    $scArgs  = if ($selfContained) { @("--self-contained", "true") } else { @("--self-contained", "false") }
    $sfArgs  = if ($singleFile) { @("-p:PublishSingleFile=true", "-p:IncludeNativeLibrariesForSelfExtract=true") } else { @() }
    $full = Join-Path $Root $project
    Write-Host "  → $project" -ForegroundColor Cyan
    dotnet publish $full -c Release -o $target --nologo @ridArgs @scArgs @sfArgs
    if ($LASTEXITCODE -ne 0) { throw "Publish failed: $project" }
}

$out = Join-Path $Root $Output

Write-Host ""
Write-Host "═══════════════════════════════════════════════" -ForegroundColor DarkCyan
Write-Host "  MachineVisionFabric  publish" -ForegroundColor Cyan
Write-Host "  Output : $out" -ForegroundColor Gray
Write-Host "  Runtime: $(if ($Runtime) { $Runtime } else { 'host default' })" -ForegroundColor Gray
Write-Host "  SC     : $SelfContained" -ForegroundColor Gray
Write-Host "  Clean  : $(-not $NoClean)" -ForegroundColor Gray
Write-Host "═══════════════════════════════════════════════" -ForegroundColor DarkCyan
Write-Host ""

# ── 0. Clean ──────────────────────────────────────────────────────────────────
if (-not $NoClean -and (Test-Path $out)) {
    # Guard against wiping something that is not a publish output.
    $resolved = (Resolve-Path $out).Path.TrimEnd('\', '/')
    if ($resolved -eq $Root.TrimEnd('\', '/')) {
        throw "Refusing to clean '$resolved': that is the repository root. Pass a dedicated -Output directory."
    }

    Write-Host "[0/4] Cleaning $resolved" -ForegroundColor Yellow
    # Clear the contents rather than the directory itself: on Windows the directory cannot be
    # removed while any shell has it as its working directory, but its contents still can.
    Remove-Item -Path (Join-Path $resolved "*") -Recurse -Force
    Write-Host ""
}

# ── 1. CLI ────────────────────────────────────────────────────────────────────
# The CLI is the only component that carries the .NET runtime (self-contained / single-file).
Write-Host "[1/4] CLI" -ForegroundColor Yellow
Invoke-Publish "src/cli/Mvf.Cli/Mvf.Cli.csproj" $out ($SelfContained -or $SingleFile) $SingleFile

# ── 2. Integration modules ────────────────────────────────────────────────────
Write-Host ""
Write-Host "[2/4] Integration modules" -ForegroundColor Yellow

$integrations = @(
    @{ Project = "modules/MachineVisionFabric.Integrations.CognexCamera/MachineVisionFabric.Integrations.CognexCamera.csproj";     Id = "mvf.realworld-cognex-camera" },
    @{ Project = "modules/MachineVisionFabric.Integrations.DarkFrameFilter/MachineVisionFabric.Integrations.DarkFrameFilter.csproj"; Id = "mvf.realworld-dark-frame-filter" },
    @{ Project = "modules/MachineVisionFabric.Integrations.BlackScreenCheck/MachineVisionFabric.Integrations.BlackScreenCheck.csproj"; Id = "mvf.black-screen-check" },
    @{ Project = "modules/MachineVisionFabric.Integrations.DatasetWriter/MachineVisionFabric.Integrations.DatasetWriter.csproj";       Id = "mvf.dataset-writer" },
    @{ Project = "modules/dotnet-brightness-gate/Mvf.Example.BrightnessGate.csproj";                                                  Id = "mvf.example-brightness-gate" }
)

# Integration modules are always framework-dependent: the CLI host loads them into its own
# runtime, so a per-module runtime would just duplicate what the CLI already ships.
foreach ($m in $integrations) {
    $target = Join-Path (Join-Path $out "integrations") $m.Id
    Invoke-Publish $m.Project $target $false $false
}

# ── 3. Packages ───────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "[3/4] Packages" -ForegroundColor Yellow
$packagesDest = Join-Path $out "packages"
New-Item -ItemType Directory -Force -Path $packagesDest | Out-Null

$packagesSource = Join-Path $Root "packages"
if (Test-Path $packagesSource) {
    Write-Host "  → packages → $packagesDest" -ForegroundColor Cyan
    # Copy the contents, not the folder itself: with an existing destination the latter
    # would nest a packages/packages directory on every run.
    Copy-Item -Path (Join-Path $packagesSource "*") -Destination $packagesDest -Recurse -Force
}

# ── 4. Patch appsettings.json for published layout ───────────────────────────
Write-Host ""
Write-Host "[4/4] Patching appsettings.json for published layout" -ForegroundColor Yellow
$appSettings = Join-Path $out "appsettings.json"
if (Test-Path $appSettings) {
    $json = Get-Content $appSettings -Raw | ConvertFrom-Json
    $json.MachineVisionFabric.IntegrationsRoot = "integrations"
    $json.MachineVisionFabric.DatasetCapture.PackageRoot = "packages/inspection-demo"
    $json.MachineVisionFabric.DatasetCapture.DatasetRoot = "datasets"
    $json | ConvertTo-Json -Depth 10 | Set-Content $appSettings
    Write-Host "  → IntegrationsRoot = integrations" -ForegroundColor Cyan
    Write-Host "  → PackageRoot      = packages/inspection-demo" -ForegroundColor Cyan
}

# ── Done ──────────────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "═══════════════════════════════════════════════" -ForegroundColor DarkCyan
Write-Host "  Done!  $out" -ForegroundColor Green
Write-Host ""
Write-Host "  Usage:" -ForegroundColor Gray
Write-Host "  cd $out" -ForegroundColor White
Write-Host "  ./Mvf.Cli execute-graph --package packages/inspection-demo" -ForegroundColor White
Write-Host "═══════════════════════════════════════════════" -ForegroundColor DarkCyan
Write-Host ""
