# mvf-egress-client

Consumer SDK for **MachineVisionFabric realtime egress** — decode a running pipeline's live
frame-state / frame-data stream. See [`docs/realtime-egress-design.md`](../../docs/realtime-egress-design.md)
for the wire format; this package is the consumer side of `src/egress/Mvf.Egress`.

The **codec + framing are isomorphic** (browser and Node). The **TCP client** (`streamRecords`) is
Node-only; a browser build uses the WebSocket entry (a later slice) over the same codec.

## Watch a running pipeline

Start an edge with egress on:

```bash
mvf execute-graph --package packages/multilang-demo --egress tcp --egress-port 8791
```

Then, from Node:

```ts
import { streamRecords, EgressStreamKind } from 'mvf-egress-client';

for await (const record of streamRecords({ host: '127.0.0.1', port: 8791 })) {
  if (record.kind === EgressStreamKind.Cycle) {
    console.log(`cycle ${record.cycleIndex}: accepted ${record.acceptedCycles}`);
  } else {
    console.log(`${record.nodeId}.${record.port} -> ${record.outputFrameBytes}B`);
  }
}
```

Or use the bundled reference viewer:

```bash
npm run viewer -- --port 8791                 # TCP
npm run viewer -- --url ws://127.0.0.1:8791/   # WebSocket
```

## Browser (WebSocket)

A browser can't open a raw UDP/TCP socket, so start the edge with a **WebSocket** egress
(`mvf execute-graph … --egress ws --egress-port 8791`) and import the browser entry:

```ts
import { streamRecordsWs, EgressStreamKind } from 'mvf-egress-client/browser';

for await (const record of streamRecordsWs('ws://127.0.0.1:8791/')) { /* … */ }
```

`examples/browser-viewer.html` is a runnable page (after `npm run build`).

## Discovery (Node)

An active edge announces itself on a UDP multicast group. List live edges without prior config:

```ts
import { discoverEdges, EdgeRegistry } from 'mvf-egress-client';

const registry = new EdgeRegistry();
for await (const beacon of discoverEdges()) {
  registry.apply(beacon);
  console.log(registry.live()); // self-expiring list of streaming edges
}
```

## Develop

- `npm test` — runs the codec / framing tests via Node's built-in test runner and type stripping
  (no dependencies, no build step; needs Node >= 22.6).
- `npm run build` — emits `dist/` (JS + `.d.ts`) for publishing; needs `npm install` for TypeScript.
