[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[A-Za-z][A-Za-z0-9]*$')]
    [string]$Name,

    [string]$ModuleId,

    [string]$DisplayName,

    [string]$CapabilityName = "camera-dataset-source"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function ConvertTo-KebabCase {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Value
    )

    return ([regex]::Replace($Value, '(?<!^)([A-Z])', '-$1')).ToLowerInvariant()
}

function Add-ProjectToSolution {
    param(
        [Parameter(Mandatory = $true)]
        [string]$SolutionFile,

        [Parameter(Mandatory = $true)]
        [string]$ProjectPath
    )

    [xml]$solutionXml = Get-Content -LiteralPath $SolutionFile
    $folderNode = $solutionXml.Solution.Folder | Where-Object { $_.Name -eq "/integrations/" } | Select-Object -First 1

    if ($null -eq $folderNode) {
        throw "Integrations folder node was not found in solution: $SolutionFile"
    }

    $existingProject = $folderNode.Project | Where-Object { $_.Path -eq $ProjectPath } | Select-Object -First 1
    if ($null -ne $existingProject) {
        return
    }

    $projectNode = $solutionXml.CreateElement("Project")
    $pathAttribute = $solutionXml.CreateAttribute("Path")
    $pathAttribute.Value = $ProjectPath
    [void]$projectNode.Attributes.Append($pathAttribute)
    [void]$folderNode.AppendChild($projectNode)
    $solutionXml.Save($SolutionFile)
}

$scriptRoot = Split-Path -Parent $PSCommandPath
$realWorldRoot = Split-Path -Parent $scriptRoot
$integrationsRoot = Join-Path $realWorldRoot "integrations"
$starterName = "CameraDatasetStarter"
$starterProjectName = "MachineVisionFabric.Integrations.$starterName"
$starterRoot = Join-Path $integrationsRoot $starterProjectName
$targetProjectName = "MachineVisionFabric.Integrations.$Name"
$targetRoot = Join-Path $integrationsRoot $targetProjectName
$solutionPath = Join-Path $realWorldRoot "MachineVisionFabric.RealWorld.slnx"

if (-not (Test-Path -LiteralPath $starterRoot)) {
    throw "Starter integration was not found: $starterRoot"
}

if (Test-Path -LiteralPath $targetRoot) {
    throw "Target integration already exists: $targetRoot"
}

if ([string]::IsNullOrWhiteSpace($ModuleId)) {
    $moduleSlug = ConvertTo-KebabCase -Value $Name
    $ModuleId = "mvf.realworld-$moduleSlug"
}

if ([string]::IsNullOrWhiteSpace($DisplayName)) {
    $DisplayName = "Real-World $Name"
}

New-Item -ItemType Directory -Path $targetRoot | Out-Null

$starterFiles = Get-ChildItem -LiteralPath $starterRoot -File | Where-Object { $_.DirectoryName -eq $starterRoot }

foreach ($file in $starterFiles) {
    $targetFileName = $file.Name.Replace($starterName, $Name)
    $targetPath = Join-Path $targetRoot $targetFileName

    $content = Get-Content -LiteralPath $file.FullName -Raw
    $content = $content.Replace($starterProjectName, $targetProjectName)
    $content = $content.Replace($starterName, $Name)
    $content = $content.Replace("mvf.realworld-camera-starter", $ModuleId)
    $content = $content.Replace("Real-World Camera Dataset Starter", $DisplayName)
    $content = $content.Replace("camera-dataset-source", $CapabilityName)
    $content = $content.Replace("Project-local resident camera source starter built for real-world dataset collection work.", "Project-local resident camera source module for real-world dataset collection work.")
    $content = $content.Replace("Project-local resident camera source starter for real-world dataset collection scenarios.", "Project-local resident camera source module for real-world dataset collection scenarios.")
    [System.IO.File]::WriteAllText($targetPath, $content, [System.Text.Encoding]::UTF8)
}

$projectPath = "integrations/$targetProjectName/$targetProjectName.csproj"
Add-ProjectToSolution -SolutionFile $solutionPath -ProjectPath $projectPath

Write-Host ""
Write-Host "Created integration project: $targetProjectName"
Write-Host "Module id: $ModuleId"
Write-Host "Display name: $DisplayName"
Write-Host "Capability: $CapabilityName"
Write-Host ""
Write-Host "Next steps:"
Write-Host "1. Replace the producer logic in $Name`Session.cs with the vendor SDK frame acquisition flow."
Write-Host "2. Adjust $Name`Options.cs for vendor-specific connection and buffering settings."
Write-Host "3. Update integration-module.json if you need additional capabilities."
Write-Host "4. Build with: dotnet build real-world-projects\\MachineVisionFabric.RealWorld.slnx -v minimal"
