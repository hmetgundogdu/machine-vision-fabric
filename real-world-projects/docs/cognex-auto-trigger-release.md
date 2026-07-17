MachineVisionFabric Cognex Auto-Trigger Release

What this release does

- connects to a Cognex In-Sight camera over HMI WebSocket and HTTP image fetch
- waits for camera-side auto-trigger image/result events
- stores incoming frames continuously
- rejects very dark frames before dataset persistence through the OpenCV processor node

Before running

1. Open `real-world-projects\packages\dataset-capture-cognex-auto-trigger-dark-filter\profile.json`
2. Change `source.config.ipAddress` to your camera IP if needed
3. If camera credentials differ, update:
   - `source.config.username`
   - `source.config.password`
4. If you want a different darkness threshold, open:
   - `real-world-projects\packages\dataset-capture-cognex-auto-trigger-dark-filter\manifest.json`
   - adjust `frameProcessor.config.minimumMeanBrightness`

How to run

- double click `run-cognex-auto-trigger.ps1`

or

```powershell
.\MachineVisionFabric.Cli.exe run --integrations-root . --package real-world-projects\packages\dataset-capture-cognex-auto-trigger-dark-filter --dataset-root artifacts\datasets-cognex --session-prefix cognex-auto
```

Output

- dataset sessions are written under `artifacts\datasets-cognex`
- each session contains:
  - `images\`
  - `metadata\`
  - `session.json`
