// S1.6 media checks in a real browser. The media lab (apps/editor/media-lab.html, dev only) makes
// small WebM files in the page and keeps "outside" files in browser storage, so moving and
// relinking can be tested without a person; the editor itself then shows the result.
//
//   pnpm test:media                    installed Edge
//   pnpm test:media --browser chrome   installed Chrome (CI)
//   pnpm test:media --big              also import ≥13 GB of test-media/ files and time it

/* global mediaLab -- the media lab page's test hooks, used inside page.evaluate() */

import { execSync, spawn } from "node:child_process";
import { existsSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { chromium } from "playwright-core";

const arg = (name, fallback) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : fallback;
};
const channel = arg("browser", "msedge");
const only = arg("only", null);
const big = process.argv.includes("--big");
const PORT = 5182;
const BASE = `http://localhost:${PORT}`;
const root = new URL("..", import.meta.url);

const log = (...parts) => console.log(new Date().toISOString().slice(11, 19), ...parts);

async function startServer() {
  // A server left on the port from an earlier, interrupted run would answer instead of ours (and
  // ours would fail to start without a word): refuse rather than test the wrong server.
  if (
    await fetch(`${BASE}/`).then(
      () => true,
      () => false,
    )
  )
    throw new Error(
      `Port ${PORT} is already in use, probably by a dev server from an earlier run. Stop it and try again.`,
    );
  const server = spawn(`pnpm --filter @renderly/editor exec vite --port ${PORT} --strictPort`, {
    detached: process.platform !== "win32",
    cwd: root,
    shell: true,
    stdio: "ignore",
  });
  // Stop the server if this run is interrupted (Ctrl+C or a kill), so it does not outlive us.
  for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"])
    process.once(signal, () => void stop(server).then(() => process.exit(130)));
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

/**
 * Stops the dev server and everything it started, and waits until its port is free, so the next
 * run can start its own server on it.
 */
async function stop(server) {
  if (process.platform === "win32") {
    // Killing only the shell would leave Vite running; taskkill /T takes the whole tree.
    try {
      execSync(`taskkill /pid ${server.pid} /T /F`, { stdio: "ignore" });
    } catch {
      // already gone
    }
  } else {
    // The server runs in its own process group (see startServer): stop all of it.
    try {
      process.kill(-server.pid, "SIGTERM");
    } catch {
      server.kill();
    }
  }
  const deadline = Date.now() + 10_000;
  while (
    Date.now() < deadline &&
    (await fetch(`${BASE}/`).then(
      () => true,
      () => false,
    ))
  )
    await new Promise((resolve) => setTimeout(resolve, 200));
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

const scenarios = {
  async "import probes and links files, and refuses what it cannot read"({ context }) {
    const page = await openLab(context);
    const result = await page.evaluate(async () => {
      for (const name of ["a.webm", "b.webm", "c.webm"]) await mediaLab.makeClip(name, 1);
      await mediaLab.writeGarbage("notes.mp4");
      await mediaLab.open(await mediaLab.createProject("Import"));
      const imported = await mediaLab.importOutside(["a.webm", "b.webm", "c.webm", "notes.mp4"]);
      return { imported, statuses: await mediaLab.statuses() };
    });
    check(
      result.imported.imported.length === 3,
      `3 imported (got ${result.imported.imported.length})`,
    );
    check(
      result.imported.failed.length === 1 && result.imported.failed[0].name === "notes.mp4",
      "the garbage file is refused",
    );
    const first = result.imported.imported[0];
    check(
      first.link === "handle" && first.video.codec === "vp9" && first.audio.codec === "opus",
      "linked, with video and audio described",
    );
    check(
      first.video.width === 320 &&
        first.video.height === 180 &&
        first.video.fps.join("/") === "30/1",
      "picture size and rate",
    );
    check(
      result.statuses.every((s) => s.status === "online"),
      "all online",
    );
    return `vp9 ${first.video.width}×${first.video.height} @ ${first.video.fps.join("/")}, opus ${first.audio.sampleRate} Hz; refused: ${result.imported.failed[0].reason}`;
  },

  async "copy into project streams the file with progress"({ context }) {
    const page = await openLab(context);
    const result = await page.evaluate(async () => {
      await mediaLab.makeClip("long.webm", 6);
      await mediaLab.open(await mediaLab.createProject("Copy"));
      const imported = await mediaLab.importOutside(["long.webm"], true);
      // The original can go: the project has its own copy.
      await mediaLab.moveOutside("long.webm", "elsewhere/long.webm");
      return { imported, statuses: await mediaLab.statuses() };
    });
    const item = result.imported.imported[0];
    check(
      item.link === "copy" && item.path === "media/long.webm",
      `copied to media/ (${item.link}, ${item.path})`,
    );
    check(
      result.imported.progressSteps >= 1 && result.imported.lastProgress === 1,
      "progress reaches 100%",
    );
    check(result.statuses[0].status === "online", "the copy stays online when the original moves");
    return `${result.imported.progressSteps} progress updates`;
  },

  async "a moved file goes offline, and relinking restores every clip"({ context }) {
    const page = await openLab(context);
    const setup = await page.evaluate(async () => {
      await mediaLab.makeClip("one.webm", 1, "one");
      await mediaLab.makeClip("two.webm", 1, "two");
      const folder = await mediaLab.createProject("Relink");
      await mediaLab.open(folder);
      const { imported } = await mediaLab.importOutside(["one.webm", "two.webm"]);
      mediaLab.addClips(imported[0].id, 5);
      mediaLab.addClips(imported[1].id, 3);
      await mediaLab.close();
      return { folder, ids: imported.map((m) => m.id) };
    });
    // Reopen in a fresh page, as after a reload, then move one file and rename the other.
    await page.reload();
    await page.waitForFunction(() => window.labReadyAt !== undefined);
    const offline = await page.evaluate(async (folder) => {
      await mediaLab.moveOutside("one.webm", "Archive/one.webm");
      await mediaLab.moveOutside("two.webm", "Archive/2024/two-renamed.webm");
      await mediaLab.open(folder);
      return { statuses: await mediaLab.statuses(), clips: await mediaLab.clipStates() };
    }, setup.folder);
    check(
      offline.statuses.every((s) => s.status === "offline" && s.reason === "missing"),
      `both offline: ${JSON.stringify(offline.statuses)}`,
    );
    check(
      offline.clips.offline === 8 && offline.clips.online === 0,
      `8 clips offline (${JSON.stringify(offline.clips)})`,
    );

    // The editor shows it: two Offline badges and the notice.
    const editor = await context.newPage();
    await page.evaluate(() => mediaLab.close());
    await editor.goto(`${BASE}/#/project/browser/${setup.folder}`);
    await editor.waitForFunction(
      () => document.querySelectorAll(".r-bin__item .r-badge--warning").length === 2,
      null,
      { timeout: 15_000 },
    );
    const notice = await editor.evaluate(
      () => document.querySelector(".r-bin__notice")?.textContent ?? "",
    );
    check(notice.includes("2 files are offline"), `the bin says so: ${notice}`);
    await editor.close();

    const relinked = await page.evaluate(async (folder) => {
      await mediaLab.open(folder);
      // A different file is refused, by size and fingerprint.
      await mediaLab.makeClip("impostor.webm", 2, "impostor");
      const wrong = await mediaLab.relinkOne(mediaLab.media()[0].id, "impostor.webm");
      const ids = await mediaLab.relinkFromOutside("Archive");
      return {
        wrong,
        ids,
        statuses: await mediaLab.statuses(),
        clips: await mediaLab.clipStates(),
      };
    }, setup.folder);
    check(
      !relinked.wrong.ok && relinked.wrong.reason === "different-file",
      "a different file is refused",
    );
    check(relinked.ids.length === 2, `both relinked from the folder (${relinked.ids.length})`);
    check(
      relinked.statuses.every((s) => s.status === "online"),
      "both online again",
    );
    check(
      relinked.clips.online === 8 && relinked.clips.offline === 0,
      `all 8 clips restored (${JSON.stringify(relinked.clips)})`,
    );
    return "2 files moved (one renamed into a subfolder) → 8 clips offline → relinked from the folder → 8 online";
  },

  async "a different file in the old place is not accepted silently"({ context }) {
    const page = await openLab(context);
    const status = await page.evaluate(async () => {
      await mediaLab.makeClip("same-name.webm", 1, "first");
      await mediaLab.open(await mediaLab.createProject("Changed"));
      await mediaLab.importOutside(["same-name.webm"]);
      await mediaLab.makeClip("same-name.webm", 2, "second"); // overwritten by another file
      return mediaLab.statuses();
    });
    check(
      status[0].status === "offline" && status[0].reason === "changed",
      `offline (changed): ${JSON.stringify(status)}`,
    );
  },
};

/** ≥13 GB of real files: probe, fingerprint, thumbnail and command for each, timed. */
async function bigImport({ context }) {
  const dir = new URL("test-media/", root);
  if (!existsSync(dir))
    throw new Failure("test-media/ is missing (node scripts/make-test-media.mjs)");
  const files = readdirSync(dir)
    .filter((name) => name.endsWith(".mp4"))
    .map((name) => join(dir.pathname.replace(/^\/([A-Z]:)/, "$1"), name));
  // Add the largest again until the total passes 13 GB (the same file twice is two imports).
  const largest = [...files].sort((a, b) => statSync(b).size - statSync(a).size)[0];
  const chosen = [...files];
  let total = chosen.reduce((n, f) => n + statSync(f).size, 0);
  while (total < 13e9) {
    chosen.push(largest);
    total += statSync(largest).size;
  }
  const page = await openLab(context);
  await page.evaluate(async () => mediaLab.open(await mediaLab.createProject("Big import")));
  await page.setInputFiles("#files", chosen);
  const result = await page.evaluate(() => mediaLab.importChosen());
  check(
    result.imported === chosen.length,
    `all imported (${result.imported} of ${chosen.length}): ${JSON.stringify(result.failed)}`,
  );
  check(result.ms < 2000, `import took ${Math.round(result.ms)} ms (target under 2,000)`);
  check(
    result.heapPeakMB < 50,
    `heap peaked at ${result.heapPeakMB.toFixed(1)} MB (target under 50)`,
  );
  return `${result.files} files, ${(result.bytes / 1e9).toFixed(2)} GB in ${Math.round(result.ms)} ms; heap ${result.heapBeforeMB.toFixed(1)} → peak ${result.heapPeakMB.toFixed(1)} MB`;
}

async function main() {
  log(`starting dev server; browser: ${channel}`);
  const server = await startServer();
  const browser = await chromium.launch({
    channel,
    headless: true,
    args: ["--enable-precise-memory-info"],
  });
  const runs = Object.entries(scenarios);
  if (big)
    runs.push([
      "importing ≥13 GB of files takes under 2 s and keeps the heap under 50 MB",
      bigImport,
    ]);
  let failed = 0;
  try {
    for (const [name, run] of runs) {
      if (only && !name.includes(only)) continue;
      const context = await browser.newContext();
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
    await stop(server);
  }
  log(failed === 0 ? "all checks passed" : `${failed} check(s) failed`);
  process.exit(failed === 0 ? 0 : 1);
}

await main();
