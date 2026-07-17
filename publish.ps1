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

.PARAMETER NoClean
    Keeps the existing output directory. By default the output is wiped first, because
    a stale module left behind from an earlier publish is still discovered at runtime.

.EXAMPLE
    ./publish.ps1
    ./publish.ps1 -IncludeRealWorld
    ./publish.ps1 -Output dist/mvf -SelfContained $true
#>
param(
    [string]$Output = "publish/mvf",
    [string]$Runtime = "",
    [bool]$SelfContained = $false,
    [switch]$IncludeRealWorld,
    [switch]$NoClean
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
Write-Host "[1/4] CLI" -ForegroundColor Yellow
Invoke-Publish "src/MachineVisionFabric.Cli/MachineVisionFabric.Cli.csproj" $out

# ── 2. Integration modules ────────────────────────────────────────────────────
Write-Host ""
Write-Host "[2/4] Integration modules" -ForegroundColor Yellow

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
Write-Host "[3/4] Packages" -ForegroundColor Yellow
$packagesDest = Join-Path $out "packages"
New-Item -ItemType Directory -Force -Path $packagesDest | Out-Null

$packagesSource = Join-Path $Root "examples/packages"
if (Test-Path $packagesSource) {
    Write-Host "  → examples/packages → $packagesDest" -ForegroundColor Cyan
    # Copy the contents, not the folder itself: with an existing destination the latter
    # would nest a packages/packages directory on every run.
    Copy-Item -Path (Join-Path $packagesSource "*") -Destination $packagesDest -Recurse -Force
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
