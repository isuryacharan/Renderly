// S1.7 timeline checks in a real browser, driven with the mouse and keyboard: drag from the bin,
// move, trim, split, delete, ripple delete, the blade tool, box select with edge auto-scroll,
// snapping, one undo step per drag and Escape cancelling one; and the redraw benchmark (500 clips
// in under 2 ms). The media lab makes the test files (see media-e2e.mjs).
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
const PORT = 5183;
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
