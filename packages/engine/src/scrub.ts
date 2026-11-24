// Scrubbing helpers. Pure: no workers, no GPU.

/** How long the playhead must rest before keyframe pictures are replaced by exact frames. */
export const SETTLE_MS = 120;

/**
 * Runs at most one job at a time. A request made while a job runs replaces any request still
 * waiting, so only the newest one runs next: a drag that moves on while a seek is in flight
 * skips the positions it passed.
 */
export class LatestWins<T> {
  private running = false;
  private waiting: { value: T } | null = null;
  /** Requests that were replaced before they ran. */
  skipped = 0;

  constructor(private readonly job: (value: T) => Promise<void>) {}

  get busy(): boolean {
    return this.running;
  }

  request(value: T): void {
    if (this.waiting) this.skipped++;
    this.waiting = { value };
    if (!this.running) void this.drain();
  }

  /** Drops a waiting request, if any. A running job finishes. */
  cancel(): void {
    this.waiting = null;
  }

  private async drain(): Promise<void> {
    this.running = true;
    try {
      while (this.waiting) {
        const { value } = this.waiting;
        this.waiting = null;
        try {
          await this.job(value);
        } catch {
          // A failed seek is not fatal to scrubbing; the next request tries again.
        }
      }
    } finally {
      this.running = false;
    }
  }
}

/** Least-recently-used set of keys with a fixed capacity. Returns what was evicted. */
export class LruKeys<K> {
  private readonly keys: K[] = [];

  constructor(readonly capacity: number) {}

  has(key: K): boolean {
    return this.keys.includes(key);
  }

  /** Marks `key` as most recently used. */
  touch(key: K): void {
    const index = this.keys.indexOf(key);
    if (index >= 0) this.keys.splice(index, 1);
    this.keys.push(key);
  }

  /** Adds `key`, evicting the least recently used key if full. */
  add(key: K): K | null {
    if (this.has(key)) {
      this.touch(key);
      return null;
    }
    this.keys.push(key);
    return this.keys.length > this.capacity ? (this.keys.shift() ?? null) : null;
  }

  clear(): K[] {
    return this.keys.splice(0);
  }
}
