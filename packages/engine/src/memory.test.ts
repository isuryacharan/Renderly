import { describe, expect, it } from "vitest";
import { halfFloatTargetBytes, MemoryBudget } from "./memory";

describe("MemoryBudget", () => {
  it("reserves under the ceiling and refuses past it without changing anything", () => {
    const budget = new MemoryBudget(100);
    expect(budget.reserve("a", 60)).toBe(true);
    expect(budget.reserve("b", 50)).toBe(false);
    expect(budget.used).toBe(60);
    expect(budget.reserve("b", 40)).toBe(true);
    expect(budget.used).toBe(100);
  });

  it("replaces a key's amount instead of adding to it", () => {
    const budget = new MemoryBudget(100);
    budget.reserve("target", 80);
    expect(budget.reserve("target", 90)).toBe(true);
    expect(budget.used).toBe(90);
  });

  it("records measured amounts past the ceiling and keeps the peak", () => {
    const budget = new MemoryBudget(100);
    budget.reserve("target", 50);
    budget.set("frames", 70);
    expect(budget.used).toBe(120);
    budget.set("frames", 10);
    expect(budget.used).toBe(60);
    expect(budget.peak).toBe(120);
    budget.resetPeak();
    expect(budget.peak).toBe(60);
  });

  it("frees on release", () => {
    const budget = new MemoryBudget(100);
    budget.reserve("a", 30);
    budget.release("a");
    budget.release("never-there");
    expect(budget.used).toBe(0);
  });

  it("sizes a 4K half-float target at about 66 MB", () => {
    expect(halfFloatTargetBytes(3840, 2160)).toBe(66_355_200);
  });
});
