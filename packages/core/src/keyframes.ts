// Keyframes: a property's value over a clip's time. Pure; used by the evaluator (preview and
// export), the commands that edit keys, and the trims and splits that must keep motion unchanged.
//
// Keys are { t, v, ease, in?, out? } with `t` in flicks from the clip's start. The span from a key
// to the next is shaped by the first key's `ease`:
// - "hold": the value stays until the next key;
// - "linear": straight from one value to the next;
// - "bezier" and "auto": a cubic curve in *progress space*: time runs 0..1 over the span and the
//   value moves from one key's value to the next by the curve's progress (0..1, overshoot allowed),
//   so numbers, positions and colours ease the same way.
//
// A handle is [time offset in flicks, progress offset] from its key: `out` leaves a key, `in`
// arrives at it. A handle written on a key always wins. Otherwise a "bezier" key has a flat handle
// (an ease) and an "auto" key gets one derived from its neighbours (smooth through the key, like a
// Catmull-Rom curve); on other keys the arriving side is a straight line. Because written handles
// win, an insert can freeze exactly the handles it would otherwise change, and the motion stays
// identical (see `insertKey`).

export type KeyValue = number | number[];
export type Ease = "linear" | "auto" | "bezier" | "hold";

export interface Key {
  t: number;
  v: KeyValue;
  ease: Ease;
  in?: [number, number];
  out?: [number, number];
}

// --- values ---

const asArray = (v: KeyValue): number[] => (typeof v === "number" ? [v] : v);
const like = (template: KeyValue, values: number[]): KeyValue =>
  typeof template === "number" ? (values[0] ?? 0) : values;

/** A value that shares nothing with its source (keys must never share a value's array). */
const copy = (v: KeyValue): KeyValue => (typeof v === "number" ? v : [...v]);

/** a + (b − a) × p, per component. */
export function mix(a: KeyValue, b: KeyValue, p: number): KeyValue {
  const x = asArray(a);
  const y = asArray(b);
  return like(
    a,
    x.map((value, i) => value + ((y[i] ?? value) - value) * p),
  );
}

const dot = (a: number[], b: number[]) => a.reduce((sum, value, i) => sum + value * (b[i] ?? 0), 0);
const sub = (a: KeyValue, b: KeyValue) => {
  const y = asArray(b);
  return asArray(a).map((value, i) => value - (y[i] ?? 0));
};

// --- one span ---

/** Control points of a span's curve in normalised (time, progress) space; P0 = (0,0), P3 = (1,1). */
export interface Curve {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}

const LINEAR: Curve = { x1: 1 / 3, y1: 1 / 3, x2: 2 / 3, y2: 2 / 3 };

/**
 * The smooth ("auto") handle of key `i` on one side, as [time offset, progress offset]. Its slope
 * is the Catmull-Rom tangent (from the neighbours on both sides) projected on the span's change;
 * the first and last keys are flat.
 */
function autoHandle(keys: readonly Key[], i: number, side: "in" | "out"): [number, number] {
  const key = keys[i] as Key;
  const other = keys[side === "out" ? i + 1 : i - 1];
  const before = keys[i - 1];
  const after = keys[i + 1];
  if (!other) return [0, 0];
  const span = Math.abs(other.t - key.t);
  const dt = (side === "out" ? 1 : -1) * (span / 3);
  if (!before || !after) return [dt, 0];
  const tangent = sub(after.v, before.v).map((d) => d / (after.t - before.t)); // value per flick
  const change = side === "out" ? sub(other.v, key.v) : sub(key.v, other.v);
  const length = dot(change, change);
  if (length === 0) return [dt, 0];
  const slope = dot(tangent, change) / length; // progress per flick
  return [dt, slope * dt];
}

/** The curve of the span from key `i` to key `i + 1` (for "bezier" and "auto" spans). */
export function spanCurve(keys: readonly Key[], i: number): Curve {
  const left = keys[i] as Key;
  const right = keys[i + 1] as Key;
  const T = right.t - left.t;
  const out =
    left.out ??
    (left.ease === "auto" ? autoHandle(keys, i, "out") : ([T / 3, 0] as [number, number]));
  const arrive =
    right.in ??
    (right.ease === "auto"
      ? autoHandle(keys, i + 1, "in")
      : right.ease === "bezier"
        ? ([-T / 3, 0] as [number, number])
        : null);
  const clamp = (x: number) => Math.min(1, Math.max(0, x));
  return {
    x1: clamp(out[0] / T),
    y1: out[1],
    x2: arrive ? clamp(1 + arrive[0] / T) : LINEAR.x2,
    y2: arrive ? 1 + arrive[1] : LINEAR.y2,
  };
}

const cubic = (a: number, b: number, c: number, d: number, u: number) => {
  const v = 1 - u;
  return v * v * v * a + 3 * v * v * u * b + 3 * v * u * u * c + u * u * u * d;
};

/** The curve parameter u at which the curve's time is `s` (0..1): Newton steps, then bisection. */
export function solveTime(curve: Curve, s: number): number {
  const x = (u: number) => cubic(0, curve.x1, curve.x2, 1, u);
  let u = s;
  for (let k = 0; k < 8; k++) {
    const dx =
      3 * (1 - u) * (1 - u) * curve.x1 +
      6 * (1 - u) * u * (curve.x2 - curve.x1) +
      3 * u * u * (1 - curve.x2);
    const error = x(u) - s;
    if (Math.abs(error) < 1e-12) return u;
    if (Math.abs(dx) < 1e-9) break;
    u = Math.min(1, Math.max(0, u - error / dx));
  }
  let lo = 0;
  let hi = 1;
  for (let k = 0; k < 60; k++) {
    const mid = (lo + hi) / 2;
    if (x(mid) < s) lo = mid;
    else hi = mid;
  }
  return (lo + hi) / 2;
}

/** Progress (0..1, overshoot allowed) of a curve at normalised time `s`. */
export function curveProgress(curve: Curve, s: number): number {
  return cubic(0, curve.y1, curve.y2, 1, solveTime(curve, s));
}

// --- a property ---

/** Index of the last key at or before `t` (−1 before the first key). Keys are sorted by time. */
function spanAt(keys: readonly Key[], t: number): number {
  let lo = 0;
  let hi = keys.length - 1;
  let found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if ((keys[mid] as Key).t <= t) {
      found = mid;
      lo = mid + 1;
    } else hi = mid - 1;
  }
  return found;
}

/** The value at clip time `t`: the first key's value before it, the last one's after it. */
export function valueAt(keys: readonly Key[], t: number): KeyValue {
  const first = keys[0];
  if (!first) throw new RangeError("No keyframes");
  const i = spanAt(keys, t);
  if (i < 0) return first.v;
  const left = keys[i] as Key;
  const right = keys[i + 1];
  if (!right || t === left.t) return left.v;
  const s = (t - left.t) / (right.t - left.t);
  if (left.ease === "hold") return left.v;
  if (left.ease === "linear") return mix(left.v, right.v, s);
  return mix(left.v, right.v, curveProgress(spanCurve(keys, i), s));
}

// --- editing that keeps the motion ---

const sorted = (keys: readonly Key[]) => [...keys].sort((a, b) => a.t - b.t);

/** Handle times are whole flicks in the stored project (1 flick is about 1.4 ns). */
const whole = (handle: [number, number]): [number, number] => [Math.round(handle[0]), handle[1]];

/** Keys as stored: handle times rounded to whole flicks. */
function stored(keys: Key[]): Key[] {
  for (const key of keys) {
    if (key.in) key.in = whole(key.in);
    if (key.out) key.out = whole(key.out);
  }
  return keys;
}

/** Writes out a key's derived ("auto") handles, so changing its neighbours cannot change them. */
function freeze(keys: Key[], i: number, sides: readonly ("in" | "out")[]): void {
  const key = keys[i];
  if (!key || key.ease !== "auto") return;
  for (const side of sides)
    if (key[side] === undefined && keys[side === "out" ? i + 1 : i - 1])
      key[side] = whole(autoHandle(keys, i, side));
}

/**
 * Adds a key at `t` without changing the motion: inside an eased span the curve is split exactly
 * (de Casteljau), and the handles the new neighbour would change are written out. A key already
 * at `t` is returned unchanged. Before the first or after the last key, the new key takes the
 * held value. Returns new keys; the input is not changed.
 */
export function insertKey(input: readonly Key[], t: number): Key[] {
  const keys = sorted(input).map((k) => ({ ...k }));
  if (keys.some((k) => k.t === t)) return keys;
  const i = spanAt(keys, t);
  const left = keys[i];
  const right = keys[i + 1];
  if (!left || !right) {
    // Outside the keys the value is held: a new key there holds it too. The end key it joins
    // gets its derived handle written out first.
    const end = left ?? right;
    if (!end) return keys;
    const at = left ? i : 0;
    freeze(keys, at, left ? ["in"] : ["out"]);
    const added: Key = { t, v: copy(valueAt(keys, t)), ease: left ? left.ease : "linear" };
    if (left && left.ease === "auto") added.ease = "bezier";
    return stored(sorted([...keys, added]));
  }
  const s = (t - left.t) / (right.t - left.t);
  const v = copy(valueAt(keys, t));
  // The two keys around the new one get a new neighbour: freeze their derived handles first.
  freeze(keys, i, ["in", "out"]);
  freeze(keys, i + 1, ["in", "out"]);
  if (left.ease === "hold" || left.ease === "linear")
    return stored(sorted([...keys, { t, v, ease: left.ease }]));

  // Split the span's curve at s.
  const curve = spanCurve(keys, i);
  const u = solveTime(curve, s);
  const lerp = (a: number, b: number) => a + (b - a) * u;
  const p1 = { x: curve.x1, y: curve.y1 };
  const p2 = { x: curve.x2, y: curve.y2 };
  const a = { x: lerp(0, p1.x), y: lerp(0, p1.y) };
  const b = { x: lerp(p1.x, p2.x), y: lerp(p1.y, p2.y) };
  const c = { x: lerp(p2.x, 1), y: lerp(p2.y, 1) };
  const d = { x: a.x + (b.x - a.x) * u, y: a.y + (b.y - a.y) * u };
  const e = { x: b.x + (c.x - b.x) * u, y: b.y + (c.y - b.y) * u };
  const m = { x: d.x + (e.x - d.x) * u, y: d.y + (e.y - d.y) * u }; // the new key: (s, progress)
  const T = right.t - left.t;
  // Back to handles: time in flicks; progress as a share of each new span's own change.
  const leftShare = m.y; // progress reached at the new key
  const rightShare = 1 - m.y;
  const share = (value: number, total: number) => (Math.abs(total) < 1e-12 ? 0 : value / total);
  left.out = [a.x * T, share(a.y, leftShare)];
  const added: Key = {
    t,
    v,
    ease: "bezier",
    in: [(d.x - m.x) * T, share(d.y - m.y, leftShare)],
    out: [(e.x - m.x) * T, share(e.y - m.y, rightShare)],
  };
  right.in = [(c.x - 1) * T, share(c.y - 1, rightShare)];
  if (left.ease === "auto") left.ease = "bezier";
  return stored(sorted([...keys, added]));
}

/**
 * Moves every key by `by` flicks (negative is earlier) and keeps only what lies from 0 on: a key
 * is added at 0 first (as `insertKey`), so the motion from 0 on is unchanged. Used when a clip's
 * start moves later over its source (trim, split).
 */
export function shiftKeys(input: readonly Key[], by: number): Key[] {
  let keys = sorted(input).map((k) => ({ ...k, t: k.t - by }));
  if (keys.length === 0) return keys;
  if (keys.some((k) => k.t < 0)) {
    keys = insertKey(keys, 0);
    // The key at 0 no longer has a key before it: write out its leaving handle first.
    const zero = keys.findIndex((k) => k.t === 0);
    freeze(keys, zero, ["out"]);
    keys = keys.filter((k) => k.t >= 0);
    const first = keys[0];
    if (first) delete first.in;
  }
  return stored(keys);
}

/** The ease presets of the keyframe menu, applied to the key at index `i`. */
export type EasePreset = "linear" | "ease-in" | "ease-out" | "ease-in-out" | "auto" | "hold";

/**
 * Applies a preset: "ease in" slows the arrival at the key, "ease out" the departure from it,
 * "ease in-out" both; "linear" and "hold" shape the span after the key; "auto" is smooth through
 * it. Returns new keys.
 */
export function applyEase(input: readonly Key[], i: number, preset: EasePreset): Key[] {
  const keys = sorted(input).map((k) => ({ ...k }));
  const key = keys[i];
  if (!key) return keys;
  const before = keys[i - 1];
  const after = keys[i + 1];
  delete key.in;
  delete key.out;
  if (preset === "linear" || preset === "hold" || preset === "auto") {
    key.ease = preset;
    return stored(keys);
  }
  key.ease = "bezier";
  const slowIn = preset === "ease-in" || preset === "ease-in-out";
  const slowOut = preset === "ease-out" || preset === "ease-in-out";
  if (before) key.in = slowIn ? [-(key.t - before.t) / 3, 0] : [-(key.t - before.t) / 3, -1 / 3];
  if (after) key.out = slowOut ? [(after.t - key.t) / 3, 0] : [(after.t - key.t) / 3, 1 / 3];
  // The span arriving here is shaped by the key before: it must ease too, not run straight.
  if (before && before.ease === "linear" && slowIn) {
    before.ease = "bezier";
    before.out = [(key.t - before.t) / 3, 1 / 3];
  }
  return stored(keys);
}
