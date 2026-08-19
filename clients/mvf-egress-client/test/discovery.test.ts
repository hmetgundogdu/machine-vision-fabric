import test from 'node:test';
import assert from 'node:assert/strict';
import { parseBeacon, EdgeRegistry } from '../src/discovery.ts';

const encode = (obj: unknown): Uint8Array => new TextEncoder().encode(JSON.stringify(obj));

test('parseBeacon accepts a well-formed beacon', () => {
  const beacon = parseBeacon(encode({
    mvf: 'egress-beacon', v: 1, edgeId: 'panel-1', pipeline: 'inspection',
    transport: 'ws', port: 8791, streams: 'state,frame', status: 'running',
  }));

  assert.ok(beacon);
  assert.equal(beacon!.edgeId, 'panel-1');
  assert.equal(beacon!.transport, 'ws');
  assert.equal(beacon!.port, 8791);
  assert.equal(beacon!.streams, 'state,frame');
});

test('parseBeacon rejects unrelated datagrams', () => {
  assert.equal(parseBeacon(encode({ foo: 1 })), null);
  assert.equal(parseBeacon(new TextEncoder().encode('not json')), null);
});

test('EdgeRegistry expires edges past their TTL', () => {
  const registry = new EdgeRegistry();
  const beacon = {
    edgeId: 'panel-1', pipeline: 'p', transport: 'tcp', port: 8791, streams: 'state', status: 'running',
  };

  registry.apply(beacon, 1000);
  assert.equal(registry.live(3000, 2000).length, 1); // fresh
  assert.equal(registry.live(3000, 5000).length, 0); // 4s old > 3s ttl → dropped
});
