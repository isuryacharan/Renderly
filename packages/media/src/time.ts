// Time at the media boundary. Renderly stores time as integer flicks (1/705,600,000 s); mediabunny
// and WebCodecs use floating-point seconds and microseconds. Conversion happens only here.

export const FLICKS_PER_SECOND = 705_600_000;

/** Integer flicks. Every frame rate in common use is a whole number of flicks per frame. */
export type Flicks = number;

export function secondsToFlicks(seconds: number): Flicks {
  return Math.round(seconds * FLICKS_PER_SECOND);
}

export function flicksToSeconds(flicks: Flicks): number {
  return flicks / FLICKS_PER_SECOND;
}

/**
 * Container timestamps are rational numbers stored as doubles, so a frame that starts exactly at
 * `t` may come back a fraction of a nanosecond late. Lookups ask for "the frame showing at t", so
 * they add this margin. It is far below any real frame interval.
 */
const LOOKUP_MARGIN_SECONDS = 1e-6;

/** Source time in flicks (0 = the first frame) to a container lookup time in seconds. */
export function lookupSeconds(time: Flicks, firstTimestamp: number): number {
  return firstTimestamp + flicksToSeconds(time) + LOOKUP_MARGIN_SECONDS;
}

/** Container timestamp in seconds to source time in flicks (0 = the first frame). */
export function sourceFlicks(timestamp: number, firstTimestamp: number): Flicks {
  return secondsToFlicks(timestamp - firstTimestamp);
}
