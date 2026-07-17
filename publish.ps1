#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Publishes MachineVisionFabric CLI + integration modules to a single output directory.

.PARAMETER Output
    Target publish directory. Defaults to publish/mvf.

.PARAMETER Runtime
    RID override (e.g. win-x64, linux-x64, osx-arm64). Defaults to current OS.

.PARAMETER SelfContained
    When true, bundles .NET runtime. When false (default), requires dotnet on PATH.

.EXAMPLE
    ./publish.ps1
    ./publish.ps1 -Output dist/mvf -SelfContained $true
#>
param(
    [string]$Output = "publish/mvf",
    [string]$Runtime = "",
    [bool]$SelfContained = $false,
    [switch]$IncludeRealWorld
)

$ErrorActionPreference = "Stop"
$Root = $PSScriptRoot

function Invoke-Publish([string]$project, [string]$target) {
    $ridArgs = if ($Runtime) { @("-r", $Runtime) } else { @() }
    $scArgs  = if ($SelfContained) { @("--self-contained", "true") } else { @("--self-contained", "false") }
    $full = Join-Path $Root $project
    Write-Host "  → $project" -ForegroundColor Cyan
    dotnet publish $full -c Release -o $target --nologo @ridArgs @scArgs
    if ($LASTEXITCODE -ne 0) { throw "Publish failed: $project" }
}

$out = Join-Path $Root $Output

Write-Host ""
Write-Host "═══════════════════════════════════════════════" -ForegroundColor DarkCyan
Write-Host "  MachineVisionFabric  publish" -ForegroundColor Cyan
Write-Host "  Output : $out" -ForegroundColor Gray
Write-Host "  Runtime: $(if ($Runtime) { $Runtime } else { 'host default' })" -ForegroundColor Gray
Write-Host "  SC     : $SelfContained" -ForegroundColor Gray
Write-Host "═══════════════════════════════════════════════" -ForegroundColor DarkCyan
Write-Host ""

# ── 1. CLI ────────────────────────────────────────────────────────────────────
Write-Host "[1/3] CLI" -ForegroundColor Yellow
Invoke-Publish "src/MachineVisionFabric.Cli/MachineVisionFabric.Cli.csproj" $out

# ── 2. Integration modules ────────────────────────────────────────────────────
Write-Host ""
Write-Host "[2/3] Integration modules" -ForegroundColor Yellow

$integrations = @(
    @{ Project = "examples/integrations/MachineVisionFabric.Integrations.DatasetWriter/MachineVisionFabric.Integrations.DatasetWriter.csproj"; Id = "mvf.dataset-writer" },
    @{ Project = "examples/integrations/MachineVisionFabric.Integrations.FolderSource/MachineVisionFabric.Integrations.FolderSource.csproj";     Id = "mvf.folder-source" },
    @{ Project = "examples/integrations/MachineVisionFabric.Integrations.SimulatedGate/MachineVisionFabric.Integrations.SimulatedGate.csproj";   Id = "mvf.simulated-gate" },
    @{ Project = "examples/integrations/MachineVisionFabric.Integrations.ResidentCameraStub/MachineVisionFabric.Integrations.ResidentCameraStub.csproj"; Id = "mvf.resident-camera-stub" }
)

if ($IncludeRealWorld) {
    $integrations += @{ Project = "real-world-projects/integrations/MachineVisionFabric.Integrations.CognexCamera/MachineVisionFabric.Integrations.CognexCamera.csproj"; Id = "mvf.realworld-cognex-camera" }
    $integrations += @{ Project = "real-world-projects/integrations/MachineVisionFabric.Integrations.DarkFrameFilter/MachineVisionFabric.Integrations.DarkFrameFilter.csproj"; Id = "mvf.realworld-dark-frame-filter" }
}

foreach ($m in $integrations) {
    $target = Join-Path (Join-Path $out "integrations") $m.Id
    Invoke-Publish $m.Project $target
}

# ── 3. Packages ───────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "[3/3] Packages" -ForegroundColor Yellow
$packagesDest = Join-Path $out "packages"

$packagesSource = Join-Path $Root "examples/packages"
if (Test-Path $packagesSource) {
    Write-Host "  → examples/packages → $packagesDest" -ForegroundColor Cyan
    Copy-Item -Path $packagesSource -Destination $packagesDest -Recurse -Force
}

if ($IncludeRealWorld) {
    $rwPackages = Join-Path $Root "real-world-projects/packages"
    if (Test-Path $rwPackages) {
        Write-Host "  → real-world-projects/packages → $packagesDest" -ForegroundColor Cyan
        Copy-Item -Path (Join-Path $rwPackages "*") -Destination $packagesDest -Recurse -Force
    }
}

# ── 4. Patch appsettings.json for published layout ───────────────────────────
Write-Host ""
Write-Host "[4/4] Patching appsettings.json for published layout" -ForegroundColor Yellow
$appSettings = Join-Path $out "appsettings.json"
if (Test-Path $appSettings) {
    $json = Get-Content $appSettings -Raw | ConvertFrom-Json
    $json.MachineVisionFabric.IntegrationsRoot = "integrations"
    $json.MachineVisionFabric.DatasetCapture.PackageRoot = "packages/dataset-capture-starter"
    $json.MachineVisionFabric.DatasetCapture.DatasetRoot = "datasets"
    $json | ConvertTo-Json -Depth 10 | Set-Content $appSettings
    Write-Host "  → IntegrationsRoot = integrations" -ForegroundColor Cyan
    Write-Host "  → PackageRoot      = packages/dataset-capture-starter" -ForegroundColor Cyan
}

# ── Done ──────────────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "═══════════════════════════════════════════════" -ForegroundColor DarkCyan
Write-Host "  Done!  $out" -ForegroundColor Green
Write-Host ""
Write-Host "  Usage:" -ForegroundColor Gray
Write-Host "  cd $out" -ForegroundColor White
Write-Host "  ./MachineVisionFabric.Cli execute-graph --package packages/dataset-capture-starter" -ForegroundColor White
Write-Host "═══════════════════════════════════════════════" -ForegroundColor DarkCyan
Write-Host ""
