# Open Questions

These questions are still useful, but they are no longer blocking the current headless MVP:

1. How visible should the difference be between built-in dataset collection and future pipeline-defined collection flows?
2. Should Python integration begin as `external executable` only, or should a `resident sidecar worker` appear in the first runtime expansion?
3. Should PLC-gated capture enter immediately after simulator-based collection, or stay one phase later?
4. When telemetry buffers fill up, which events may be dropped and which must always land in local logs?
5. Will package-level trust or signature validation be required later for external processes and scripts?

## Decisions Already Locked

- `Windows-first`
- strict typed graph
- separate `data edge` and `control edge`
- `PLC control node` remains a first-class graph concept
- `JSON + folder package` import/export
- telemetry is optional and non-blocking
- first telemetry preference is `WebSocket`
- real vendor cameras are not required for the first MVP
- multi-simulator source approach is preferred
- first MVP is `dataset-first`
- first MVP is `headless-first`

## Nearest Product Decision

The nearest scope decision is now:

- first complete increment = `built-in dataset capture + multi-simulator + package import`
- next increment = `PLC gate + actual frame persistence + session metadata`
