import { describe, expect, it } from "vitest";
import { fileName, formatTimecode, parseTimecode } from "./format";

describe("formatTimecode", () => {
  it("formats zero", () => expect(formatTimecode(0)).toBe("00:00.00"));
  it("rolls seconds into minutes", () => expect(formatTimecode(125)).toBe("02:05.00"));
  it("shows centiseconds", () => expect(formatTimecode(3.5)).toBe("00:03.50"));
  it("does not round up into the next second", () => expect(formatTimecode(59.999)).toBe("00:59.99"));
  it("is not thrown off by binary float error (1.23 must read .23)", () => {
    expect(formatTimecode(1.23)).toBe("00:01.23");
    expect(formatTimecode(0.29)).toBe("00:00.29");
  });
});

describe("parseTimecode", () => {
  it("parses raw seconds", () => {
    expect(parseTimecode("12")).toBe(12);
    expect(parseTimecode(" 1.5 ")).toBe(1.5);
  });
  it("parses M:SS and MM:SS.ff", () => {
    expect(parseTimecode("1:30")).toBe(90);
    expect(parseTimecode("01:05.25")).toBe(65.25);
  });
  it("treats a single fractional digit as tenths", () => {
    expect(parseTimecode("0:01.5")).toBe(1.5);
  });
  it("rejects empty, malformed, negative and out-of-range input", () => {
    expect(parseTimecode("")).toBeNull();
    expect(parseTimecode("   ")).toBeNull();
    expect(parseTimecode("abc")).toBeNull();
    expect(parseTimecode("-3")).toBeNull();
    expect(parseTimecode("1:75")).toBeNull();
    expect(parseTimecode("1:2:3")).toBeNull();
  });
  it("round-trips with formatTimecode at centisecond precision", () => {
    for (const t of [0, 0.5, 7.25, 61.04, 599.99]) {
      expect(parseTimecode(formatTimecode(t))).toBeCloseTo(t, 2);
    }
  });
});

describe("fileName", () => {
  it("handles both separators", () => {
    expect(fileName(String.raw`C:\clips\a.mp4`)).toBe("a.mp4");
    expect(fileName("/home/u/b.wav")).toBe("b.wav");
  });
  it("returns bare names unchanged", () => expect(fileName("c.png")).toBe("c.png"));
});
