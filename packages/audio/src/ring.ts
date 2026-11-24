// A single-producer, single-consumer ring of planar float audio in a SharedArrayBuffer.
// The audio decode worker writes; the AudioWorklet reads. Neither side ever blocks: the writer
// retries later when the ring is full, the reader outputs what is there.

const HEADER_INTS = 4; // [write index, read index, complete flag, unused]
const WRITE = 0;
const READ = 1;
const COMPLETE = 2;

export class AudioRing {
  private readonly header: Int32Array;
  private readonly planes: Float32Array[];

  /** Bytes needed for a ring of `capacity` frames. One frame is always left empty. */
  static bytes(channels: number, capacity: number): number {
    return HEADER_INTS * 4 + channels * capacity * 4;
  }

  /** Allocates a new shared ring. */
  static create(channels: number, capacity: number): AudioRing {
    return new AudioRing(
      new SharedArrayBuffer(AudioRing.bytes(channels, capacity)),
      channels,
      capacity,
    );
  }

  constructor(
    readonly buffer: SharedArrayBuffer,
    readonly channels: number,
    readonly capacity: number,
  ) {
    if (buffer.byteLength < AudioRing.bytes(channels, capacity))
      throw new RangeError("Buffer too small");
    this.header = new Int32Array(buffer, 0, HEADER_INTS);
    this.planes = Array.from(
      { length: channels },
      (_, c) => new Float32Array(buffer, HEADER_INTS * 4 + c * capacity * 4, capacity),
    );
  }

  /** The writer has written everything it will; an empty ring now means the end, not an underrun. */
  get complete(): boolean {
    return Atomics.load(this.header, COMPLETE) === 1;
  }

  markComplete(): void {
    Atomics.store(this.header, COMPLETE, 1);
  }

  /** Frames waiting to be read. */
  get readable(): number {
    const w = Atomics.load(this.header, WRITE);
    const r = Atomics.load(this.header, READ);
    return (w - r + this.capacity) % this.capacity;
  }

  /** Frames that can be written now. */
  get writable(): number {
    return this.capacity - 1 - this.readable;
  }

  /**
   * Writes up to `frames` frames from `source` (one array per channel, starting at `offset`).
   * Returns how many were written.
   */
  write(source: Float32Array[], offset: number, frames: number): number {
    const n = Math.min(frames, this.writable);
    let w = Atomics.load(this.header, WRITE);
    for (let done = 0; done < n;) {
      const run = Math.min(n - done, this.capacity - w);
      for (let c = 0; c < this.channels; c++) {
        const from = source[c] ?? source[0];
        if (from)
          (this.planes[c] as Float32Array).set(
            from.subarray(offset + done, offset + done + run),
            w,
          );
      }
      done += run;
      w = (w + run) % this.capacity;
    }
    Atomics.store(this.header, WRITE, w);
    return n;
  }

  /** Writes `frames` frames of silence, as far as there is room. Returns how many were written. */
  writeSilence(frames: number): number {
    const n = Math.min(frames, this.writable);
    let w = Atomics.load(this.header, WRITE);
    for (let done = 0; done < n;) {
      const run = Math.min(n - done, this.capacity - w);
      for (const plane of this.planes) plane.fill(0, w, w + run);
      done += run;
      w = (w + run) % this.capacity;
    }
    Atomics.store(this.header, WRITE, w);
    return n;
  }

  /**
   * Reads up to `frames` frames into `target` (one array per output channel, starting at
   * `offset`). A mono ring feeds every output channel. Returns how many were read.
   */
  read(target: Float32Array[], offset: number, frames: number): number {
    const n = Math.min(frames, this.readable);
    let r = Atomics.load(this.header, READ);
    for (let done = 0; done < n;) {
      const run = Math.min(n - done, this.capacity - r);
      for (let c = 0; c < target.length; c++) {
        const plane = this.planes[Math.min(c, this.channels - 1)] as Float32Array;
        (target[c] as Float32Array).set(plane.subarray(r, r + run), offset + done);
      }
      done += run;
      r = (r + run) % this.capacity;
    }
    Atomics.store(this.header, READ, r);
    return n;
  }
}
