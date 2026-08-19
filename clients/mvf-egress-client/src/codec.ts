// Isomorphic (browser + Node) codec for the MachineVisionFabric realtime-egress wire format.
// Mirrors src/egress/Mvf.Egress/EgressWire.cs — keep the two in lockstep.
// Pure DataView / TypedArray: no Node built-ins, so this file runs unchanged in a browser.

export const EGRESS_MAGIC = 0x4745; // 'EG'

/**
 * The version this codec emits. v2 added the topology record.
 *
 * Adding a stream kind is not backwards compatible in the direction that matters: a v1 decoder reads an
 * unknown kind as a Cycle record and reports confident nonsense, so the version moves with it.
 */
export const EGRESS_VERSION = 2;

/** The oldest producer this decoder accepts — a v1 engine simply never sends topology. */
export const EGRESS_MIN_VERSION = 1;

// PayloadDescriptor header (mirrors Mvf.Abstractions.PayloadDescriptor): fixed 192-byte little-endian.
const DESCRIPTOR_SIZE = 192;
const DESC_MEDIA_TYPE = 12; // u16
const DESC_ELEMENT_TYPE = 14; // u8
const DESC_RANK = 15; // u8
const DESC_SHAPE = 24; // i64[8]

/** DLPack/numpy-compatible element (scalar) type — matches Mvf.Abstractions.PayloadElementType. */
export const PayloadElementType = {
  UInt8: 0, Int8: 1, UInt16: 2, Int16: 3, UInt32: 4, Int32: 5,
  UInt64: 6, Int64: 7, Float16: 8, BFloat16: 9, Float32: 10, Float64: 11,
} as const;
export type PayloadElementType = (typeof PayloadElementType)[keyof typeof PayloadElementType];

/** Semantic payload tag — matches Mvf.Abstractions.PayloadMediaType. */
export const PayloadMediaType = {
  Blob: 0, Tensor: 1, Image: 2, Json: 3,
} as const;
export type PayloadMediaType = (typeof PayloadMediaType)[keyof typeof PayloadMediaType];

export interface PayloadInfo {
  mediaType: number;
  elementType: number;
  shape: number[];
}

// A const object rather than a TS `enum`: enums need runtime code generation, which Node's type-only
// stripping (and `verbatimModuleSyntax`) reject. This form is fully erasable yet still gives a value
// (`EgressStreamKind.Cycle`) and a type (`EgressStreamKind`).
export const EgressStreamKind = {
  Cycle: 0,
  NodeTransition: 1,
  /** The graph's shape, sent once per run and replayed to each new subscriber (wire v2+). */
  Topology: 2,
} as const;

export type EgressStreamKind = (typeof EgressStreamKind)[keyof typeof EgressStreamKind];

export interface CycleRecord {
  kind: typeof EgressStreamKind.Cycle;
  runId: string;
  cycleIndex: number;
  seq: number;
  totalCycles: number;
  acceptedCycles: number;
  accepted: boolean;
  elapsedMillis: number;
}

export interface NodeTransitionRecord {
  kind: typeof EgressStreamKind.NodeTransition;
  runId: string;
  cycleIndex: number;
  seq: number;
  nodeId: string;
  port: string;
  durationMicros: number;
  hasOutput: boolean;
  faulted: boolean;
  /** Bytes of the largest frame the node emitted this cycle, or -1 when it emitted none. */
  outputFrameBytes: number;
  hasPayload: boolean;
  /** The typed descriptor (media/element type + shape) when `hasPayload`, else undefined. */
  payloadInfo?: PayloadInfo;
  /** The raw frame bytes when `hasPayload`, else undefined. Wrap with {@link toTypedArray}. */
  payload?: Uint8Array;
}

/** A port on a topology node; the data/control split is structure, so it travels with it. */
export interface TopologyPort {
  name: string;
  /** 'data' | 'control'. */
  channel: string;
  dataType: string;
}

export interface TopologyNode {
  id: string;
  displayName: string;
  /** integration-module | embedded-primitive | runtime-builtin. */
  kind: string;
  /** source | compute | classify | flow | sink | value. */
  category: string;
  moduleId?: string;
  primitiveType?: string;
  builtinType?: string;
  inputs: TopologyPort[];
  outputs: TopologyPort[];
}

export interface TopologyEdge {
  id: string;
  /** 'data' | 'control'. */
  kind: string;
  fromNode: string;
  fromPort: string;
  toNode: string;
  toPort: string;
}

/**
 * The running graph's shape. Structure only — node config never travels, because this stream is announced
 * on the LAN and served to whoever attaches.
 */
export interface TopologyRecord {
  kind: typeof EgressStreamKind.Topology;
  runId: string;
  cycleIndex: number;
  seq: number;
  name: string;
  nodes: TopologyNode[];
  edges: TopologyEdge[];
}

export type EgressRecord = CycleRecord | NodeTransitionRecord | TopologyRecord;

const textDecoder = new TextDecoder();
const textEncoder = new TextEncoder();

/** Decodes one record body (the bytes after the u32 length prefix). */
export function decodeRecord(body: Uint8Array): EgressRecord {
  const view = new DataView(body.buffer, body.byteOffset, body.byteLength);
  let o = 0;

  const magic = view.getUint16(o, true);
  o += 2;
  if (magic !== EGRESS_MAGIC) {
    throw new Error(`Bad egress magic 0x${magic.toString(16)} (expected 0x${EGRESS_MAGIC.toString(16)}).`);
  }

  const version = view.getUint8(o);
  o += 1;
  if (version < EGRESS_MIN_VERSION || version > EGRESS_VERSION) {
    throw new Error(
      `Unsupported egress version ${version} (this build reads ${EGRESS_MIN_VERSION}..${EGRESS_VERSION}).`,
    );
  }

  const kind = view.getUint8(o) as EgressStreamKind;
  o += 1;
  const runId = formatGuidN(body, o);
  o += 16;
  const cycleIndex = view.getUint32(o, true);
  o += 4;
  const seq = view.getUint32(o, true);
  o += 4;

  if (kind === EgressStreamKind.Topology) {
    const name = readString();

    const nodeCount = view.getUint32(o, true);
    o += 4;
    const nodes: TopologyNode[] = [];
    for (let i = 0; i < nodeCount; i++) {
      const id = readString();
      const displayName = readString();
      const nodeKind = readString();
      const category = readString();
      const moduleId = readString();
      const primitiveType = readString();
      const builtinType = readString();
      nodes.push({
        id,
        displayName,
        kind: nodeKind,
        category,
        // The three type slots are mutually exclusive; empty means "not this kind", not "".
        moduleId: moduleId || undefined,
        primitiveType: primitiveType || undefined,
        builtinType: builtinType || undefined,
        inputs: readPorts(),
        outputs: readPorts(),
      });
    }

    const edgeCount = view.getUint32(o, true);
    o += 4;
    const edges: TopologyEdge[] = [];
    for (let i = 0; i < edgeCount; i++) {
      edges.push({
        id: readString(),
        kind: readString(),
        fromNode: readString(),
        fromPort: readString(),
        toNode: readString(),
        toPort: readString(),
      });
    }

    return { kind: EgressStreamKind.Topology, runId, cycleIndex, seq, name, nodes, edges };
  }

  if (kind === EgressStreamKind.NodeTransition) {
    const nodeId = readString();
    const port = readString();
    const durationMicros = Number(view.getBigInt64(o, true));
    o += 8;
    const flags = view.getUint8(o);
    o += 1;
    const outputFrameBytes = Number(view.getBigInt64(o, true));
    o += 8;
    const hasPayload = view.getUint8(o) !== 0;
    o += 1;

    let payloadInfo: PayloadInfo | undefined;
    let payload: Uint8Array | undefined;
    if (hasPayload) {
      const descriptor = body.subarray(o, o + DESCRIPTOR_SIZE);
      o += DESCRIPTOR_SIZE;
      payloadInfo = parseDescriptor(descriptor);
      payload = body.subarray(o); // rest of body
      o += payload.length;
    }

    return {
      kind: EgressStreamKind.NodeTransition,
      runId,
      cycleIndex,
      seq,
      nodeId,
      port,
      durationMicros,
      hasOutput: (flags & 0x01) !== 0,
      faulted: (flags & 0x02) !== 0,
      outputFrameBytes,
      hasPayload,
      payloadInfo,
      payload,
    };
  }

  const totalCycles = view.getUint32(o, true);
  o += 4;
  const acceptedCycles = view.getUint32(o, true);
  o += 4;
  const accepted = view.getUint8(o) !== 0;
  o += 1;
  const elapsedMillis = Number(view.getBigInt64(o, true));
  o += 8;
  return {
    kind: EgressStreamKind.Cycle,
    runId,
    cycleIndex,
    seq,
    totalCycles,
    acceptedCycles,
    accepted,
    elapsedMillis,
  };

  function readString(): string {
    const length = view.getUint16(o, true);
    o += 2;
    const slice = body.subarray(o, o + length);
    o += length;
    return textDecoder.decode(slice);
  }

  function readPorts(): TopologyPort[] {
    const count = view.getUint32(o, true);
    o += 4;
    const ports: TopologyPort[] = [];
    for (let i = 0; i < count; i++) {
      ports.push({ name: readString(), channel: readString(), dataType: readString() });
    }
    return ports;
  }
}

// ---- encoding (symmetry + tests; the browser SDK only ever decodes) ----------------------------

export function encodeCycle(record: Omit<CycleRecord, 'kind'>): Uint8Array {
  const w = new Writer();
  writeHeader(w, EgressStreamKind.Cycle, record.runId, record.cycleIndex, record.seq);
  w.u32(record.totalCycles);
  w.u32(record.acceptedCycles);
  w.u8(record.accepted ? 1 : 0);
  w.i64(BigInt(Math.trunc(record.elapsedMillis)));
  return w.frame();
}

/** Encoder for the topology record — symmetry with the decoder, and what the round-trip test drives. */
export function encodeTopology(record: Omit<TopologyRecord, 'kind'>): Uint8Array {
  const w = new Writer();
  writeHeader(w, EgressStreamKind.Topology, record.runId, record.cycleIndex, record.seq);
  w.str(record.name);

  w.u32(record.nodes.length);
  for (const n of record.nodes) {
    w.str(n.id);
    w.str(n.displayName);
    w.str(n.kind);
    w.str(n.category);
    w.str(n.moduleId ?? '');
    w.str(n.primitiveType ?? '');
    w.str(n.builtinType ?? '');
    writePorts(w, n.inputs);
    writePorts(w, n.outputs);
  }

  w.u32(record.edges.length);
  for (const e of record.edges) {
    w.str(e.id);
    w.str(e.kind);
    w.str(e.fromNode);
    w.str(e.fromPort);
    w.str(e.toNode);
    w.str(e.toPort);
  }

  return w.frame();
}

function writePorts(w: Writer, ports: TopologyPort[]): void {
  w.u32(ports.length);
  for (const p of ports) {
    w.str(p.name);
    w.str(p.channel);
    w.str(p.dataType);
  }
}

export function encodeNodeTransition(record: Omit<NodeTransitionRecord, 'kind'>): Uint8Array {
  const w = new Writer();
  writeHeader(w, EgressStreamKind.NodeTransition, record.runId, record.cycleIndex, record.seq);
  w.str(record.nodeId);
  w.str(record.port);
  w.i64(BigInt(Math.trunc(record.durationMicros)));
  let flags = 0;
  if (record.hasOutput) flags |= 0x01;
  if (record.faulted) flags |= 0x02;
  w.u8(flags);
  w.i64(BigInt(Math.trunc(record.outputFrameBytes)));
  w.u8(record.hasPayload ? 1 : 0);
  return w.frame();
}

export function encodeNodeTransitionFrame(
  record: Omit<NodeTransitionRecord, 'kind' | 'payloadInfo' | 'payload'>,
  descriptor: Uint8Array,
  payload: Uint8Array,
): Uint8Array {
  const w = new Writer();
  writeHeader(w, EgressStreamKind.NodeTransition, record.runId, record.cycleIndex, record.seq);
  w.str(record.nodeId);
  w.str(record.port);
  w.i64(BigInt(Math.trunc(record.durationMicros)));
  let flags = 0;
  if (record.hasOutput) flags |= 0x01;
  if (record.faulted) flags |= 0x02;
  w.u8(flags);
  w.i64(BigInt(Math.trunc(record.outputFrameBytes)));
  w.u8(1);
  w.bytes(descriptor);
  w.bytes(payload);
  return w.frame();
}

/** Builds a 192-byte PayloadDescriptor header (media/element type + shape). */
export function buildDescriptor(mediaType: number, elementType: number, shape: number[]): Uint8Array {
  const desc = new Uint8Array(DESCRIPTOR_SIZE);
  const view = new DataView(desc.buffer);
  view.setUint16(DESC_MEDIA_TYPE, mediaType, true);
  view.setUint8(DESC_ELEMENT_TYPE, elementType);
  view.setUint8(DESC_RANK, shape.length);
  for (let i = 0; i < shape.length; i++) {
    view.setBigInt64(DESC_SHAPE + i * 8, BigInt(shape[i]), true);
  }
  return desc;
}

/** Wraps a frame-data record's raw bytes as the native typed array its element type implies. */
export function toTypedArray(record: NodeTransitionRecord): ArrayBufferView | undefined {
  if (!record.payload || !record.payloadInfo) {
    return undefined;
  }

  // Copy to a tightly-owned buffer so the typed-array view is correctly aligned and sized.
  const buffer = record.payload.slice().buffer;
  switch (record.payloadInfo.elementType) {
    case PayloadElementType.Int8: return new Int8Array(buffer);
    case PayloadElementType.UInt16: return new Uint16Array(buffer);
    case PayloadElementType.Int16: return new Int16Array(buffer);
    case PayloadElementType.UInt32: return new Uint32Array(buffer);
    case PayloadElementType.Int32: return new Int32Array(buffer);
    case PayloadElementType.UInt64: return new BigUint64Array(buffer);
    case PayloadElementType.Int64: return new BigInt64Array(buffer);
    case PayloadElementType.Float32: return new Float32Array(buffer);
    case PayloadElementType.Float64: return new Float64Array(buffer);
    default: return new Uint8Array(buffer); // UInt8, and Float16/BFloat16 which have no native view
  }
}

function parseDescriptor(descriptor: Uint8Array): PayloadInfo {
  const view = new DataView(descriptor.buffer, descriptor.byteOffset, descriptor.byteLength);
  const mediaType = view.getUint16(DESC_MEDIA_TYPE, true);
  const elementType = view.getUint8(DESC_ELEMENT_TYPE);
  const rank = view.getUint8(DESC_RANK);
  const shape: number[] = [];
  for (let i = 0; i < rank; i++) {
    shape.push(Number(view.getBigInt64(DESC_SHAPE + i * 8, true)));
  }

  return { mediaType, elementType, shape };
}

function writeHeader(w: Writer, kind: EgressStreamKind, runId: string, cycleIndex: number, seq: number): void {
  w.u16(EGRESS_MAGIC);
  w.u8(EGRESS_VERSION);
  w.u8(kind);
  w.bytes(parseGuidN(runId));
  w.u32(cycleIndex);
  w.u32(seq);
}

// ---- GUID <-> 16 bytes, matching .NET Guid.ToByteArray / ToString("N") -------------------------
// .NET serializes the first three fields little-endian and the trailing 8 bytes as-is.

function formatGuidN(bytes: Uint8Array, offset: number): string {
  const view = new DataView(bytes.buffer, bytes.byteOffset + offset, 16);
  const d1 = view.getUint32(0, true).toString(16).padStart(8, '0');
  const d2 = view.getUint16(4, true).toString(16).padStart(4, '0');
  const d3 = view.getUint16(6, true).toString(16).padStart(4, '0');
  let tail = '';
  for (let i = 8; i < 16; i++) {
    tail += bytes[offset + i].toString(16).padStart(2, '0');
  }
  return d1 + d2 + d3 + tail;
}

function parseGuidN(guid: string): Uint8Array {
  const s = guid.replace(/-/g, '');
  const bytes = new Uint8Array(16);
  const view = new DataView(bytes.buffer);
  view.setUint32(0, parseInt(s.slice(0, 8), 16), true);
  view.setUint16(4, parseInt(s.slice(8, 12), 16), true);
  view.setUint16(6, parseInt(s.slice(12, 16), 16), true);
  for (let i = 0; i < 8; i++) {
    bytes[8 + i] = parseInt(s.slice(16 + i * 2, 18 + i * 2), 16);
  }
  return bytes;
}

class Writer {
  private parts: number[] = [];

  u8(v: number): void {
    this.parts.push(v & 0xff);
  }

  u16(v: number): void {
    this.parts.push(v & 0xff, (v >>> 8) & 0xff);
  }

  u32(v: number): void {
    this.parts.push(v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff);
  }

  i64(v: bigint): void {
    let x = BigInt.asUintN(64, v);
    for (let i = 0; i < 8; i++) {
      this.parts.push(Number(x & 0xffn));
      x >>= 8n;
    }
  }

  bytes(b: Uint8Array): void {
    for (const byte of b) this.parts.push(byte);
  }

  str(value: string): void {
    const encoded = textEncoder.encode(value);
    this.u16(encoded.length);
    this.bytes(encoded);
  }

  frame(): Uint8Array {
    const body = Uint8Array.from(this.parts);
    const out = new Uint8Array(4 + body.length);
    new DataView(out.buffer).setUint32(0, body.length, true);
    out.set(body, 4);
    return out;
  }
}
