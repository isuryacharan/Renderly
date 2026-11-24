// GPU memory accounting for the render worker. Pure: it counts bytes, it does not allocate.
//
// The browser does not report GPU memory, so the renderer accounts for what it holds itself:
// render targets, the upload texture, and decoded frames on loan from the decoders. Targets are
// reserved before they are allocated and refused past the ceiling; frames are measured (their
// count is bounded by the decoders' frames-in-flight limit) and only reported.

export class MemoryBudget {
  private readonly items = new Map<string, number>();
  private total = 0;
  private highest = 0;

  constructor(readonly ceiling: number) {}

  get used(): number {
    return this.total;
  }

  /** The highest `used` seen since the last `resetPeak`. */
  get peak(): number {
    return this.highest;
  }

  /**
   * Reserves `bytes` under `key`, replacing what `key` held. Returns false, and changes nothing,
   * if that would pass the ceiling.
   */
  reserve(key: string, bytes: number): boolean {
    const next = this.total - (this.items.get(key) ?? 0) + bytes;
    if (next > this.ceiling) return false;
    this.set(key, bytes);
    return true;
  }

  /** Records a measured amount under `key`, ceiling or not. */
  set(key: string, bytes: number): void {
    this.total += bytes - (this.items.get(key) ?? 0);
    this.items.set(key, bytes);
    this.highest = Math.max(this.highest, this.total);
  }

  release(key: string): void {
    this.total -= this.items.get(key) ?? 0;
    this.items.delete(key);
  }

  resetPeak(): void {
    this.highest = this.total;
  }
}

/** Bytes of an RGBA half-float render target. */
export const halfFloatTargetBytes = (width: number, height: number) => width * height * 8;
/** Bytes of an 8-bit RGBA texture with a full mipmap chain. */
export const mipmappedTextureBytes = (width: number, height: number) =>
  Math.round((width * height * 4 * 4) / 3);
/** Bytes of a decoded 4:2:0 frame (NV12). */
export const decodedFrameBytes = (width: number, height: number) =>
  Math.round(width * height * 1.5);
