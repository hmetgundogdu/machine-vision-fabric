Trigger-window Cognex profile notes

- Set `ipAddress` in `profile.json` to the camera IP visible in In-Sight Explorer.
- This package actively triggers the camera through HMI and uses the simulated gate module to exercise the trigger-window capture path.
- Replace `mvf.simulated-gate` in `manifest.json` with `mvf.tcp-plc-gate` or `mvf.s7-gateway-gate` when the real product-present signal is ready.
