// Estimates the display refresh period from recent intervals between animation frames, and
// says whether an interval was late. The median of a short window ignores both stalls (long
// gaps) and bunched callbacks after a stall (short gaps); a running minimum does not.

const WINDOW = 31;

export class RefreshMeter {
  private readonly intervals: number[] = [];

  /** Records the gap between two consecutive animation frames, in ms. */
  add(interval: number): void {
    if (!(interval > 0)) return;
    this.intervals.push(interval);
    if (this.intervals.length > WINDOW) this.intervals.shift();
  }

  /** The display's refresh period in ms, or null until a few intervals have been seen. */
  get period(): number | null {
    if (this.intervals.length < 3) return null;
    const sorted = [...this.intervals].sort((a, b) => a - b);
    return sorted[Math.floor(sorted.length / 2)] ?? null;
  }

  /** True when `interval` is long enough that at least one refresh was missed. */
  isLate(interval: number): boolean {
    const period = this.period;
    return period !== null && interval > period * 1.5;
  }
}
