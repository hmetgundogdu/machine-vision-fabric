# Universal Node Contract — `mvf.dev/v1alpha1`

A **node** is a logical workload with a versioned typed interface, one or more executable artifacts,
placement, isolation, memory, lifecycle and resource policy. The graph refers to the logical node;
the runtime negotiates *how* it runs. Implementation: `rust/crates/mvf-contract`.

`v1alpha1` is a research schema. It may change without migration until the ADR (#16) freezes it.

## Example

```yaml
apiVersion: mvf.dev/v1alpha1
kind: NodeContract
metadata:
  id: py.brightness-classifier        # [a-z][a-z0-9]* segments joined by . - _
  version: 2.1.0                      # semantic version, always a string
spec:
  ports:
    inputs:
      - { name: frame, channel: data, type: frame }
    outputs:
      - { name: class, channel: data, type: classification }
      - { name: dark,  channel: control, type: signal }
  artifacts:                          # in order of preference
    - backend: worker
      runtime: python
      entry: classifier.py            # relative, inside the node package
      protocol: 1
  execution:
    backend: auto                     # or pin: in-process | worker | oci | wasm | compute
    placement: { target: cpu, fallback: deny }
  isolation: { minimum: process }     # none < process < sandbox < vm
  memory: { domain: host, zeroCopy: required }
  lifecycle:
    activation: resident
    readinessTimeoutMs: 15000
    drainTimeoutMs: 5000
    executeTimeoutMs: 2000            # 0 = no deadline
    replacement: stateless            # unsupported | stateless | stateful (+ state:)
  resources: { memoryMib: 512, enforcement: best-effort }
```

## Fields

| Field | Values | Default | Notes |
|---|---|---|---|
| `ports.*.channel` | `data`, `control` | — | Never interchangeable. |
| `ports.*.type` | `frame`, `tensor`, `classification`, `json`, `value`, `signal` | — | `signal` is the only control type; every other type is data. |
| `ports.inputs[].required` | bool | `true` | Outputs may not set it. |
| `artifacts[].backend` | `in-process`, `worker`, `oci`, `wasm`, `compute` | — | Each variant has its own closed field set. |
| `artifacts[].targets` | `[cpu]`, `[gpu]`, `[cpu, gpu]` | `[cpu]` (`compute` is `[gpu]`) | Negotiation never translates an artifact to an undeclared target. |
| `execution.backend` | `auto` or a backend | `auto` | A pinned backend is never substituted. |
| `execution.placement.target` | `cpu`, `gpu` | `cpu` | |
| `execution.placement.fallback` | `deny`, `cpu` | `deny` | A fallback is always reported as a placement note. |
| `execution.maxParallelism` | `1..=64` | `1` | Instances the pipelined executor may run side by side (node `parallelism`). Above 1 asserts the node keeps no state; refused for `replacement: stateful` (`contract.execution.stateful-parallelism`). |
| `isolation.minimum` | `none`, `process`, `sandbox`, `vm` | `none` | Replaces the ambiguous `isolation.required: true` (rejected as an unknown field). |
| `memory.domain` | `host`, `device` | `host` | `device` requires `target: gpu`. |
| `memory.zeroCopy` | `required`, `preferred`, `disabled` | `preferred` | `required` fails negotiation on a copying backend. |
| `permissions.network` | `none`, `loopback`, `host`, `restricted`, or `{ mode: restricted, inbound: [...], outbound: deny \| [...] }` | `none` | Enforced only by a sandbox backend; trusted backends share the host network (logged). `restricted` needs `isolation.minimum: sandbox`. See [stage 16](stages/16.md). |
| `permissions.display.mode` | `none`, `native`, `managed-surface` | `none` | Applies to every backend (an allocation, not only a restriction). `native`: the node's own toolkit opens windows; needs an interactive session (`negotiation.display-unavailable` otherwise). `managed-surface`: shared-memory surfaces a separate `mvf-display-host` presents (workers and OCI nodes; an OCI node sees only its own surfaces, bind-mounted at `/mvf/display`, and never a display server socket, so containers are not granted `native`). A node without `native` does not inherit `DISPLAY`/`WAYLAND_DISPLAY` (hygiene for trusted backends, not enforcement). See [stage 18](stages/18.md) and `protocol/display-surface.md`. |
| `permissions.display.required` | bool | `true` | `false`: run headless (`placement.display-headless`) where the mode is unavailable. |
| `permissions.display.maxWindows`, `maxWidth`, `maxHeight` | `1..=8`; `16..=8192` | `1`; `1920`×`1080` | One surface per window, sized for the largest frame. |
| `permissions.display.keyboard`, `mouse` | bool | `false` | Input the display host routes to the node's surfaces; anything else is refused by the host. Native windows take OS input directly (`placement.display-input-unmediated`). |
| `permissions.display.clipboard`, `fullscreen` | bool | `false` | Not grantable yet: asking is rejected (`negotiation.display-clipboard-unsupported`, `-fullscreen-unsupported`), never ignored. |
| `presentation` | `{ title, width, height }` | node id, 1280×720 | Window hints for the host; needs a display mode. |
| `isolation.syscalls` | `denylist`, `allowlist` | `denylist` | Sandbox seccomp policy; `allowlist` is Docker's default profile minus the denylist. Needs `isolation.minimum: sandbox`. |
| `isolation.lsm` | `optional`, `required` | `optional` | `required` fails negotiation unless the host applies an AppArmor/SELinux profile. Needs `minimum: sandbox`. |
| `memory.arena` | `shared`, `private` | `shared` | `private`: the node's process maps an arena of its own (no other node's payload is visible to it); inputs are copied in. Needs a backend with private arenas (worker, OCI); incompatible with `zeroCopy: required`. |
| `lifecycle.activation` | `resident`, `on-demand` | `resident` | MVF defaults: sources, PLC and models are resident. |
| `lifecycle.replacement` | `unsupported`, `stateless`, `stateful` | `unsupported` | `stateful` requires `state.schemaVersion` (+ `migratesFrom`). |
| `resources.enforcement` | `best-effort`, `required` | `best-effort` | `required` rejects any backend that cannot enforce every declared limit. |

### Isolation levels — what each does **not** guarantee

| Level | Guarantees | Does not guarantee |
|---|---|---|
| `none` | Nothing beyond Rust memory safety of the host. | A panic is caught, but a native fault, leak or infinite loop affects the engine. Not a security boundary. |
| `process` | A crash or hang is contained and the process can be killed. | Not a security boundary: same user, same filesystem, same network. |
| `sandbox` | An enforced security boundary (WASM sandbox, namespaces/cgroups/seccomp, AppContainer). | Hardware isolation; kernel vulnerabilities. |
| `vm` | Hardware virtualisation boundary. | Side channels. |

## Strict YAML rules

All MVF YAML (contracts and pipelines) goes through one loader (`mvf_contract::yaml`):

- one document only; no anchors, aliases or tags (`yaml.anchor-forbidden`, `yaml.tag-forbidden`);
- duplicate keys are errors (`yaml.duplicate-key`), never last-one-wins; keys are scalars;
- bounded: 256 KiB input, depth 32, 20 000 nodes, 64 KiB per scalar;
- plain scalars resolve with a YAML 1.2 core-schema subset: `null`/`~`/empty, `true`/`false`,
  decimal integers and finite decimal floats. `yes`, `0x1f`, `.inf` stay strings, so a typed
  field rejects them instead of guessing. `version: 1.0` is a float and fails the string field;
- unknown fields anywhere are rejected with the field path (`yaml.schema`, `spec.ports.inputs[0]`).

## Capability negotiation

A host advertises one `BackendCapabilities` per backend it **actually provides**: isolation level,
targets, zero-copy memory domains, cancellation (`cooperative` vs `enforceable`), enforceable
termination, enforceable memory/cpu/pids limits and supported replacement modes.

`negotiate(contract, available)` walks targets (preferred, then CPU when `fallback: cpu`) and
artifacts in declared order, and returns the first candidate satisfying every mandatory
requirement. Optional deviations become `PlacementNote`s (`placement.fallback-to-cpu`,
`placement.copying-hand-off`, `placement.advisory-limits`). Otherwise it returns a
`NegotiationError` listing every rejected candidate with every reason
(`negotiation.backend-unavailable`, `negotiation.isolation-insufficient`,
`negotiation.zero-copy-unavailable`, `negotiation.resource-limit-unenforceable`,
`negotiation.replacement-unsupported`, `negotiation.target-unsupported`).

## Error codes

YAML: `yaml.too-large`, `yaml.too-deep`, `yaml.too-many-nodes`, `yaml.scalar-too-long`,
`yaml.syntax`, `yaml.empty`, `yaml.multiple-documents`, `yaml.anchor-forbidden`,
`yaml.alias-forbidden`, `yaml.tag-forbidden`, `yaml.complex-key`, `yaml.duplicate-key`,
`yaml.schema`.

Contract: `contract.api-version.unsupported`, `contract.kind.unsupported`,
`contract.metadata.invalid-id`, `contract.metadata.invalid-version`, `contract.port.none`,
`contract.port.too-many`, `contract.port.invalid-name`, `contract.port.duplicate-name`,
`contract.port.channel-type-mismatch`, `contract.port.optional-output`, `contract.artifact.none`,
`contract.artifact.too-many`, `contract.artifact.invalid-builtin`, `contract.artifact.unsafe-path`,
`contract.artifact.unsupported-protocol`, `contract.artifact.invalid-image`,
`contract.artifact.invalid-digest`, `contract.artifact.invalid-entry-point`,
`contract.artifact.no-targets`, `contract.artifact.duplicate-target`,
`contract.execution.backend-without-artifact`, `contract.memory.private-arena-copies`, `contract.execution.invalid-max-parallelism`, `contract.execution.stateful-parallelism`, `contract.placement.no-artifact-for-target`,
`contract.placement.redundant-fallback`, `contract.memory.device-without-gpu`,
`contract.memory.zero-copy-conflicts-with-fallback`, `contract.lifecycle.timeout-out-of-range`,
`contract.lifecycle.stateful-without-state`, `contract.lifecycle.state-without-stateful`,
`contract.lifecycle.invalid-state-version`, `contract.lifecycle.invalid-migration`,
`contract.resources.zero-limit`, `contract.resources.required-without-limits`.
