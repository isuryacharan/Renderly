// Filmstrip thumbnails for timeline clips. Requests come only from what the timeline shows,
// nearest the centre first; the queue is replaced on every redraw, so tiles scrolled out of view
// are dropped before they are decoded. Each tile is the keyframe at or before its time, decoded
// by the same decode worker as playback and scaled down at once; the VideoFrame is closed as soon
// as it is drawn. Bitmaps are kept in an LRU and closed when evicted.
//
// Keys sit on a power-of-two time grid (`level`), so they stay the same across zoom levels, and a
// coarser tile already in the cache stands in while a finer one loads.

import { testSeekDelayMs } from "@renderly/engine";
import { createDecodeClient, type DecodeClient } from "@renderly/media";

export interface ThumbRequest {
  key: string;
  media: string;
  /** Source time, in flicks. */
  time: number;
  /** Distance from the centre of the view: lower is sooner. */
  priority: number;
  width: number;
  height: number;
  rotation: 0 | 90 | 180 | 270;
}

interface Decoder {
  client: DecodeClient;
  media: string | null;
  busy: boolean;
}

const CACHE_MAX = 600;
const QUEUE_MAX = 200;
const DECODERS = 2;

export const thumbKey = (media: string, level: number, k: number) => `${media}|${level}|${k}`;

export class Thumbnails {
  private readonly cache = new Map<string, ImageBitmap>();
  private readonly failed = new Set<string>();
  private readonly inFlight = new Set<string>();
  private queue: ThumbRequest[] = [];
  private readonly decoders: Decoder[] = [];
  private notifyPending = false;
  private closed = false;

  constructor(
    /** The file to read for a media item, or null when it is offline or still being checked. */
    private readonly file: (media: string) => File | null,
    private readonly onUpdate: () => void,
  ) {}

  get size(): number {
    return this.cache.size;
  }

  get pending(): number {
    return this.queue.length + this.inFlight.size;
  }

  /** A cached tile, bumped in the LRU. */
  get(key: string): ImageBitmap | undefined {
    const bitmap = this.cache.get(key);
    if (bitmap) {
      this.cache.delete(key);
      this.cache.set(key, bitmap);
    }
    return bitmap;
  }

  /** Replaces the queue with what is on screen now. */
  want(requests: ThumbRequest[]): void {
    if (this.closed) return;
    this.queue = requests
      .filter(
        (r) =>
          !this.cache.has(r.key) &&
          !this.inFlight.has(r.key) &&
          !this.failed.has(r.key) &&
          this.file(r.media) !== null,
      )
      .sort((a, b) => a.priority - b.priority)
      .slice(0, QUEUE_MAX);
    this.pump();
  }

  close(): void {
    this.closed = true;
    this.queue = [];
    for (const decoder of this.decoders) decoder.client.terminate();
    for (const bitmap of this.cache.values()) bitmap.close();
    this.cache.clear();
  }

  private pump(): void {
    while (this.queue.length > 0) {
      const request = this.queue[0];
      if (!request) return;
      const decoder =
        this.decoders.find((d) => !d.busy && d.media === request.media) ??
        this.decoders.find((d) => !d.busy) ??
        this.spawn();
      if (!decoder) return;
      this.queue.shift();
      void this.run(decoder, request);
    }
  }

  private spawn(): Decoder | undefined {
    if (this.decoders.length >= DECODERS) return undefined;
    const decoder: Decoder = { client: createDecodeClient(), media: null, busy: false };
    const delay = testSeekDelayMs();
    if (delay > 0) decoder.client.setTestSeekDelay(delay);
    this.decoders.push(decoder);
    return decoder;
  }

  private async run(decoder: Decoder, request: ThumbRequest): Promise<void> {
    decoder.busy = true;
    this.inFlight.add(request.key);
    try {
      if (decoder.media !== request.media) {
        const file = this.file(request.media);
        if (!file) throw new Error("offline");
        if (decoder.media !== null) await decoder.client.close();
        decoder.media = null;
        const opened = await decoder.client.open(file, 2);
        if (!opened.ok) throw new Error(opened.reason);
        decoder.media = request.media;
      }
      const frame = await decoder.client.seek(request.time, "keyframe");
      if (this.closed) {
        frame?.release();
        return;
      }
      if (!frame) throw new Error("no frame");
      let bitmap: ImageBitmap;
      try {
        bitmap = draw(frame.frame, request);
      } finally {
        frame.release();
      }
      this.put(request.key, bitmap);
    } catch {
      if (!this.closed) this.failed.add(request.key);
      if (decoder.media !== request.media) decoder.media = null;
    } finally {
      this.inFlight.delete(request.key);
      decoder.busy = false;
      if (!this.closed) this.pump();
    }
  }

  private put(key: string, bitmap: ImageBitmap): void {
    this.cache.get(key)?.close();
    this.cache.delete(key);
    this.cache.set(key, bitmap);
    while (this.cache.size > CACHE_MAX) {
      const oldest = this.cache.entries().next().value;
      if (!oldest) break;
      oldest[1].close();
      this.cache.delete(oldest[0]);
    }
    if (this.notifyPending) return;
    this.notifyPending = true;
    requestAnimationFrame(() => {
      this.notifyPending = false;
      this.onUpdate();
    });
  }
}

/** Scales (and turns upright) a decoded frame into a small bitmap. */
function draw(frame: VideoFrame, request: ThumbRequest): ImageBitmap {
  const { width, height, rotation } = request;
  const canvas = new OffscreenCanvas(width, height);
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("no 2d context");
  const sideways = rotation === 90 || rotation === 270;
  ctx.translate(width / 2, height / 2);
  ctx.rotate((rotation * Math.PI) / 180);
  const w = sideways ? height : width;
  const h = sideways ? width : height;
  ctx.drawImage(frame, -w / 2, -h / 2, w, h);
  return canvas.transferToImageBitmap();
}
