Cognex auto-trigger dark-filter package

- Camera runs in auto-trigger mode.
- The runtime does not fire software trigger in this package.
- It waits for HMI websocket result/image events and stores incoming frames continuously.
- Very dark frames are rejected by the OpenCV processor before dataset persistence.

Change these values in `profile.json` if needed:

- `source.config.ipAddress`
- `source.config.username`
- `source.config.password`

Change this value in `manifest.json` if needed:

- `frameProcessor.config.minimumMeanBrightness`
