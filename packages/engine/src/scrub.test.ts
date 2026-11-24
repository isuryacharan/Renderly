import { describe, expect, it } from "vitest";
import { LatestWins, LruKeys } from "./scrub";

/** A job whose completions the test controls. */
function controlled() {
  const started: number[] = [];
  const finish: (() => void)[] = [];
  const job = (value: number) =>
    new Promise<void>((resolve) => {
      started.push(value);
      finish.push(resolve);
    });
  const settle = async () => {
    finish.shift()?.();
    await new Promise((r) => setTimeout(r, 0));
  };
  return { started, job, settle };
}

describe("LatestWins", () => {
  it("runs one job at a time and only the newest waiting request next", async () => {
    const { started, job, settle } = controlled();
    const latest = new LatestWins(job);
    latest.request(1);
    latest.request(2);
    latest.request(3);
    latest.request(4);
    expect(started).toEqual([1]);
    await settle();
    expect(started).toEqual([1, 4]);
    expect(latest.skipped).toBe(2);
    await settle();
    expect(latest.busy).toBe(false);
  });

  it("drops a waiting request on cancel", async () => {
    const { started, job, settle } = controlled();
    const latest = new LatestWins(job);
    latest.request(1);
    latest.request(2);
    latest.cancel();
    await settle();
    expect(started).toEqual([1]);
  });

  it("keeps going after a failed job", async () => {
    const ran: number[] = [];
    const latest = new LatestWins(async (value: number) => {
      ran.push(value);
      if (value === 1) throw new Error("seek failed");
    });
    latest.request(1);
    latest.request(2);
    await new Promise((r) => setTimeout(r, 0));
    expect(ran).toEqual([1, 2]);
  });
});

describe("LruKeys", () => {
  it("evicts the least recently used key", () => {
    const lru = new LruKeys<number>(2);
    expect(lru.add(1)).toBeNull();
    expect(lru.add(2)).toBeNull();
    lru.touch(1);
    expect(lru.add(3)).toBe(2);
    expect(lru.has(1)).toBe(true);
    expect(lru.has(2)).toBe(false);
  });

  it("refreshes a key added twice instead of duplicating it", () => {
    const lru = new LruKeys<number>(2);
    lru.add(1);
    lru.add(2);
    lru.add(1);
    expect(lru.add(3)).toBe(2);
  });
});
