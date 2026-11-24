import { describe, expect, it } from "vitest";
import { FrameQueue, type Timed } from "./frame-queue";

const frames = (...times: number[]): Timed[] => times.map((time) => ({ time, duration: 10 }));

describe("FrameQueue", () => {
  it("waits when the first frame is still in the future", () => {
    const queue = new FrameQueue<Timed>();
    for (const f of frames(10, 20)) queue.push(f);
    expect(queue.take(5)).toEqual({ frame: null, dropped: [] });
    expect(queue.length).toBe(2);
  });

  it("shows the frame whose interval contains the playhead and keeps later ones", () => {
    const queue = new FrameQueue<Timed>();
    for (const f of frames(0, 10, 20)) queue.push(f);
    const pick = queue.take(12);
    expect(pick.frame?.time).toBe(10);
    expect(pick.dropped.map((f) => f.time)).toEqual([0]);
    expect(queue.length).toBe(1);
  });

  it("shows a frame that starts exactly at the playhead", () => {
    const queue = new FrameQueue<Timed>();
    for (const f of frames(0, 10)) queue.push(f);
    expect(queue.take(10).frame?.time).toBe(10);
  });

  it("returns everything on clear", () => {
    const queue = new FrameQueue<Timed>();
    for (const f of frames(0, 10)) queue.push(f);
    expect(queue.clear()).toHaveLength(2);
    expect(queue.length).toBe(0);
  });
});
