import { describe, expect, it } from "vitest";
import { cutPoints, evaluateSequence, frameAt, graphHash, stableHash } from "./evaluate";
import { projectSchema, type Clip, type Project, type Track } from "./schema";

const id = (prefix: string) => prefix.padEnd(16, "0");
const SEC = 705_600_000;
const s = (seconds: number) => Math.round(seconds * SEC);
const SEQ = id("sq");
const AV = id("mav");
const B = id("mbb");

const clip = (name: string, start: number, duration: number, extra: Partial<Clip> = {}): Clip => ({
  id: id(`c${name}`),
  media: AV,
  start: s(start),
  duration: s(duration),
  in: 0,
  speed: 1,
  enabled: true,
  components: [],
  ...extra,
});

const track = (name: string, kind: Track["kind"], clips: Clip[], extra: Partial<Track> = {}) => ({
  id: id(`t${name}`),
  kind,
  name: "",
  locked: false,
  hidden: false,
  muted: false,
  magnetic: false,
  clips,
  ...extra,
});

const media = (mediaId: string) => ({
  id: mediaId,
  name: `${mediaId}.mp4`,
  link: "handle",
  path: `${mediaId}.mp4`,
  fingerprint: { size: 1, modified: 1, head: "x" },
  duration: s(60),
  video: { width: 1920, height: 1080, fps: [30, 1], codec: "avc" },
  audio: { sampleRate: 48_000, channels: 2, codec: "aac" },
});

/**
 * V1: a [0,2) b [2,5) from 10 s in · V2 (over V1): c [1,3) at half size, d [6,7) disabled ·
 * A1: e [0,8) · T1: empty · V3 hidden: f [0,1).
 */
function fixture(): Project {
  return projectSchema().parse({
    format: "renderly",
    version: 3,
    id: id("pj"),
    name: "Evaluate",
    revision: 0,
    media: [media(AV), media(B)],
    sequences: [
      {
        id: SEQ,
        name: "Main",
        width: 1920,
        height: 1080,
        fps: [30, 1],
        tracks: [
          track("v1", "video", [clip("a", 0, 2), clip("b", 2, 3, { in: s(10), media: B })]),
          track("a1", "audio", [clip("e", 0, 8)]),
          track("v2", "video", [
            clip("c", 1, 2, {
              components: [
                {
                  type: "transform",
                  position: [0.25, 0],
                  scale: [0.5, 0.4],
                  rotation: 30,
                  anchor: [0.1, -0.2],
                  flipX: true,
                  opacity: 0.8,
                  fit: "fill",
                },
                { type: "blend", mode: "multiply" },
                { type: "crop", left: 0.1, right: 0.2 },
              ],
            }),
            clip("d", 6, 1, { enabled: false }),
          ]),
          track("t1", "text", []),
          track("v3", "video", [clip("f", 0, 1)], { hidden: true }),
        ],
      },
    ],
  }) as Project;
}

describe("evaluateSequence", () => {
  it("lowers visible video tracks to layers, bottom first", () => {
    const graph = evaluateSequence(fixture(), SEQ);
    expect(graph.layers.map((l) => l.track)).toEqual([id("tv1"), id("tv2")]);
    expect(graph.layers[0]?.clips.map((c) => [c.clip, c.media, c.in, c.out, c.start])).toEqual([
      [id("ca"), AV, 0, s(2), 0],
      [id("cb"), B, s(10), s(13), s(2)],
    ]);
    expect(graph.layers[1]?.clips.map((c) => c.clip)).toEqual([id("cc")]); // d is disabled
    expect(graph.layers[1]?.clips[0]?.transform).toEqual({
      x: 0.25,
      y: 0,
      scaleX: 0.5,
      scaleY: 0.4,
      rotation: 30,
      anchorX: 0.1,
      anchorY: -0.2,
      flipX: true,
      flipY: false,
      opacity: 0.8,
      fit: "fill",
      cropLeft: 0.1,
      cropTop: 0,
      cropRight: 0.2,
      cropBottom: 0,
      blend: "multiply",
    });
    expect(graph.duration).toBe(s(8)); // the audio track is the longest
    expect(graph.background).toEqual({ kind: "color", color: "#000000", blur: 0.5 });
  });

  it("plays a freeze frame (speed 0) as its first source frame throughout", () => {
    const project = fixture();
    const first = project.sequences[0]?.tracks[0]?.clips[1];
    if (!first) throw new Error("no clip");
    first.speed = 0;
    const graph = evaluateSequence(project, SEQ);
    const frozen = graph.layers[0]?.clips[1];
    expect(frozen?.freeze).toBe(true);
    expect([frozen?.in, frozen?.out, frozen?.start]).toEqual([s(10), s(13), s(2)]);
    expect(frameAt(graph, s(2)).layers[0]?.sourceTime).toBe(s(10));
    expect(frameAt(graph, s(4.5)).layers[0]?.sourceTime).toBe(s(10));
    expect(frameAt(graph, s(3.2)).hash).toBe(frameAt(graph, s(4.5)).hash); // only the still shows
    expect(graph.skipped).toEqual([]);
  });

  it("animates the crop's edges from their keys", () => {
    const project = fixture();
    const c = project.sequences[0]?.tracks[2]?.clips[0];
    const crop = c?.components[2] as Record<string, unknown> | undefined;
    if (!crop) throw new Error("no crop");
    crop["keys"] = {
      left: [
        { t: 0, v: 0, ease: "linear" },
        { t: s(1), v: 0.4, ease: "linear" },
      ],
    };
    const graph = evaluateSequence(project, SEQ);
    expect(graph.layers[1]?.clips[0]?.keys?.cropLeft).toHaveLength(2);
    const at = frameAt(graph, s(1.5)).layers[1]?.transform;
    expect(at?.cropLeft).toBeCloseTo(0.2, 9);
    expect(at?.cropRight).toBe(0.2);
  });

  it("leaves out media that cannot be read, and says so", () => {
    const graph = evaluateSequence(fixture(), SEQ, { available: (m) => m !== B });
    expect(graph.layers[0]?.clips.map((c) => c.clip)).toEqual([id("ca")]);
    expect(graph.skipped).toEqual([{ clip: id("cb"), reason: "unavailable" }]);
  });
});

describe("frameAt", () => {
  const graph = evaluateSequence(fixture(), SEQ);

  it("gives each layer's media, source time and transform at a time", () => {
    const frame = frameAt(graph, s(2.5));
    expect(frame.layers).toEqual([
      expect.objectContaining({ layer: 0, clip: id("cb"), media: B, sourceTime: s(10.5) }),
      expect.objectContaining({ layer: 1, clip: id("cc"), media: AV, sourceTime: s(1.5) }),
    ]);
    expect(frame.sources).toEqual([B, AV]);
    expect(frameAt(graph, s(0.5)).layers.map((l) => l.clip)).toEqual([id("ca")]);
    expect(frameAt(graph, s(6.5)).layers).toEqual([]); // past every drawing clip
  });

  it("hashes what decides the picture, not the time or the clip ids", () => {
    const a = frameAt(graph, s(0.5));
    expect(frameAt(graph, s(0.5)).hash).toBe(a.hash);
    expect(frameAt(graph, s(0.6)).hash).not.toBe(a.hash);
    // Two empty times show the same (black) picture.
    expect(frameAt(graph, s(6)).hash).toBe(frameAt(graph, s(7.5)).hash);
  });
});

describe("hashes", () => {
  it("do not depend on key order", () => {
    expect(stableHash({ a: 1, b: [2, { c: 3, d: 4 }] })).toBe(
      stableHash({ b: [2, { d: 4, c: 3 }], a: 1 }),
    );
    expect(stableHash({ a: 1 })).not.toBe(stableHash({ a: 2 }));
    expect(stableHash("x")).toMatch(/^[0-9a-f]{16}$/);
  });

  it("change with the graph exactly when what plays changes", () => {
    const project = fixture();
    const before = graphHash(evaluateSequence(project, SEQ));
    expect(graphHash(evaluateSequence(fixture(), SEQ))).toBe(before);
    const renamed = { ...project, name: "Other" };
    expect(graphHash(evaluateSequence(renamed, SEQ))).toBe(before);
    const sequence = project.sequences[0];
    const v1 = sequence?.tracks[0];
    if (!v1 || !v1.clips[0]) throw new Error("fixture");
    v1.clips[0].in = s(1);
    expect(graphHash(evaluateSequence(project, SEQ))).not.toBe(before);
  });
});

describe("cutPoints", () => {
  it("lists every clip start and end on drawing layers", () => {
    expect(cutPoints(evaluateSequence(fixture(), SEQ))).toEqual([0, s(1), s(2), s(3), s(5)]);
  });
});
