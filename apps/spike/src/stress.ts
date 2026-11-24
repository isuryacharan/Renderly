// Seeded edit lists for the S0.5 cut tests. All times are whole 60 fps frames.

import type { Cut } from "@renderly/engine";
import { FLICKS_PER_SECOND } from "@renderly/media";

const FRAME = FLICKS_PER_SECOND / 60;

function random(seed: number): () => number {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/**
 * `count` cuts over sources whose lengths (in frames) are given: most are 15-90 frames from a
 * random point, some only 2-5 frames, and some continue the previous cut (a split point in
 * continuous footage, which must play straight through).
 */
export function stressList(sourceFrames: number[], count: number, seed: number): Cut[] {
  const next = random(seed);
  const cuts: Cut[] = [];
  let start = 0;
  for (let i = 0; i < count; i++) {
    const previous = cuts[cuts.length - 1];
    const kind = next();
    let source: number;
    let inFrame: number;
    let length: number;
    if (previous && kind < 0.2) {
      source = previous.source;
      inFrame = previous.out / FRAME;
      length = 15 + Math.floor(next() * 76);
    } else {
      source = Math.floor(next() * sourceFrames.length);
      length = kind < 0.3 ? 2 + Math.floor(next() * 4) : 15 + Math.floor(next() * 76);
      inFrame = Math.floor(next() * ((sourceFrames[source] ?? 0) - length - 1));
    }
    const maxLength = (sourceFrames[source] ?? 0) - inFrame;
    length = Math.max(1, Math.min(length, maxLength));
    cuts.push({ source, in: inFrame * FRAME, out: (inFrame + length) * FRAME, start });
    start += length * FRAME;
  }
  return cuts;
}

/** `count` clips of 2-4 frames, alternating between sources at random points. */
export function shortClipList(sourceFrames: number[], count: number, seed: number): Cut[] {
  const next = random(seed);
  const cuts: Cut[] = [];
  let start = 0;
  for (let i = 0; i < count; i++) {
    const source = i % sourceFrames.length;
    const length = 2 + Math.floor(next() * 3);
    const inFrame = Math.floor(next() * ((sourceFrames[source] ?? 0) - length - 1));
    cuts.push({ source, in: inFrame * FRAME, out: (inFrame + length) * FRAME, start });
    start += length * FRAME;
  }
  return cuts;
}
