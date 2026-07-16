# Real-World Projects

This folder is intentionally separate from `src/`.
It stays in the same repository, but it has its own solution boundary: `MachineVisionFabric.RealWorld.slnx`.

Purpose:

- hold project-specific camera integrations
- hold project-specific packages and dataset collection scenarios
- evolve station or customer work without polluting the platform core

Solution rule:

- `MachineVisionFabric.slnx` = platform solution
- `real-world-projects/MachineVisionFabric.RealWorld.slnx` = project-specific solution

Current starter:

- `integrations/MachineVisionFabric.Integrations.CameraDatasetStarter`
- `packages/dataset-capture-camera-starter`
- `integrations/MachineVisionFabric.Integrations.CognexCamera`
- `packages/dataset-capture-cognex-delay-gate`
- `packages/dataset-capture-cognex-hmi-passive`
- `packages/dataset-capture-cognex-hmi-trigger-window`

Scaffold a new project-specific integration from the starter:

```powershell
powershell -ExecutionPolicy Bypass -File real-world-projects\tools\New-MvfRealWorldIntegration.ps1 -Name CognexCamera -DisplayName "Cognex Camera Dataset Source"
```

Run the starter scenario:

```powershell
dotnet src\MachineVisionFabric.Cli\bin\Debug\net10.0\MachineVisionFabric.Cli.dll run --integrations-root real-world-projects\integrations --package real-world-projects\packages\dataset-capture-camera-starter --dataset-root artifacts\datasets-real-world --session-prefix real-world
```

List only real-world modules:

```powershell
dotnet src\MachineVisionFabric.Cli\bin\Debug\net10.0\MachineVisionFabric.Cli.dll modules --root real-world-projects\integrations
```

Suggested next step:

- clone `MachineVisionFabric.Integrations.CameraDatasetStarter`
- rename it to a real vendor adapter such as `MachineVisionFabric.Integrations.CognexCamera`
- replace the file-backed producer inside `CameraDatasetStarterSession` with real SDK acquisition code

The scaffold script above automates that rename and solution registration step.

Run the Cognex passive HMI package:

```powershell
dotnet run --project src\MachineVisionFabric.Cli -- run --integrations-root . --package real-world-projects\packages\dataset-capture-cognex-hmi-passive --dataset-root artifacts\datasets-cognex --session-prefix cognex-passive
```

Run the Cognex trigger-window package:

```powershell
dotnet run --project src\MachineVisionFabric.Cli -- run --integrations-root . --package real-world-projects\packages\dataset-capture-cognex-hmi-trigger-window --dataset-root artifacts\datasets-cognex --session-prefix cognex-trigger
```

Run the Cognex delay-gate package:

```powershell
dotnet run --project src\MachineVisionFabric.Cli -- run --integrations-root . --package real-world-projects\packages\dataset-capture-cognex-delay-gate --dataset-root artifacts\datasets-cognex --session-prefix cognex-delay
```

Why `--integrations-root .` is used here:

- the Cognex source module lives under `real-world-projects/`
- the simulated or PLC gate modules live under `examples/`
- scanning the repository root lets the runtime resolve both built module sets together
