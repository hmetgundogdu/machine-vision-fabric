// Node-only TCP stream client. Connects to an edge's egress TCP server and yields decoded records.
// (The browser entry — WebSocket — arrives in a later slice; the codec/framing above are shared.)

import net from 'node:net';
import { on } from 'node:events';
import { decodeRecord, type EgressRecord } from './codec.ts';
import { FrameSplitter } from './framing.ts';

export interface StreamOptions {
  host?: string;
  port: number;
  /** Abort to stop streaming and close the socket. */
  signal?: AbortSignal;
}

/**
 * Streams decoded egress records off a TCP connection as an async iterable:
 *
 * ```ts
 * for await (const record of streamRecords({ port: 8791 })) { ... }
 * ```
 *
 * The iterator ends when the server closes the connection, on socket error, or when `signal` aborts.
 */
export async function* streamRecords(options: StreamOptions): AsyncGenerator<EgressRecord> {
  const closed = new AbortController();
  const signal = options.signal
    ? AbortSignal.any([options.signal, closed.signal])
    : closed.signal;

  const socket = net.connect({ host: options.host ?? '127.0.0.1', port: options.port });
  socket.on('close', () => closed.abort());
  socket.on('error', () => closed.abort());

  const splitter = new FrameSplitter();
  try {
    for await (const [chunk] of on(socket, 'data', { signal })) {
      for (const body of splitter.push(chunk as Uint8Array)) {
        yield decodeRecord(body);
      }
    }
  } catch (error) {
    if (!(error instanceof Error) || error.name !== 'AbortError') {
      throw error;
    }
  } finally {
    socket.destroy();
  }
}
