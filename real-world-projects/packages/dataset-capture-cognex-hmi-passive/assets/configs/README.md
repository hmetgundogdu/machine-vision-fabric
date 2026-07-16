Passive Cognex profile notes

- Set `ipAddress` in `profile.json` to the camera IP visible in In-Sight Explorer.
- Use this profile when the station or the camera job already triggers image acquisition.
- The pipeline only listens for HMI image/result events and stores the incoming frames.
- This package expects `mvf.realworld-cognex-camera` and `mvf.simulated-gate` to be discoverable from the selected integrations root.
