[CmdletBinding()]
param(
    [string]$Version = "20260716",
    [string]$Configuration = "Release"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$releaseRoot = Join-Path $repoRoot ("artifacts\\releases\\mvf-cognex-delay-gate-" + $Version)
$zipPath = $releaseRoot + ".zip"

if (Test-Path -LiteralPath $releaseRoot) {
    Remove-Item -LiteralPath $releaseRoot -Recurse -Force
}

if (Test-Path -LiteralPath $zipPath) {
    Remove-Item -LiteralPath $zipPath -Force
}

New-Item -ItemType Directory -Path $releaseRoot | Out-Null
New-Item -ItemType Directory -Path (Join-Path $releaseRoot "src") | Out-Null

function Copy-Tree {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Source,

        [Parameter(Mandatory = $true)]
        [string]$Destination
    )

    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    $null = robocopy $Source $Destination /E /XD .vs obj TestResults > $null
    $exitCode = $LASTEXITCODE
    if ($exitCode -ge 8) {
        throw "robocopy failed for '$Source' -> '$Destination' with exit code $exitCode"
    }
}

Push-Location $repoRoot
try {
    dotnet build MachineVisionFabric.slnx -c $Configuration -v minimal
    dotnet build real-world-projects\MachineVisionFabric.RealWorld.slnx -c $Configuration -v minimal
    dotnet publish src\MachineVisionFabric.Cli\MachineVisionFabric.Cli.csproj -c $Configuration -o $releaseRoot

    Copy-Tree -Source (Join-Path $repoRoot "examples") -Destination (Join-Path $releaseRoot "examples")
    Copy-Tree -Source (Join-Path $repoRoot "real-world-projects") -Destination (Join-Path $releaseRoot "real-world-projects")

    $readmeSource = Join-Path $repoRoot "real-world-projects\docs\cognex-delay-gate-release.md"
    Copy-Item -LiteralPath $readmeSource -Destination (Join-Path $releaseRoot "README.txt")

    $runScript = @'
$ErrorActionPreference = "Stop"
Set-Location -LiteralPath $PSScriptRoot

.\MachineVisionFabric.Cli.exe run --integrations-root . --package real-world-projects\packages\dataset-capture-cognex-delay-gate --dataset-root artifacts\datasets-cognex --session-prefix cognex-delay
'@
    [System.IO.File]::WriteAllText((Join-Path $releaseRoot "run-cognex-delay-gate.ps1"), $runScript, [System.Text.Encoding]::ASCII)

    Compress-Archive -Path (Join-Path $releaseRoot "*") -DestinationPath $zipPath
}
finally {
    Pop-Location
}

Write-Host "ReleaseRoot: $releaseRoot"
Write-Host "ReleaseZip: $zipPath"
