import { describe, expect, it } from "vitest";
import { CreditGate } from "./credits";

describe("CreditGate", () => {
  it("grants up to capacity, then waits for a release", async () => {
    const gate = new CreditGate(2);
    expect(await gate.acquire()).toBe(true);
    expect(await gate.acquire()).toBe(true);
    expect(gate.inFlight).toBe(2);

    let granted: boolean | undefined;
    void gate.acquire().then((g) => (granted = g));
    await Promise.resolve();
    expect(granted).toBeUndefined();

    gate.release();
    await Promise.resolve();
    expect(granted).toBe(true);
    expect(gate.inFlight).toBe(2);
  });

  it("wakes waiters with false when cancelled, without granting credit", async () => {
    const gate = new CreditGate(1);
    await gate.acquire();
    const waiting = gate.acquire();
    gate.cancelWaiters();
    expect(await waiting).toBe(false);
    expect(gate.inFlight).toBe(1);
  });

  it("never exceeds capacity on extra releases", () => {
    const gate = new CreditGate(3);
    gate.release(5);
    expect(gate.inFlight).toBe(0);
  });

  it("rejects a capacity below one", () => {
    expect(() => new CreditGate(0)).toThrow(RangeError);
  });
});
