// WebSocket stream client. Uses the global `WebSocket`, so it runs in the **browser** and in Node (>=22,
// which ships a global WebSocket). Each binary message is one record body — decode it directly (no framing).

import { decodeRecord, type EgressRecord } from './codec.ts';

export interface WsStreamOptions {
  /** Abort to close the socket and end iteration. */
  signal?: AbortSignal;
}

/**
 * Streams decoded egress records off a WebSocket as an async iterable:
 *
 * ```ts
 * for await (const record of streamRecordsWs('ws://127.0.0.1:8791/')) { ... }
 * ```
 */
export async function* streamRecordsWs(url: string, options: WsStreamOptions = {}): AsyncGenerator<EgressRecord> {
  const socket = new WebSocket(url);
  socket.binaryType = 'arraybuffer';

  const queue: EgressRecord[] = [];
  let wake: (() => void) | null = null;
  let done = false;
  let failure: unknown = null;

  const notify = (): void => {
    if (wake) {
      const resume = wake;
      wake = null;
      resume();
    }
  };

  socket.addEventListener('message', (event: MessageEvent) => {
    const data = event.data;
    let bytes: Uint8Array | null = null;
    if (data instanceof ArrayBuffer) {
      bytes = new Uint8Array(data);
    } else if (ArrayBuffer.isView(data)) {
      bytes = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    }

    if (bytes) {
      try {
        queue.push(decodeRecord(bytes));
      } catch (error) {
        failure = error;
        done = true;
      }
      notify();
    }
  });
  socket.addEventListener('close', () => {
    done = true;
    notify();
  });
  socket.addEventListener('error', () => {
    done = true;
    notify();
  });

  const onAbort = (): void => {
    done = true;
    try {
      socket.close();
    } catch {
      // ignore
    }
    notify();
  };
  options.signal?.addEventListener('abort', onAbort, { once: true });

  try {
    while (true) {
      while (queue.length > 0) {
        yield queue.shift()!;
      }

      if (done) {
        if (failure) {
          throw failure;
        }
        break;
      }

      await new Promise<void>((resolve) => {
        wake = resolve;
      });
    }
  } finally {
    options.signal?.removeEventListener('abort', onAbort);
    try {
      socket.close();
    } catch {
      // ignore
    }
  }
}
