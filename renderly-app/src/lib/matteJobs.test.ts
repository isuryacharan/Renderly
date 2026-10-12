import { describe, expect, it } from "vitest";
import { formatModelSize, matteProgressPercent, matteStatusLabel } from "./matteJobs";

describe("matte job helpers", () => {
  it("computes a clamped percentage", () => {
    expect(matteProgressPercent(null)).toBe(0);
    expect(matteProgressPercent({ frame: 5, total: 0 })).toBe(0);
    expect(matteProgressPercent({ frame: 30, total: 120 })).toBe(25);
    expect(matteProgressPercent({ frame: 130, total: 120 })).toBe(100);
  });

  it("labels every matte state", () => {
    expect(matteStatusLabel(null, null)).toBe("Not generated");
    expect(matteStatusLabel({ state: "not_generated" }, null)).toBe("Not generated");
    expect(matteStatusLabel({ state: "ready", frames: 301 }, null)).toBe("Ready · 301 frames");
    expect(matteStatusLabel({ state: "ready", frames: null }, null)).toBe("Ready");
    expect(matteStatusLabel({ state: "stale", reason: "source file changed on disk" }, null)).toBe(
      "Out of date — source file changed on disk",
    );
    // A running bake wins over whatever the stored status says.
    expect(matteStatusLabel({ state: "ready", frames: 3 }, { frame: 1, total: 4 })).toBe(
      "Generating… 25%",
    );
    expect(matteStatusLabel(null, { frame: 0, total: 0 })).toBe("Generating…");
  });

  it("formats the download size like the consent text quotes it", () => {
    expect(formatModelSize(14_975_696)).toBe("≈15 MB");
    expect(formatModelSize(229_738)).toBe("≈230 KB");
  });
});
