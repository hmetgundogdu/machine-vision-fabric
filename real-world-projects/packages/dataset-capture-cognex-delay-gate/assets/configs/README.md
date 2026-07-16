Cognex delay-gate package

- This package is for temporary pipeline validation without PLC.
- The gate stays `false` for `10` seconds, then returns `true`.
- Change camera IP from `profile.json`:
  - `source.config.ipAddress`
- Default trigger mode:
  - `manual-trigger-loop`
- Default dataset output:
  - `artifacts/datasets-cognex`
