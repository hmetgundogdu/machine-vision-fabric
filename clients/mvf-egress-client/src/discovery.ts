// Node-only edge discovery: listen on the UDP multicast group for "alive" beacons and keep a live,
// self-expiring list of streaming edges. (A browser cannot receive UDP multicast — a browser viewer
// discovers via a known WS URL instead.)

import dgram from 'node:dgram';

export const DISCOVERY_GROUP = '239.255.7.71';
export const DISCOVERY_PORT = 8790;
const BEACON_MARKER = 'egress-beacon';

export interface EdgeBeacon {
  edgeId: string;
  pipeline: string;
  transport: string;
  port: number;
  streams: string;
  status: string;
}

/** Parses a beacon datagram; returns null for anything that is not one of ours. Isomorphic (pure JSON). */
export function parseBeacon(data: Uint8Array): EdgeBeacon | null {
  try {
    const obj = JSON.parse(new TextDecoder().decode(data));
    if (!obj || obj.mvf !== BEACON_MARKER) {
      return null;
    }

    return {
      edgeId: String(obj.edgeId ?? ''),
      pipeline: String(obj.pipeline ?? ''),
      transport: String(obj.transport ?? 'tcp'),
      port: Number(obj.port ?? 0),
      streams: String(obj.streams ?? 'state'),
      status: String(obj.status ?? 'running'),
    };
  } catch {
    return null;
  }
}

/** A live, self-expiring registry of discovered edges, keyed by edgeId+port. */
export class EdgeRegistry {
  private readonly edges = new Map<string, { beacon: EdgeBeacon; lastSeen: number }>();

  apply(beacon: EdgeBeacon, now: number = Date.now()): void {
    this.edges.set(`${beacon.edgeId}:${beacon.port}`, { beacon, lastSeen: now });
  }

  /** The edges whose beacons are still fresh; anything older than `ttlMs` is dropped ("alive" semantics). */
  live(ttlMs = 3000, now: number = Date.now()): EdgeBeacon[] {
    const alive: EdgeBeacon[] = [];
    for (const [key, entry] of this.edges) {
      if (now - entry.lastSeen > ttlMs) {
        this.edges.delete(key);
      } else {
        alive.push(entry.beacon);
      }
    }

    return alive;
  }
}

export interface DiscoveryOptions {
  signal?: AbortSignal;
}

/** Streams beacons as they arrive on the multicast group (Node only). */
export async function* discoverEdges(options: DiscoveryOptions = {}): AsyncGenerator<EdgeBeacon> {
  const socket = dgram.createSocket({ type: 'udp4', reuseAddr: true });

  const queue: EdgeBeacon[] = [];
  let wake: (() => void) | null = null;
  let done = false;

  const notify = (): void => {
    if (wake) {
      const resume = wake;
      wake = null;
      resume();
    }
  };

  socket.on('message', (msg: Buffer) => {
    const beacon = parseBeacon(msg);
    if (beacon) {
      queue.push(beacon);
      notify();
    }
  });
  socket.on('error', () => {
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

  await new Promise<void>((resolve, reject) => {
    socket.bind(DISCOVERY_PORT, () => {
      try {
        socket.addMembership(DISCOVERY_GROUP);
        resolve();
      } catch (error) {
        reject(error);
      }
    });
  });

  try {
    while (true) {
      while (queue.length > 0) {
        yield queue.shift()!;
      }

      if (done) {
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
