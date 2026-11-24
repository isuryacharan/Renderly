import { describe, expect, it } from "vitest";
import { createId, ID_PATTERN } from "./ids";

describe("createId", () => {
  it("makes 16-character base-32 ids", () => {
    for (let i = 0; i < 1000; i++) expect(createId()).toMatch(ID_PATTERN);
  });

  it("is repeatable with an injected source of bytes", () => {
    const zeros = (bytes: Uint8Array<ArrayBuffer>) => bytes.fill(0);
    expect(createId(zeros)).toBe("0000000000000000");
  });

  it("does not repeat over many ids", () => {
    const ids = new Set(Array.from({ length: 100_000 }, () => createId()));
    expect(ids.size).toBe(100_000);
  });
});
