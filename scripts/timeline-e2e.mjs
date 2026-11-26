// Timeline and viewer checks in a real browser, driven with the mouse and keyboard.
// S1.7: drag from the bin, move, trim, split, delete, ripple delete, the blade tool, box select
// with edge auto-scroll, snapping, one undo step per drag and Escape cancelling one; the redraw
// benchmark (500 clips in under 2 ms).
// S1.8: the viewer plays what the timeline says (transport, frame step, cuts, loop), edits during
// playback reach the engine within a frame and never break it, and trimming shows the edge frame.
// The media lab makes the test files (see media-e2e.mjs).
//
//   pnpm test:timeline                    installed Edge
//   pnpm test:timeline --browser chrome   installed Chrome (CI)
//   pnpm test:timeline --only redraw      one scenario

/* global mediaLab -- the media lab page's test hooks, used inside page.evaluate() */

import { spawn } from "node:child_process";
import { chromium } from "playwright-core";

const arg = (name, fallback) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : fallback;
};
const channel = arg("browser", "msedge");
const only = arg("only", null);
const slowdown = Number(arg("cpu-slowdown", "1"));
const PORT = Number(process.env.PORT ?? 5183);
const BASE = `http://localhost:${PORT}`;
const SECOND = 705_600_000;
const FRAME = SECOND / 30;

const log = (...parts) => console.log(new Date().toISOString().slice(11, 19), ...parts);

async function startServer() {
  const server = spawn(`pnpm --filter @renderly/editor exec vite --port ${PORT} --strictPort`, {
    cwd: new URL("..", import.meta.url),
    shell: true,
    stdio: "ignore",
  });
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    try {
      if ((await fetch(`${BASE}/media-lab.html`)).ok) return server;
    } catch {
      // not up yet
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error("Dev server did not start");
}

function stop(server) {
  if (process.platform === "win32") spawn("taskkill", ["/pid", String(server.pid), "/T", "/F"]);
  else server.kill();
}

class Failure extends Error {}
function check(condition, message) {
  if (!condition) throw new Failure(message);
}

async function openLab(context) {
  const page = await context.newPage();
  page.on("pageerror", (error) => log("page error:", error.message));
  await page.goto(`${BASE}/media-lab.html`);
  await page.waitForFunction(() => window.labReadyAt !== undefined, null, { timeout: 60_000 });
  return page;
}

async function openEditor(context, folder) {
  const page = await context.newPage();
  // --cpu-slowdown N: throttle the editor page N times, as a slow CI runner would be.
  if (slowdown > 1)
    await (
      await context.newCDPSession(page)
    ).send("Emulation.setCPUThrottlingRate", { rate: slowdown });
  page.on("pageerror", (error) => log("page error:", error.message));
  await page.goto(`${BASE}/#/project/browser/${folder}`);
  await page.waitForFunction(() => window.renderlyTimeline !== undefined, null, {
    timeout: 20_000,
  });
  await page.waitForFunction(() => window.renderlyTimeline.sequence !== undefined);
  // The lab's tab may still hold the project's writer lock for a moment; edits need the writer.
  await page.waitForFunction(() => !window.renderlyTimeline["options"].readOnly(), null, {
    timeout: 10_000,
  });
  // A reload in the middle of a scenario would explain a lost drop: say so.
  page.on("framenavigated", (frame) => {
    if (frame === page.mainFrame()) log("  the editor reloaded:", frame.url());
  });
  return page;
}

/** Tracks and clips of the sequence on screen, the selection and the undo list. */
const state = (page) =>
  page.evaluate(() => {
    const t = window.renderlyTimeline;
    const { store, view } = t["options"];
    return {
      tracks: t.sequence.tracks.map((track) => ({
        id: track.id,
        kind: track.kind,
        clips: track.clips.map((c) => ({
          id: c.id,
          start: c.start,
          duration: c.duration,
          in: c.in,
        })),
      })),
      selection: [...view.get().selection],
      undo: store.undoLabels,
      revision: store.revision,
      scrollT: view.get().scroll.t,
    };
  });

const clips = (s) => s.tracks.flatMap((t) => t.clips);
const box = (page, id) => page.evaluate((id) => window.renderlyTimeline.clipBox(id), id);
const pageX = (page, t) => page.evaluate((t) => window.renderlyTimeline.pageX(t), t);

/**
 * Drags bin item `index` onto the track area at (x, y) relative to it, the way a person does:
 * press, nudge past the drag threshold, move in steps, release. Waits for the bin to settle first
 * (its thumbnails load after it appears), and re-measures right before pressing.
 */
async function dropMedia(page, index, x, y) {
  await page.waitForFunction(() =>
    [...document.querySelectorAll(".r-bin__item img")].every((img) => img.complete),
  );
  const item = page.locator(".r-bin__item").nth(index);
  await item.scrollIntoViewIfNeeded();
  const from = await item.boundingBox();
  const area = await page.locator(".timeline__tracks").boundingBox();
  const x0 = from.x + from.width / 2;
  const y0 = from.y + from.height / 3;
  await page.mouse.move(x0, y0);
  await page.mouse.down();
  await page.mouse.move(x0 + 8, y0 + 8);
  await page.mouse.move(area.x + x, area.y + y, { steps: 12 });
  await page.mouse.move(area.x + x, area.y + y);
  await page.mouse.up();
}

/** Presses at (x0, y), moves in steps to (x1, y1) and releases, like a person dragging. */
async function drag(page, x0, y0, x1, y1, { steps = 20, before } = {}) {
  await page.mouse.move(x0, y0);
  await page.mouse.down();
  for (let i = 1; i <= steps; i++) {
    await page.mouse.move(x0 + ((x1 - x0) * i) / steps, y0 + ((y1 - y0) * i) / steps);
    await page.waitForTimeout(8);
  }
  if (before) await before();
  await page.mouse.up();
}

/**
 * A project for playback: an 8 s clip on V1 at 0 (two 4 s files back to back) and a 1 s clip on
 * V2 at 2 s. Returns the editor page once the viewer's engine is up.
 */
async function playbackProject(context, name) {
  const lab = await openLab(context);
  const folder = await lab.evaluate(async (name) => {
    await mediaLab.makeClip("p1.webm", 4, "p1");
    await mediaLab.makeClip("p2.webm", 4, "p2");
    const folder = await mediaLab.createProject(name);
    await mediaLab.open(folder);
    await mediaLab.importOutside(["p1.webm", "p2.webm"]);
    await mediaLab.close();
    return folder;
  }, name);
  await lab.close();
  const page = await openEditor(context, folder);
  await page.waitForFunction(() => window.renderlyViewer !== undefined, null, { timeout: 20_000 });
  await page.waitForFunction(() => document.querySelectorAll(".r-bin__item").length === 2);
  await page.evaluate((S) => {
    const t = window.renderlyTimeline;
    const [a, b] = t["project"].media;
    t.addMedia(a, 0, null);
    t.addMedia(b, 4 * S, t.sequence.tracks[0].id);
    t.addMedia(a, 2 * S, null);
    const top = t.sequence.tracks[1];
    const clip = top.clips[0];
    t["options"].store.dispatch({
      type: "clip.trim",
      payload: { clips: [clip.id], edge: "end", delta: -3 * S },
    });
    t.fit();
    t["options"].view.set({ selection: [] });
  }, SECOND);
  // The engine has the edit list and every file opened.
  await page.waitForFunction(
    () => window.renderlyViewer.updates > 0 && window.renderlyViewer.duration > 0,
    null,
    {
      timeout: 20_000,
    },
  );
  await page.waitForTimeout(500);
  return page;
}

const scenarios = {
  async "drag from the bin, then move, trim, split and delete with the mouse and keys"({
    context,
  }) {
    const lab = await openLab(context);
    const folder = await lab.evaluate(async () => {
      await mediaLab.makeClip("a.webm", 4, "a");
      await mediaLab.makeClip("b.webm", 3, "b");
      const folder = await mediaLab.createProject("Edits");
      await mediaLab.open(folder);
      await mediaLab.importOutside(["a.webm", "b.webm"]);
      await mediaLab.close();
      return folder;
    });
    await lab.close();
    const page = await openEditor(context, folder);
    await page.waitForFunction(() => document.querySelectorAll(".r-bin__item").length === 2);
    const tracks = page.locator(".timeline__tracks");

    // Drop a at the start (snaps to 0), then b at about 5 s.
    await dropMedia(page, 0, 3, 60);
    let s = await state(page);
    if (s.tracks.length === 0)
      log(
        "  debug:",
        JSON.stringify(
          await page.evaluate(() => ({
            readOnly: window.renderlyTimeline["options"].readOnly(),
            media: window.renderlyTimeline["project"].media.length,
            toasts: [...document.querySelectorAll(".r-toast")].map((t) => t.textContent),
            draws: window.renderlyTimeline.draws,
            tracksBox: document.querySelector(".timeline__tracks")?.getBoundingClientRect(),
            canvasBox: document.querySelector(".timeline__canvas")?.getBoundingClientRect(),
          })),
        ),
      );
    check(s.tracks.length === 1 && s.tracks[0].kind === "video", "the drop made a video track");
    const [a] = clips(s);
    check(a?.start === 0 && a.duration === 4 * SECOND, `a sits at 0 for 4 s: ${JSON.stringify(a)}`);
    const area = await tracks.boundingBox();
    const at5 = (await pageX(page, 5 * SECOND)) - area.x;
    await dropMedia(page, 1, at5, 60);
    s = await state(page);
    const b = clips(s).find((c) => c.id !== a.id);
    check(b && Math.abs(b.start - 5 * SECOND) <= 2 * FRAME, `b near 5 s: ${b?.start / SECOND}`);
    check(b.start % FRAME === 0, "b starts on a frame");
    check(s.undo.length === 2, `two undo steps after two drops: ${s.undo}`);

    // Move b left so it lands next to a: one undo step, snapped to a's end.
    let r = await box(page, b.id);
    const y = r.y + r.height / 2;
    const shift = (await pageX(page, 4.05 * SECOND)) - (await pageX(page, b.start));
    await drag(page, r.x + r.width / 2, y, r.x + r.width / 2 + shift, y, { steps: 30 });
    s = await state(page);
    const moved = clips(s).find((c) => c.id === b.id);
    check(moved.start === 4 * SECOND, `b snapped to a's end: ${moved.start / SECOND}`);
    check(s.undo.length === 3 && s.undo[0] === "Move 1 clip", `one step: ${s.undo}`);

    // Escape in the middle of a drag puts it back and leaves no undo step.
    const before = s;
    r = await box(page, b.id);
    await drag(page, r.x + r.width / 2, y, r.x + r.width / 2 + 200, y, {
      before: async () => {
        const mid = await state(page);
        check(clips(mid).find((c) => c.id === b.id).start > 4 * SECOND, "the drag previews");
        await page.keyboard.press("Escape");
      },
    });
    s = await state(page);
    check(JSON.stringify(s.tracks) === JSON.stringify(before.tracks), "Escape restored the clips");
    check(s.undo.length === before.undo.length, "a cancelled drag is not an undo step");
    check(s.revision === before.revision, "a cancelled drag changes no revision");

    // Trim a's end 1 s earlier.
    r = await box(page, a.id);
    const oneSecond = (await pageX(page, SECOND)) - (await pageX(page, 0));
    await drag(page, r.x + r.width - 2, y, r.x + r.width - 2 - oneSecond, y);
    s = await state(page);
    const trimmed = clips(s).find((c) => c.id === a.id);
    check(trimmed.duration === 3 * SECOND, `a trimmed to 3 s: ${trimmed.duration / SECOND}`);
    check(s.undo[0] === "Trim 1 clip", `trim is one step: ${s.undo[0]}`);

    // Click the ruler at 1.5 s, then S splits a there.
    const canvas = await page.locator(".timeline__canvas").boundingBox();
    await page.mouse.click(await pageX(page, 1.5 * SECOND), canvas.y + 8);
    await page.keyboard.press("s");
    s = await state(page);
    check(clips(s).length === 3, `split made 3 clips: ${clips(s).length}`);
    const right = clips(s).find((c) => c.start === 1.5 * SECOND);
    check(right && right.in === 1.5 * SECOND, "the right half reads on from the cut");

    // Delete the first piece; ripple delete the second, which pulls b back.
    r = await box(page, a.id);
    await page.mouse.click(r.x + r.width / 2, y);
    await page.keyboard.press("Delete");
    s = await state(page);
    check(clips(s).length === 2 && !clips(s).some((c) => c.id === a.id), "Delete removed a");
    r = await box(page, right.id);
    await page.mouse.click(r.x + r.width / 2, y);
    await page.keyboard.press("Shift+Delete");
    s = await state(page);
    const pulled = clips(s).find((c) => c.id === b.id);
    check(clips(s).length === 1, "ripple delete removed the piece");
    check(pulled.start === 2.5 * SECOND, `b moved back 1.5 s: ${pulled.start / SECOND}`);

    // The blade tool cuts where it is clicked.
    await page.keyboard.press("b");
    await page.mouse.click(await pageX(page, 4 * SECOND), y);
    await page.keyboard.press("v");
    s = await state(page);
    check(clips(s).length === 2, "the blade cut b in two");
    check(
      clips(s).some((c) => c.start === 4 * SECOND),
      `the cut is at 4 s: ${clips(s).map((c) => c.start / SECOND)}`,
    );

    // Everything undoes back to the empty timeline.
    const steps = s.undo.length;
    for (let i = 0; i < steps; i++) await page.keyboard.press("Control+z");
    s = await state(page);
    check(clips(s).length === 0 && s.tracks.length === 0, "undo returns to the empty timeline");
    return `${steps} undo steps for 2 drops, move, trim, split, delete, ripple delete, blade`;
  },

  async "box select scrolls at the edge, and snapping shows a guide"({ context }) {
    const lab = await openLab(context);
    const folder = await lab.evaluate(async () => {
      await mediaLab.makeClip("c.webm", 1, "c");
      const folder = await mediaLab.createProject("Box");
      await mediaLab.open(folder);
      const { imported } = await mediaLab.importOutside(["c.webm"]);
      mediaLab.addClips(imported[0].id, 40); // 0.5 s clips, one a second
      await mediaLab.close();
      return folder;
    });
    await lab.close();
    const page = await openEditor(context, folder);
    await page.evaluate((zoom) => window.renderlyTimeline.setZoom(zoom, 0), SECOND / 100);
    let s = await state(page);
    const first = clips(s)[0];
    const r = await box(page, first.id);
    const y = r.y + r.height / 2;
    const canvas = await page.locator(".timeline__canvas").boundingBox();

    // From the gap after the first clip to the right edge; holding there scrolls.
    const from = await pageX(page, 0.75 * SECOND);
    await page.mouse.move(from, y);
    await page.mouse.down();
    await page.mouse.move(canvas.x + canvas.width - 4, y + 4, { steps: 10 });
    await page.waitForTimeout(800);
    await page.mouse.up();
    s = await state(page);
    const visible = canvas.width / 100; // seconds on screen at the start
    check(s.scrollT > 0, `the timeline scrolled while selecting: ${s.scrollT / SECOND} s`);
    check(
      s.selection.length > visible,
      `selected past the first screen: ${s.selection.length} > ${visible.toFixed(1)}`,
    );
    check(!s.selection.includes(first.id), "the box began after the first clip");
    const selected = s.selection.length;

    // Snapping: dragging clip 2 so its start is 3 px from clip 1's end shows the guide and lands there.
    await page.evaluate(() => {
      const t = window.renderlyTimeline;
      t["options"].view.set({ scroll: { t: 0, y: 0 }, selection: [] });
    });
    s = await state(page);
    const [c1, c2] = clips(s);
    const r2 = await box(page, c2.id);
    const target = (await pageX(page, c1.start + c1.duration)) + 3;
    let guide = null;
    // Grab 15 px in: the first 6 px are the trim edge.
    await drag(page, r2.x + 15, y, target + 15, y, {
      before: async () => {
        guide = await page.evaluate(() => window.renderlyTimeline["snapLine"]);
      },
    });
    s = await state(page);
    const landed = clips(s).find((c) => c.id === c2.id);
    check(guide === c1.start + c1.duration, `the guide marks clip 1's end: ${guide}`);
    check(landed.start === c1.start + c1.duration, `clip 2 snapped: ${landed.start / SECOND}`);
    return `${selected} clips box-selected with auto-scroll; snap guide shown`;
  },

  async "the viewer plays the timeline: play, pause, frame step, cuts and loop"({ context }) {
    const page = await playbackProject(context, "Transport");
    const t = (seconds) => Math.round(seconds * SECOND);
    const view = () =>
      page.evaluate(() => {
        const v = window.renderlyTimeline["options"].view.get();
        return { playhead: v.playhead, playing: v.playing };
      });

    // Play from 0 for a second; the playhead moves and the engine reports playing.
    await page.getByRole("button", { name: "Play", exact: true }).click();
    await page.waitForFunction(() => window.renderlyViewer.preview.readPlayhead().playing);
    await page.waitForTimeout(1000);
    let v = await view();
    check(v.playing && v.playhead > t(0.3), `playing, playhead ${v.playhead / SECOND} s`);
    // Pause rests on a frame start.
    await page.getByRole("button", { name: "Pause", exact: true }).click();
    await page.waitForFunction(() => !window.renderlyViewer.preview.readPlayhead().playing);
    v = await view();
    check(!v.playing && v.playhead % FRAME === 0, `paused on a frame: ${v.playhead / FRAME}`);
    const timecode = await page.locator(".viewer__timecode").textContent();
    check(/^\d\d:\d\d:\d\d:\d\d$/.test(timecode ?? ""), `timecode shown: ${timecode}`);

    // Frame step, then the next cut (the second clip starts at 4 s on the bottom track).
    const before = v.playhead;
    await page.getByRole("button", { name: "Step forward one frame" }).click();
    v = await view();
    check(v.playhead === before + FRAME, "one frame forward");
    await page.getByRole("button", { name: "Go to the next cut" }).click();
    v = await view();
    check(v.playhead === t(2), `next cut at 2 s (top clip starts): ${v.playhead / SECOND}`);
    await page.getByRole("button", { name: "Go to the next cut" }).click();
    v = await view();
    check(v.playhead === t(3), `next cut at 3 s (top clip ends): ${v.playhead / SECOND}`);
    await page.getByRole("button", { name: "Go to the next cut" }).click();
    v = await view();
    check(v.playhead === t(4), `next cut at 4 s (second file): ${v.playhead / SECOND}`);
    await page.getByRole("button", { name: "Go to the previous cut" }).click();
    v = await view();
    check(v.playhead === t(3), `previous cut at 3 s: ${v.playhead / SECOND}`);

    // Loop the selected 1-second clip on the top track: the playhead wraps back into it.
    await page.evaluate(() => {
      const tl = window.renderlyTimeline;
      const top = tl.sequence.tracks[1].clips[0];
      tl["options"].view.set({ selection: [top.id], loop: true });
    });
    await page.getByRole("button", { name: "Play", exact: true }).click();
    // Watch until it has wrapped twice (a slow machine takes longer to start each pass), at most 8 s.
    const seen = await page.evaluate(async () => {
      const times = [];
      let wraps = 0;
      const end = performance.now() + 8000;
      while (performance.now() < end && wraps < 2) {
        const t = window.renderlyTimeline["options"].view.get().playhead;
        if (times.length && t < times[times.length - 1]) wraps++;
        times.push(t);
        await new Promise((r) => requestAnimationFrame(r));
      }
      return times;
    });
    await page.getByRole("button", { name: "Pause", exact: true }).click();
    const wraps = seen.filter((time, i) => i > 0 && time < seen[i - 1]).length;
    if (wraps === 0)
      log(
        "  debug: loop",
        JSON.stringify({
          samples: seen.length,
          first: seen[0] / SECOND,
          last: seen.at(-1) / SECOND,
          min: Math.min(...seen) / SECOND,
          max: Math.max(...seen) / SECOND,
          engine: await page.evaluate(() => window.renderlyViewer.preview.readPlayhead()),
          stats: await page.evaluate(() => {
            const st = window.renderlyViewer.stats;
            return (
              st && {
                playing: st.playing,
                audioClock: st.audioClock,
                startLatencyMs: st.startLatencyMs,
                presented: st.presented,
                late: st.late,
                fps: st.fps,
              }
            );
          }),
          errors: await page.evaluate(() => window.renderlyViewer.errors),
          view: await view(),
        }),
      );
    check(wraps >= 1, `looped back ${wraps} times`);
    check(
      seen.every((time) => time >= t(2) - FRAME && time <= t(3) + FRAME),
      `stayed in the 2-3 s loop: ${Math.min(...seen) / SECOND}-${Math.max(...seen) / SECOND}`,
    );

    // Quick playhead jumps, then Play at once: seeks still queued must not stop the playback.
    await page.evaluate(() =>
      window.renderlyTimeline["options"].view.set({ loop: false, selection: [] }),
    );
    await page.evaluate(async (S) => {
      const view = window.renderlyTimeline["options"].view;
      for (let i = 0; i < 20; i++) view.set({ playhead: Math.round(((i % 7) + 0.5) * S) });
      window.renderlyViewer.play();
    }, SECOND);
    await page.waitForTimeout(2500);
    const afterJumps = await page.evaluate(() => ({
      engine: window.renderlyViewer.preview.readPlayhead(),
      view: window.renderlyTimeline["options"].view.get().playing,
    }));
    check(
      afterJumps.engine.playing && afterJumps.view,
      `still playing after quick seeks then Play: ${JSON.stringify(afterJumps)}`,
    );
    await page.getByRole("button", { name: "Pause", exact: true }).click();
    await page.waitForFunction(() => !window.renderlyViewer.preview.readPlayhead().playing);

    // Playing to the end stops there.
    await page.evaluate(() => window.renderlyTimeline["options"].view.set({ loop: false }));
    await page.evaluate((t) => window.renderlyTimeline["options"].view.set({ playhead: t }), t(7));
    await page.getByRole("button", { name: "Play", exact: true }).click();
    await page.waitForFunction(() => !window.renderlyTimeline["options"].view.get().playing, null, {
      timeout: 10_000,
    });
    v = await view();
    check(Math.abs(v.playhead - t(8)) <= 2 * FRAME, `stopped at the end: ${v.playhead / SECOND}`);
    const errors = await page.evaluate(() => window.renderlyViewer.errors);
    check(errors.length === 0, `no engine errors: ${errors.join("; ")}`);
    return `${wraps} loop wraps; stopped at ${(v.playhead / SECOND).toFixed(2)} s`;
  },

  async "edits during playback reach the engine within a frame and never break it"({ context }) {
    const page = await playbackProject(context, "Live edits");
    await page.getByRole("button", { name: "Play", exact: true }).click();
    await page.waitForFunction(() => window.renderlyViewer.preview.readPlayhead().playing);
    // 40 edits, one every 100 ms: moves, trims, splits, hiding a track, deletes, undos and redos.
    const done = await page.evaluate(async () => {
      const tl = window.renderlyTimeline;
      const { store, view } = tl["options"];
      const S = 705_600_000;
      const F = S / 30;
      const ran = [];
      for (let i = 0; i < 40; i++) {
        if (!view.get().playing) {
          view.set({ playhead: 0 });
          window.renderlyViewer.play();
          await new Promise((r) => setTimeout(r, 150));
        }
        const clips = tl.sequence.tracks.flatMap((t) => t.clips);
        const clip = clips[i % Math.max(1, clips.length)];
        const kind = i % 8;
        let result;
        if (kind === 6) result = store.undo();
        else if (kind === 7) result = store.redo();
        else if (!clip) result = store.undo();
        else if (kind === 0)
          result = store.dispatch({
            type: "clip.move",
            payload: { clips: [clip.id], delta: (i % 2 ? 1 : -1) * 6 * F },
          });
        else if (kind === 1)
          result = store.dispatch({
            type: "clip.trim",
            payload: { clips: [clip.id], edge: "end", delta: -3 * F },
          });
        else if (kind === 2)
          result = store.dispatch({
            type: "clip.split",
            payload: { clips: [clip.id], t: clip.start + 10 * F },
          });
        else if (kind === 3) {
          const track = tl.sequence.tracks[1];
          result = store.dispatch({
            type: "track.set",
            payload: { tracks: [track.id], hidden: !track.hidden },
          });
        } else if (kind === 4)
          result = store.dispatch({
            type: "clip.trim",
            payload: { clips: [clip.id], edge: "start", delta: 2 * F },
          });
        else result = store.dispatch({ type: "clip.delete", payload: { clips: [clip.id] } });
        ran.push(result.ok);
        await new Promise((r) => setTimeout(r, 100));
      }
      return ran;
    });
    await page.waitForTimeout(500);
    await page.evaluate(() => window.renderlyViewer.pause());
    await page.waitForTimeout(1500); // a stats report after stopping
    const result = await page.evaluate(() => {
      const v = window.renderlyViewer;
      return {
        refreshes: v.updateRefreshes,
        errors: v.errors,
        stats: v.stats && { framesOutstanding: v.stats.framesOutstanding, queued: v.stats.queued },
      };
    });
    const applied = done.filter(Boolean).length;
    const r = [...result.refreshes].sort((a, b) => a - b);
    const median = r[Math.floor(r.length / 2)];
    const worst = r[r.length - 1];
    check(applied >= 30, `most edits applied: ${applied} of 40`);
    check(r.length >= 20, `edits reached the engine while it played: ${r.length}`);
    check(result.errors.length === 0, `no engine errors: ${result.errors.join("; ")}`);
    check(median <= 1, `median edit shown on the next frame: ${median} refreshes`);
    check(
      result.stats?.framesOutstanding === 0 && result.stats.queued === 0,
      `no frames left on loan after stopping: ${JSON.stringify(result.stats)}`,
    );
    return `${applied} edits during playback; ${r.length} reached the engine while playing, median ${median} refresh, worst ${worst}`;
  },

  async "trimming shows the edge frame in the viewer"({ context }) {
    const page = await playbackProject(context, "Trim peek");
    const bottom = await page.evaluate(
      () => window.renderlyTimeline.sequence.tracks[0].clips[0].id,
    );
    const r = await box(page, bottom);
    const y = r.y + r.height / 2;
    const oneSecond = (await pageX(page, SECOND)) - (await pageX(page, 0));
    let during = null;
    await drag(page, r.x + r.width - 2, y, r.x + r.width - 2 - oneSecond, y, {
      before: async () => {
        await page.waitForTimeout(400);
        during = await page.evaluate(() => ({
          peek: window.renderlyTimeline["options"].view.get().peek,
          engine: window.renderlyViewer.preview.readPlayhead().time,
          end: window.renderlyTimeline.sequence.tracks[0].clips[0],
        }));
      },
    });
    const expected = during.end.start + during.end.duration - FRAME;
    check(
      during.peek === expected,
      `peek is the last frame of the trimmed clip: ${during.peek / FRAME}`,
    );
    check(during.engine === expected, `the engine shows it: ${during.engine / FRAME} (frames)`);
    await page.waitForFunction(() => window.renderlyTimeline["options"].view.get().peek === null);
    await page.waitForTimeout(500);
    const after = await page.evaluate(() => ({
      playhead: window.renderlyTimeline["options"].view.get().playhead,
      engine: window.renderlyViewer.preview.readPlayhead().time,
    }));
    check(after.engine === after.playhead, `back to the playhead after the trim: ${after.engine}`);
    return `edge frame ${(expected / FRAME).toFixed(0)} shown while trimming`;
  },

  async "500 clips redraw in under 2 ms"({ context }) {
    const lab = await openLab(context);
    const folder = await lab.evaluate(async () => {
      await mediaLab.makeClip("d.webm", 1, "d");
      const folder = await mediaLab.createProject("Redraw");
      await mediaLab.open(folder);
      const { imported } = await mediaLab.importOutside(["d.webm"]);
      for (let i = 0; i < 5; i++) mediaLab.addClips(imported[0].id, 100);
      await mediaLab.close();
      return folder;
    });
    await lab.close();
    const page = await openEditor(context, folder);
    await page.setViewportSize({ width: 1920, height: 1080 });
    await page.waitForTimeout(300);
    const measure = (zoom) =>
      page.evaluate(async (zoom) => {
        const t = window.renderlyTimeline;
        if (zoom === null) t.fit();
        else t.setZoom(zoom, 0);
        // Let thumbnails for this view load, so the strips are drawn too.
        const deadline = performance.now() + 5000;
        while (t.thumbnailStats.pending > 0 && performance.now() < deadline)
          await new Promise((r) => setTimeout(r, 50));
        let visible = 0;
        const view = t["viewport"];
        const to = view.t + t.canvas.clientWidth * view.zoom;
        for (const track of t.sequence.tracks)
          for (const c of track.clips) if (c.start < to && c.start + c.duration > view.t) visible++;
        // One full redraw per display frame, as the app does (it never draws twice in a frame).
        const raw = [];
        for (let i = 0; i < 400; i++) {
          await new Promise((r) => requestAnimationFrame(r));
          raw.push(t.redraw());
        }
        const slowest = raw
          .map((ms, i) => [i, ms])
          .sort((a, b) => b[1] - a[1])
          .slice(0, 3)
          .map(([i, ms]) => `#${i} ${ms.toFixed(1)}`);
        const times = [...raw].sort((a, b) => a - b);
        return {
          visible,
          median: times[200],
          p95: times[380],
          max: times[399],
          slowest,
          thumbs: t.thumbnailStats.cached,
        };
      }, zoom);
    const all = await measure(null);
    const close = await measure(SECOND / 150);
    const f = (n) => n.toFixed(3);
    log(
      `  all ${all.visible} clips: median ${f(all.median)} ms, p95 ${f(all.p95)} ms, max ${f(all.max)} ms (slowest ${all.slowest})`,
    );
    log(
      `  ${close.visible} clips with filmstrips (${close.thumbs} tiles cached): median ${f(close.median)} ms, p95 ${f(close.p95)} ms, max ${f(close.max)} ms (slowest ${close.slowest})`,
    );
    check(all.visible === 500, `all 500 clips on screen: ${all.visible}`);
    // The gate is the 95th percentile: single frames can be slow when the browser does other work.
    check(all.p95 < 2, `500 clips: p95 ${f(all.p95)} ms < 2 ms`);
    check(close.p95 < 2, `filmstrips: p95 ${f(close.p95)} ms < 2 ms`);
    return `500 clips p95 ${f(all.p95)} ms (median ${f(all.median)}, max ${f(all.max)})`;
  },
};

async function main() {
  log(`starting dev server; browser: ${channel}`);
  const server = await startServer();
  const browser = await chromium.launch({ channel, headless: true });
  let failed = 0;
  try {
    for (const [name, run] of Object.entries(scenarios)) {
      if (only && !name.includes(only)) continue;
      const context = await browser.newContext({ viewport: { width: 1600, height: 900 } });
      const reset = await openLab(context);
      await reset.evaluate(() => mediaLab.reset());
      await reset.close();
      const started = Date.now();
      try {
        const detail = await run({ context });
        log(`PASS ${name} (${Date.now() - started} ms)${detail ? ` — ${detail}` : ""}`);
      } catch (error) {
        failed++;
        log(`FAIL ${name}: ${error instanceof Error ? error.message : String(error)}`);
      } finally {
        await context.close();
      }
    }
  } finally {
    await browser.close();
    stop(server);
  }
  log(failed === 0 ? "all checks passed" : `${failed} check(s) failed`);
  process.exit(failed === 0 ? 0 : 1);
}

await main();
