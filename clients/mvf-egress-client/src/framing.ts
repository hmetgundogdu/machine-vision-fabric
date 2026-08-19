// Reassembles length-prefixed egress records from an arbitrarily-chunked byte stream.
// Isomorphic: works over a Node socket's 'data' chunks or a browser WebSocket's binary messages.

export class FrameSplitter {
  private buffer = new Uint8Array(0);

  /** Feed a chunk; returns any complete record bodies (length prefix stripped) it now holds. */
  push(chunk: Uint8Array): Uint8Array[] {
    if (this.buffer.length === 0) {
      this.buffer = chunk;
    } else {
      const merged = new Uint8Array(this.buffer.length + chunk.length);
      merged.set(this.buffer);
      merged.set(chunk, this.buffer.length);
      this.buffer = merged;
    }

    const bodies: Uint8Array[] = [];
    while (this.buffer.length >= 4) {
      const length = new DataView(this.buffer.buffer, this.buffer.byteOffset, 4).getUint32(0, true);
      if (this.buffer.length < 4 + length) {
        break;
      }

      bodies.push(this.buffer.slice(4, 4 + length));
      this.buffer = this.buffer.slice(4 + length);
    }

    return bodies;
  }
}
