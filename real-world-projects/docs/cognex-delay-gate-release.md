MachineVisionFabric Cognex Delay-Gate Release

What this release does

- connects to a Cognex In-Sight camera over HMI WebSocket and HTTP image fetch
- waits 10 seconds before the temporary product gate returns `true`
- captures a trigger-window dataset session

Before running

1. Open `real-world-projects\packages\dataset-capture-cognex-delay-gate\profile.json`
2. Change `source.config.ipAddress` to your camera IP if needed
3. If camera credentials differ, update:
   - `source.config.username`
   - `source.config.password`

How to run

- double click `run-cognex-delay-gate.ps1`

or

```powershell
.\MachineVisionFabric.Cli.exe run --integrations-root . --package real-world-projects\packages\dataset-capture-cognex-delay-gate --dataset-root artifacts\datasets-cognex --session-prefix cognex-delay
```

Output

- dataset sessions are written under `artifacts\datasets-cognex`
- each session contains:
  - `images\`
  - `metadata\`
  - `session.json`
