// Public surface of the MachineVisionFabric realtime-egress consumer SDK.
//
// The codec + framing are isomorphic (browser + Node). The TCP client (`streamRecords`) is Node-only;
// a browser build importing it will fail to resolve `node:net`, which is intentional — browsers use the
// WebSocket entry (added in a later slice) over the same codec.

export * from './codec.ts';
export * from './framing.ts';
export * from './client.ts';
export * from './ws-client.ts';
export * from './discovery.ts';
