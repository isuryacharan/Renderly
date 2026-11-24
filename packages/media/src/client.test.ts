import { describe, expect, it } from "vitest";
import { DecodeClient, type DecodedFrame } from "./client";
import type { Endpoint } from "./decoder";
import type { FromWorker, ToWorker } from "./protocol";

/** A stand-in for VideoFrame: only close() matters to the client. */
class FakeFrame {
  closed = false;
  close() {
    this.closed = true;
  }
}

function setup() {
  const sent: ToWorker[] = [];
  const endpoint: Endpoint = {
    onmessage: null,
    postMessage: (message) => sent.push(message as ToWorker),
  };
  const client = new DecodeClient(endpoint);
  const reply = (message: FromWorker) =>
    endpoint.onmessage?.(new MessageEvent("message", { data: message }));
  const frame = (id: number, time = 0) => {
    const f = new FakeFrame();
    reply({ type: "frame", id, frame: f as unknown as VideoFrame, time, duration: 1 });
    return f;
  };
  const credits = () =>
    sent
      .filter((m) => m.type === "credit")
      .reduce((n, m) => n + (m.type === "credit" ? m.count : 0), 0);
  return { client, sent, reply, frame, credits };
}

describe("DecodeClient", () => {
  it("counts frames until they are released, and returns a credit for each", async () => {
    const { client, sent, reply, frame, credits } = setup();
    const received: DecodedFrame[] = [];
    const playback = client.play(0, 100, (f) => received.push(f));
    const id = (sent[0] as Extract<ToWorker, { type: "play" }>).id;

    const a = frame(id, 0);
    frame(id, 10);
    expect(client.outstanding).toBe(2);

    received[0]!.release();
    received[0]!.release(); // a second release is a no-op
    expect(a.closed).toBe(true);
    expect(client.outstanding).toBe(1);
    expect(credits()).toBe(1);

    reply({ type: "done", id, frames: 2 });
    expect(await playback.done).toBe(2);
    received[1]!.release();
    expect(client.outstanding).toBe(0);
    expect(credits()).toBe(2);
  });

  it("closes frames that arrive after their stream ended", async () => {
    const { client, sent, reply, frame } = setup();
    const playback = client.play(0, 100, () => {});
    const id = (sent[0] as Extract<ToWorker, { type: "play" }>).id;
    reply({ type: "done", id, frames: 0 });
    await playback.done;

    const late = frame(id);
    expect(late.closed).toBe(true);
    expect(client.outstanding).toBe(0);
  });

  it("resolves a seek with its frame, and null when it was replaced", async () => {
    const { client, sent, reply, frame } = setup();
    const first = client.seek(5);
    const second = client.seek(6);
    const [a, b] = sent.map((m) => (m as Extract<ToWorker, { type: "seek" }>).id);
    reply({ type: "done", id: a!, frames: 0 });
    frame(b!, 6);
    reply({ type: "done", id: b!, frames: 1 });

    expect(await first).toBeNull();
    const result = await second;
    expect(result?.time).toBe(6);
    result?.release();
    expect(client.outstanding).toBe(0);
  });

  it("rejects a stream when the worker reports an error", async () => {
    const { client, sent, reply } = setup();
    const playback = client.play(0, 100, () => {});
    const id = (sent[0] as Extract<ToWorker, { type: "play" }>).id;
    reply({ type: "error", id, message: "decode failed" });
    await expect(playback.done).rejects.toThrow("decode failed");
  });
});
