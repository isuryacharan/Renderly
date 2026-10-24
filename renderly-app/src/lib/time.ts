// Integer-microsecond time (schema v8). Mirrors renderly-core/src/time.rs; the wasm module
// does not export these, so this is the one TS implementation. Every stored project time is an
// integer number of microseconds (a JS `number` holds those exactly for ~285 years). Convert to
// seconds only at display / HTMLMediaElement / WebCodecs boundaries.

export const US_PER_SEC = 1_000_000;

/** Rational frame rate, e.g. `{ num: 30000, den: 1001 }` for 29.97. */
export interface Fps {
  num: number;
  den: number;
}

export const DEFAULT_FPS: Fps = { num: 30, den: 1 };

/** Seconds (float) to integer µs, rounded to the nearest µs (never truncated). */
export function secsToUs(secs: number): number {
  return Math.round(secs * US_PER_SEC);
}

/** µs to seconds, for display and media-element / WebCodecs boundaries only. */
export function usToSecs(us: number): number {
  return us / US_PER_SEC;
}

/** Rate as a float (display and decoder boundaries only). */
export function fpsToNumber(fps: Fps): number {
  return fps.num / fps.den;
}

/** Accept a rational, a legacy float, or nothing; always returns a valid rational. */
export function normalizeFps(fps: Fps | number | null | undefined): Fps {
  if (fps == null) return DEFAULT_FPS;
  if (typeof fps === "number") return fpsFromNumber(fps);
  const num = Math.round(fps.num);
  const den = Math.round(fps.den);
  return num > 0 && den > 0 ? { num, den } : DEFAULT_FPS;
}

const STANDARD_FPS: Fps[] = [
  { num: 24000, den: 1001 },
  { num: 24, den: 1 },
  { num: 25, den: 1 },
  { num: 30000, den: 1001 },
  { num: 30, den: 1 },
  { num: 48, den: 1 },
  { num: 50, den: 1 },
  { num: 60000, den: 1001 },
  { num: 60, den: 1 },
  { num: 120, den: 1 },
];

/** Nearest standard rational within 0.01 fps, else `{ round(fps*1000), 1000 }` (as core `Fps::from_f64`). */
export function fpsFromNumber(value: number): Fps {
  if (!Number.isFinite(value) || value <= 0) return DEFAULT_FPS;
  for (const f of STANDARD_FPS) {
    if (Math.abs(value - f.num / f.den) <= 0.01) return { ...f };
  }
  return { num: Math.max(1, Math.round(value * 1000)), den: 1000 };
}

/** Nominal integer frames per second (30 for 29.97), used for timecode. */
export function nominalFps(fps: Fps): number {
  return Math.max(1, Math.round(fpsToNumber(fps)));
}

/** floor(a / b) for bigint `a` and `b > 0`. */
function floorDivBig(a: bigint, b: bigint): bigint {
  const q = a / b;
  return a % b !== 0n && a < 0n ? q - 1n : q;
}

/**
 * Start of frame `n` in µs: `floor(n * 1_000_000 * den / num)`. `n * 1e6 * den` can exceed
 * 2^53 on long timelines at 1001 dens, so the fast path is used only while the product is an
 * exact integer; everything else goes through BigInt.
 */
export function frameStart(fps: Fps, n: number): number {
  const num = Math.max(1, fps.num);
  const den = Math.max(1, fps.den);
  const product = n * US_PER_SEC * den;
  if (Number.isSafeInteger(product)) {
    let q = Math.floor(product / num);
    const r = product - q * num;
    if (r < 0) q -= 1;
    else if (r >= num) q += 1;
    return q;
  }
  return Number(floorDivBig(BigInt(n) * BigInt(US_PER_SEC) * BigInt(den), BigInt(num)));
}

/** Index of the frame containing `tUs` (the last frame whose start is `<= tUs`). */
export function frameAt(fps: Fps, tUs: number): number {
  const num = Math.max(1, fps.num);
  const den = Math.max(1, fps.den);
  let n = Math.floor((tUs * num) / (US_PER_SEC * den));
  while (frameStart(fps, n + 1) <= tUs) n += 1;
  while (frameStart(fps, n) > tUs) n -= 1;
  return n;
}

/** True when `tUs` is exactly a frame boundary. */
export function isOnFrame(fps: Fps, tUs: number): boolean {
  return frameStart(fps, frameAt(fps, tUs)) === tUs;
}

/** The nearest frame boundary to `tUs` (ties go to the earlier frame), clamped at 0. */
export function snapToFrameUs(fps: Fps, tUs: number): number {
  const t = Math.max(0, Math.round(tUs));
  const n = frameAt(fps, t);
  const lo = frameStart(fps, n);
  const hi = frameStart(fps, n + 1);
  return t - lo <= hi - t ? lo : hi;
}

/** `tUs` moved by `delta` frames (may be negative), on a frame boundary and >= 0. */
export function stepFrames(fps: Fps, tUs: number, delta: number): number {
  return Math.max(0, frameStart(fps, frameAt(fps, Math.max(0, tUs)) + delta));
}

/** Length of the first frame in µs (a sensible minimum clip length). */
export function frameDurationUs(fps: Fps): number {
  return Math.max(1, frameStart(fps, 1));
}

/** Pointer offset in px to µs at `pxPerSec`; rounds, never truncates. */
export function pxToUs(px: number, pxPerSec: number): number {
  return Math.round((px / pxPerSec) * US_PER_SEC);
}

/** µs to a (fractional) pixel distance at `pxPerSec`. */
export function usToPx(us: number, pxPerSec: number): number {
  return (us / US_PER_SEC) * pxPerSec;
}

/** Display form of a rate: "30", "29.97", "59.94". */
export function formatFps(fps: Fps): string {
  const n = fpsToNumber(fps);
  if (Number.isInteger(n)) return String(n);
  return n.toFixed(2).replace(/0+$/, "").replace(/\.$/, "");
}
