# Cognex HMI Source

`MachineVisionFabric.Integrations.CognexCamera` is the first real-world camera source under `real-world-projects/`.

Design choices:

- open-source safe: no Cognex SDK DLL dependency
- transport: Cognex HMI WebSocket + HTTP image fetch
- runtime model: resident source session
- acquisition modes:
  - `passive-listen`
  - `manual-trigger-loop`

Use `passive-listen` when the station or the camera job already produces images after the product arrives.

Use `manual-trigger-loop` when you want the platform to keep issuing software triggers and validate the pipeline path before PLC wiring is complete.

Operational notes:

- if In-Sight Explorer holds the session aggressively, HMI software trigger behavior may be unstable
- keep the camera reachable on the same network from the runtime machine
- default HMI endpoint assumptions:
  - websocket: `ws://<ip>:8087/ws`
  - image fetch: HTTP on port `8087`
