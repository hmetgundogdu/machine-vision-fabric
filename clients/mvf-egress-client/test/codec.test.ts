import test from 'node:test';
import assert from 'node:assert/strict';
import {
  EgressStreamKind,
  PayloadElementType,
  PayloadMediaType,
  buildDescriptor,
  decodeRecord,
  encodeCycle,
  encodeNodeTransition,
  encodeNodeTransitionFrame,
  toTypedArray,
  type CycleRecord,
  type NodeTransitionRecord,
} from '../src/codec.ts';
import { FrameSplitter } from '../src/framing.ts';

// strip the u32 length prefix a full frame carries
const body = (frame: Uint8Array): Uint8Array => frame.subarray(4);

test('cycle record round-trips', () => {
  const frame = encodeCycle({
    runId: '0123456789abcdef0123456789abcdef',
    cycleIndex: 4,
    seq: 7,
    totalCycles: 5,
    acceptedCycles: 3,
    accepted: true,
    elapsedMillis: 1234,
  });

  const decoded = decodeRecord(body(frame)) as CycleRecord;
  assert.equal(decoded.kind, EgressStreamKind.Cycle);
  assert.equal(decoded.runId, '0123456789abcdef0123456789abcdef');
  assert.equal(decoded.cycleIndex, 4);
  assert.equal(decoded.seq, 7);
  assert.equal(decoded.totalCycles, 5);
  assert.equal(decoded.acceptedCycles, 3);
  assert.equal(decoded.accepted, true);
  assert.equal(decoded.elapsedMillis, 1234);
});

test('node-transition record round-trips', () => {
  const frame = encodeNodeTransition({
    runId: 'ffffffffffffffffffffffffffffffff',
    cycleIndex: 2,
    seq: 11,
    nodeId: 'classify1',
    port: 'frame',
    durationMicros: 987,
    hasOutput: true,
    faulted: false,
    outputFrameBytes: 4096,
    hasPayload: false,
  });

  const decoded = decodeRecord(body(frame)) as NodeTransitionRecord;
  assert.equal(decoded.kind, EgressStreamKind.NodeTransition);
  assert.equal(decoded.nodeId, 'classify1');
  assert.equal(decoded.port, 'frame');
  assert.equal(decoded.cycleIndex, 2);
  assert.equal(decoded.seq, 11);
  assert.equal(decoded.durationMicros, 987);
  assert.equal(decoded.hasOutput, true);
  assert.equal(decoded.faulted, false);
  assert.equal(decoded.outputFrameBytes, 4096);
  assert.equal(decoded.hasPayload, false);
});

test('node-transition frame payload round-trips and wraps as a typed array', () => {
  const descriptor = buildDescriptor(PayloadMediaType.Image, PayloadElementType.Int16, [2]);
  const payload = new Uint8Array([1, 0, 2, 0]); // two little-endian int16: 1, 2

  const frame = encodeNodeTransitionFrame(
    {
      runId: '0123456789abcdef0123456789abcdef',
      cycleIndex: 1, seq: 3, nodeId: 'cam', port: 'frame', durationMicros: 500,
      hasOutput: true, faulted: false, outputFrameBytes: 4, hasPayload: true,
    },
    descriptor,
    payload,
  );

  const decoded = decodeRecord(body(frame)) as NodeTransitionRecord;
  assert.equal(decoded.hasPayload, true);
  assert.equal(decoded.payloadInfo?.mediaType, PayloadMediaType.Image);
  assert.equal(decoded.payloadInfo?.elementType, PayloadElementType.Int16);
  assert.deepEqual(decoded.payloadInfo?.shape, [2]);
  assert.deepEqual([...decoded.payload!], [1, 0, 2, 0]);

  const typed = toTypedArray(decoded) as Int16Array;
  assert.ok(typed instanceof Int16Array);
  assert.deepEqual([...typed], [1, 2]);
});

test('FrameSplitter reassembles records split across chunks', () => {
  const a = encodeCycle({
    runId: '0123456789abcdef0123456789abcdef',
    cycleIndex: 0, seq: 0, totalCycles: 1, acceptedCycles: 1, accepted: true, elapsedMillis: 10,
  });
  const b = encodeNodeTransition({
    runId: '0123456789abcdef0123456789abcdef',
    cycleIndex: 0, seq: 1, nodeId: 'n', port: 'p', durationMicros: 1,
    hasOutput: false, faulted: true, outputFrameBytes: -1, hasPayload: false,
  });

  const stream = new Uint8Array(a.length + b.length);
  stream.set(a);
  stream.set(b, a.length);

  const splitter = new FrameSplitter();
  const bodies: Uint8Array[] = [];
  // feed one byte at a time — the worst-case fragmentation
  for (let i = 0; i < stream.length; i++) {
    bodies.push(...splitter.push(stream.subarray(i, i + 1)));
  }

  assert.equal(bodies.length, 2);
  assert.equal(decodeRecord(bodies[0]).kind, EgressStreamKind.Cycle);
  const second = decodeRecord(bodies[1]) as NodeTransitionRecord;
  assert.equal(second.kind, EgressStreamKind.NodeTransition);
  assert.equal(second.faulted, true);
  assert.equal(second.outputFrameBytes, -1);
});
