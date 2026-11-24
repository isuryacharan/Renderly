/**
 * Back-pressure for frames crossing a thread boundary. The sender takes one credit per frame and
 * waits when none are left; the receiver gives credits back as it closes frames. A receiver that
 * leaks frames therefore stalls the decoder instead of filling GPU memory.
 */
export class CreditGate {
  private available: number;
  private waiters: { resolve: (ok: boolean) => void }[] = [];

  constructor(readonly capacity: number) {
    if (!Number.isInteger(capacity) || capacity < 1) throw new RangeError("capacity must be >= 1");
    this.available = capacity;
  }

  /** Credits currently taken, i.e. frames the receiver has not closed yet. */
  get inFlight(): number {
    return this.capacity - this.available;
  }

  /** Resolves true once a credit is taken, or false if `cancelWaiters` ran first. */
  acquire(): Promise<boolean> {
    if (this.available > 0) {
      this.available--;
      return Promise.resolve(true);
    }
    return new Promise((resolve) => this.waiters.push({ resolve }));
  }

  release(count = 1): void {
    for (let i = 0; i < count; i++) {
      const waiter = this.waiters.shift();
      if (waiter) waiter.resolve(true);
      else if (this.available < this.capacity) this.available++;
    }
  }

  /** Wakes every waiter with `false`, e.g. when the operation it belongs to is stopped. */
  cancelWaiters(): void {
    for (const waiter of this.waiters.splice(0)) waiter.resolve(false);
  }
}
