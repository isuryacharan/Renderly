// Time in the project model. Every time is an integer number of flicks (1/705,600,000 s), which
// divides evenly into frames at every supported rate and into samples at the usual audio rates,
// so nothing drifts. Rates are exact rationals [numerator, denominator] frames per second.

export const FLICKS_PER_SECOND = 705_600_000;

/** Integer flicks. */
export type Flicks = number;

/** Frames per second as an exact fraction, e.g. [30, 1] or [30000, 1001] for 29.97. */
export type Rate = readonly [number, number];

const WHOLE = [24, 25, 30, 50, 60, 90, 100, 120];

/** Every frame rate a sequence or source may have: whole rates and their NTSC (÷1.001) variants. */
export const SUPPORTED_RATES: readonly Rate[] = [
  ...WHOLE.map((fps): Rate => [fps, 1]),
  ...WHOLE.map((fps): Rate => [fps * 1000, 1001]),
];

/** Audio sample rates a sequence may have; each sample is a whole number of flicks. */
export const SUPPORTED_SAMPLE_RATES = [32_000, 44_100, 48_000, 88_200, 96_000] as const;

/**
 * Frame rates a source file may have: every sequence rate, plus the slower and cinema rates that
 * cameras, screen recorders and animation use. Each frame is still a whole number of flicks.
 */
export const MEDIA_RATES: readonly Rate[] = [
  ...SUPPORTED_RATES,
  ...[12, 15, 48].flatMap((fps): Rate[] => [
    [fps, 1],
    [fps * 1000, 1001],
  ]),
];

/** Sample rates a source file may have: the sequence rates plus the common lower and higher ones. */
export const MEDIA_SAMPLE_RATES = [
  8_000, 11_025, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000,
] as const;

export function isMediaRate(rate: Rate): boolean {
  return MEDIA_RATES.some(([num, den]) => num * rate[1] === rate[0] * den);
}

/**
 * The media rate nearest a measured one (frames per second), e.g. 29.97003 → 30000/1001. Files
 * with a variable frame rate report an average; this picks the rate they were meant to have.
 */
export function nearestMediaRate(fps: number): Rate {
  let best: Rate = [30, 1];
  let error = Infinity;
  for (const rate of MEDIA_RATES) {
    const e = Math.abs(rate[0] / rate[1] - fps) / fps;
    if (e < error) {
      best = rate;
      error = e;
    }
  }
  return best;
}

export function isSupportedRate(rate: Rate): boolean {
  return SUPPORTED_RATES.some(([num, den]) => num * rate[1] === rate[0] * den);
}

/** Flicks in one frame at `rate`. A whole number for every supported rate. */
export function flicksPerFrame(rate: Rate): Flicks {
  const [num, den] = rate;
  const perFrame = (FLICKS_PER_SECOND * den) / num;
  if (!Number.isInteger(perFrame)) throw new RangeError(`Unsupported frame rate ${num}/${den}`);
  return perFrame;
}

/** The start of frame `frame` (a whole number, may be negative). Exact. */
export function framesToFlicks(frame: number, rate: Rate): Flicks {
  if (!Number.isInteger(frame)) throw new RangeError("Frame must be a whole number");
  return checked(frame * flicksPerFrame(rate));
}

/**
 * Times must be exact integers: about ±147 days fits. Past that a double cannot hold every
 * flick, so refuse instead of silently rounding.
 */
function checked(t: number): Flicks {
  if (!Number.isSafeInteger(t)) throw new RangeError("Time is out of range (about ±147 days)");
  return t;
}

/** The frame showing at time `t`: the last frame starting at or before it. */
export function flicksToFrame(t: Flicks, rate: Rate): number {
  return Math.floor(t / flicksPerFrame(rate));
}

export type SnapMode = "floor" | "round" | "ceil";

/** `t` moved onto the frame grid at `rate`. */
export function snapToFrame(t: Flicks, rate: Rate, mode: SnapMode = "round"): Flicks {
  const perFrame = flicksPerFrame(rate);
  const frames = t / perFrame;
  const snapped =
    mode === "floor"
      ? Math.floor(frames)
      : mode === "ceil"
        ? Math.ceil(frames)
        : Math.round(frames);
  return snapped * perFrame;
}

/** Seconds to flicks, rounded to the nearest flick. For user input; never store seconds. */
export function secondsToFlicks(seconds: number): Flicks {
  return Math.round(seconds * FLICKS_PER_SECOND);
}

export function flicksToSeconds(t: Flicks): number {
  return t / FLICKS_PER_SECOND;
}

/** The start of sample `sample` at `sampleRate`. Exact for the supported sample rates. */
export function samplesToFlicks(sample: number, sampleRate: number): Flicks {
  const perSample = FLICKS_PER_SECOND / sampleRate;
  if (!Number.isInteger(perSample)) throw new RangeError(`Unsupported sample rate ${sampleRate}`);
  if (!Number.isInteger(sample)) throw new RangeError("Sample must be a whole number");
  return checked(sample * perSample);
}

/** The sample playing at time `t`. */
export function flicksToSample(t: Flicks, sampleRate: number): number {
  // Divide by the whole number of flicks per sample; multiplying first would overflow.
  const perSample = FLICKS_PER_SECOND / sampleRate;
  if (!Number.isInteger(perSample)) throw new RangeError(`Unsupported sample rate ${sampleRate}`);
  return Math.floor(t / perSample);
}

/** The frame at rate `to` showing at the start of frame `frame` at rate `from`. */
export function convertFrame(frame: number, from: Rate, to: Rate): number {
  return flicksToFrame(framesToFlicks(frame, from), to);
}

/** Nominal whole frames per second for timecode: 30 for 29.97, 24 for 23.976. */
function timecodeBase(rate: Rate): number {
  return Math.round(rate[0] / rate[1]);
}

/** 29.97 and 59.94 use drop-frame timecode, so timecode stays in step with the clock. */
export function isDropFrame(rate: Rate): boolean {
  return rate[1] === 1001 && (rate[0] === 30000 || rate[0] === 60000);
}

const pad = (n: number) => String(n).padStart(2, "0");

/**
 * Time `t` as SMPTE timecode at `rate`: HH:MM:SS:FF, or HH:MM:SS;FF in drop-frame for 29.97 and
 * 59.94 (frame numbers 0 and 1, or 0 to 3, are skipped at every minute except each tenth).
 * Negative times get a leading minus sign.
 */
export function formatTimecode(t: Flicks, rate: Rate): string {
  const sign = t < 0 ? "-" : "";
  let frame = Math.abs(flicksToFrame(Math.abs(t) === t ? t : -t, rate));
  const base = timecodeBase(rate);
  const drop = isDropFrame(rate);
  if (drop) {
    // Standard drop-frame conversion: add back the dropped frame numbers.
    const dropped = base === 60 ? 4 : 2;
    const perTenMinutes = base * 60 * 10 - dropped * 9;
    const perMinute = base * 60 - dropped;
    const tens = Math.floor(frame / perTenMinutes);
    const rest = frame % perTenMinutes;
    frame += dropped * 9 * tens;
    if (rest > dropped) frame += dropped * Math.floor((rest - dropped) / perMinute);
  }
  const ff = frame % base;
  const totalSeconds = Math.floor(frame / base);
  const ss = totalSeconds % 60;
  const mm = Math.floor(totalSeconds / 60) % 60;
  const hh = Math.floor(totalSeconds / 3600);
  return `${sign}${pad(hh)}:${pad(mm)}:${pad(ss)}${drop ? ";" : ":"}${pad(ff)}`;
}
