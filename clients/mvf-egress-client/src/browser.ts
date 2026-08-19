// Browser-safe entry point: the isomorphic codec + framing + the WebSocket client only.
// It deliberately does NOT export the Node TCP client (`node:net`) or UDP discovery (`node:dgram`),
// so a browser bundle never tries to resolve a Node built-in. A browser observer connects over WebSocket
// (a browser cannot open a raw UDP/TCP socket) and discovers via a known URL rather than UDP multicast.

export * from './codec.ts';
export * from './framing.ts';
export * from './ws-client.ts';
