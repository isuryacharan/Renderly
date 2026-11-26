import {
  FLICKS_PER_SECOND,
  flicksPerFrame,
  type Clip,
  type Sequence,
  type Track,
} from "@renderly/core";
import { describe, expect, it } from "vitest";
import {
  clipsInBox,
  edgeScroll,
  firstVisible,
  hitClip,
  isMouseWheel,
  layoutRows,
  rowsHeight,
  rulerLabel,
  rulerSteps,
  snapSpan,
  timeToX,
  wheelIntent,
  zoomAround,
  zoomLimits,
  type View,
} from "./geometry";

const S = FLICKS_PER_SECOND;
let n = 0;
const id = () => `C${String(n++).padStart(15, "0")}`;

function clip(start: number, duration: number): Clip {
  return {
    id: id(),
    media: "M000000000000000",
    start,
    duration,
    in: 0,
    speed: 1,
    enabled: true,
    components: [],
  };
}
function track(kind: Track["kind"], clips: Clip[] = []): Track {
  return {
    id: id(),
    kind,
    name: "",
    locked: false,
    hidden: false,
    muted: false,
    magnetic: false,
    clips,
  };
}
function sequence(tracks: Track[]): Sequence {
  return {
    id: id(),
    name: "Main",
    width: 1920,
    height: 1080,
    fps: [30, 1],
    sampleRate: 48_000,
    tracks,
    markers: [],
  };
}

describe("layoutRows", () => {
  it("puts picture tracks highest first, then audio tracks in order", () => {
    const v1 = track("video");
    const a1 = track("audio");
    const v2 = track("video");
    const t1 = track("text");
    const a2 = track("audio");
    const rows = layoutRows(sequence([v1, a1, v2, t1, a2]));
    expect(rows.map((r) => r.track)).toEqual([t1, v2, v1, a1, a2]);
    expect(rows.map((r) => r.index)).toEqual([3, 2, 0, 1, 4]);
    expect(rows[1]?.y).toBe(41); // text track 40 + 1px line
    expect(rowsHeight(rows)).toBe(40 + 72 + 72 + 48 + 48 + 5);
  });
});

describe("firstVisible", () => {
  it("finds the first clip ending after a time", () => {
    const clips = [clip(0, S), clip(S, S), clip(3 * S, S)];
    expect(firstVisible(clips, 0)).toBe(0);
    expect(firstVisible(clips, S)).toBe(1);
    expect(firstVisible(clips, 2.5 * S)).toBe(2);
    expect(firstVisible(clips, 10 * S)).toBe(3);
  });
});

describe("hitClip", () => {
  const view: View = { t: 0, zoom: S / 100, y: 0 }; // 100 px a second
  const a = clip(0, S);
  const b = clip(S, S);
  const c = clip(3 * S, 0.1 * S); // 10 px wide
  const rows = layoutRows(sequence([track("video", [a, b, c])]));

  it("tells edges from the body", () => {
    expect(hitClip(rows, view, 50, 30)).toMatchObject({ clip: a, zone: "body" });
    expect(hitClip(rows, view, 2, 30)).toMatchObject({ clip: a, zone: "start" });
    expect(hitClip(rows, view, 197, 30)).toMatchObject({ clip: b, zone: "end" });
  });

  it("gives a shared edge to the clip under the pointer", () => {
    expect(hitClip(rows, view, 98, 30)).toMatchObject({ clip: a, zone: "end" });
    expect(hitClip(rows, view, 102, 30)).toMatchObject({ clip: b, zone: "start" });
  });

  it("splits a narrow clip's edges at its middle", () => {
    expect(hitClip(rows, view, 302, 30)).toMatchObject({ clip: c, zone: "start" });
    expect(hitClip(rows, view, 308, 30)).toMatchObject({ clip: c, zone: "end" });
  });

  it("misses empty space and the row's inset", () => {
    expect(hitClip(rows, view, 250, 30)).toBeUndefined();
    expect(hitClip(rows, view, 50, 1)).toBeUndefined();
    expect(hitClip(rows, view, 50, 500)).toBeUndefined();
  });
});

describe("clipsInBox", () => {
  it("selects clips crossing the box on the rows it covers", () => {
    const view: View = { t: 0, zoom: S / 100, y: 0 };
    const v = track("video", [clip(0, S), clip(2 * S, S)]);
    const au = track("audio", [clip(0, 3 * S)]);
    const rows = layoutRows(sequence([v, au]));
    const ids = clipsInBox(rows, view, { x0: 150, y0: 10, x1: 250, y1: 20 });
    expect(ids).toEqual([v.clips[1]?.id]);
    const both = clipsInBox(rows, view, { x0: 50, y0: 10, x1: 60, y1: 100 });
    expect(both).toEqual([v.clips[0]?.id, au.clips[0]?.id]);
  });
});

describe("snapSpan", () => {
  it("snaps whichever edge is closer", () => {
    expect(snapSpan([100, 500], 104, 50, 10)).toEqual({ shift: -4, hit: 100 });
    expect(snapSpan([100, 500], 447, 50, 10)).toEqual({ shift: 3, hit: 500 });
    expect(snapSpan([100], 300, 50, 10)).toEqual({ shift: 0, hit: null });
  });
});

describe("zoom", () => {
  it("goes from 24 px a frame to the whole sequence", () => {
    const seq = sequence([]);
    const { min, max } = zoomLimits(seq, 600 * S, 1000);
    expect(min).toBe(flicksPerFrame([30, 1]) / 24);
    expect(max).toBeCloseTo((600 * S * 1.15) / 1000);
  });

  it("keeps the time under the anchor in place", () => {
    const view: View = { t: 10 * S, zoom: S / 100, y: 0 };
    const zoomed = zoomAround(view, S / 200, 300);
    expect(timeToX(zoomed, 13 * S)).toBeCloseTo(300);
  });
});

describe("ruler", () => {
  const seq = sequence([]);
  it("labels frames when zoomed in and seconds when out", () => {
    const frame = flicksPerFrame([30, 1]);
    expect(rulerSteps(seq, frame / 24).major).toBe(5 * frame);
    expect(rulerSteps(seq, S / 100).major).toBe(S);
    expect(rulerSteps(seq, S).major).toBe(120 * S);
    expect(rulerLabel(65 * S + 12 * frame, seq, true)).toBe("00:01:05:12");
    expect(rulerLabel(3723 * S, seq, false)).toBe("1:02:03");
  });
});

describe("wheel", () => {
  const base = {
    deltaX: 0,
    deltaY: 0,
    deltaMode: 0,
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    shiftKey: false,
  };
  it("tells a notched mouse wheel from a trackpad", () => {
    expect(isMouseWheel({ ...base, deltaY: 100, wheelDeltaY: -120 })).toBe(true);
    expect(isMouseWheel({ ...base, deltaY: 3, deltaMode: 1 })).toBe(true);
    expect(isMouseWheel({ ...base, deltaY: 4.5, wheelDeltaY: -13.5 })).toBe(false);
    expect(isMouseWheel({ ...base, deltaX: 2, deltaY: 6 })).toBe(false);
  });

  it("maps gestures to scrolling and zooming", () => {
    expect(wheelIntent({ ...base, deltaY: 100, wheelDeltaY: -120 })).toEqual({
      kind: "scroll",
      dx: 100,
      dy: 0,
    });
    expect(wheelIntent({ ...base, deltaY: 100, wheelDeltaY: -120, shiftKey: true })).toEqual({
      kind: "scroll",
      dx: 0,
      dy: 100,
    });
    expect(wheelIntent({ ...base, deltaX: 3, deltaY: 7 })).toEqual({
      kind: "scroll",
      dx: 3,
      dy: 7,
    });
    const pinch = wheelIntent({ ...base, deltaY: -10, ctrlKey: true });
    expect(pinch.kind === "zoom" && pinch.factor < 1).toBe(true);
  });

  it("scrolls faster nearer the edge", () => {
    expect(edgeScroll(500, 0, 1000)).toBe(0);
    expect(edgeScroll(990, 0, 1000)).toBeGreaterThan(edgeScroll(975, 0, 1000));
    expect(edgeScroll(5, 0, 1000)).toBeLessThan(0);
    expect(edgeScroll(5000, 0, 1000)).toBe(32);
  });
});
