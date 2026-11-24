// Finds beep onsets in the test media's audio: a 440 Hz tone that switches to 1760 Hz for a short
// beep at the start of every second. The onset is where zero crossings suddenly come four times
// closer together. Accurate to well under a millisecond.

/** Sample indices where a beep starts. */
export function beepOnsets(samples: Float32Array, rate: number): number[] {
  const crossings: number[] = [];
  for (let i = 1; i < samples.length; i++) {
    const a = samples[i - 1] ?? 0;
    const b = samples[i] ?? 0;
    if ((a < 0 && b >= 0) || (a >= 0 && b < 0)) crossings.push(i);
  }
  // Half-periods: 440 Hz ≈ 54.5 samples at 48 kHz, 1760 Hz ≈ 13.6. Split between them.
  const split = rate / 440 / 2 / 2;
  const onsets: number[] = [];
  for (let k = 2; k < crossings.length; k++) {
    const before = (crossings[k - 1] ?? 0) - (crossings[k - 2] ?? 0);
    const after = (crossings[k] ?? 0) - (crossings[k - 1] ?? 0);
    if (before > split && after < split) onsets.push(crossings[k - 1] ?? 0);
  }
  return onsets;
}
