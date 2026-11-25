// S1.4 storage scenarios in a real browser, in browser storage (OPFS): Playwright drives the
// storage lab page (apps/spike/storage.html) in one or two tabs.
//
//   pnpm test:storage                  installed Edge
//   pnpm test:storage --browser chrome installed Chrome (CI)

/* global lab -- the storage lab page's test hooks, used inside page.evaluate() */

import { spawn } from "node:child_process";
import { chromium } from "playwright-core";

const arg = (name, fallback) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : fallback;
};
const channel = arg("browser", "msedge");
const only = arg("only", null);
const PORT = 5198;
const BASE = `http://localhost:${PORT}`;

const step = (...parts) => process.env.STEPS && log("  ·", ...parts);
const log = (...parts) => console.log(new Date().toISOString().slice(11, 19), ...parts);

async function startServer() {
  const server = spawn(`pnpm --filter @renderly/spike exec vite --port ${PORT} --strictPort`, {
    cwd: new URL("..", import.meta.url),
    shell: true,
    stdio: "ignore",
  });
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    try {
      if ((await fetch(`${BASE}/storage.html`)).ok) return server;
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

async function openTab(context) {
  const page = await context.newPage();
  page.on("pageerror", (error) => log("page error:", error.message));
  await page.goto(`${BASE}/storage.html`);
  await page.waitForFunction(() => window.pageReadyAt !== undefined);
  return page;
}

class Failure extends Error {}
function check(condition, message) {
  if (!condition) throw new Failure(message);
}
const until = (page, fn, arg, timeout = 10_000) =>
  page.waitForFunction(fn, arg, { timeout, polling: 50 });
const saved = (page) =>
  until(page, () => lab.status() === "saved" && lab.savedRevision() === lab.revision());

const scenarios = {
  async "saves edits to project.json and the meta file"({ context }) {
    const page = await openTab(context);
    const folder = await page.evaluate(() => lab.create("Alpha"));
    check((await page.evaluate((f) => lab.open(f), folder)) === "writer", "should be the writer");
    await page.evaluate(() => lab.rename("Alpha 2"));
    await saved(page);
    const disk = await page.evaluate((f) => lab.readDisk(f), folder);
    check(disk.includes('"name": "Alpha 2"'), "project.json has the new name");
    const list = await page.evaluate(() => lab.list());
    check(list[0]?.meta.name === "Alpha 2", "the meta file has the new name");
    const save = await page.evaluate(
      () => lab.events.findLast((e) => e.type === "save" && e.save.status === "saved")?.save,
    );
    return `saved r${save.savedRevision}, ${save.bytes} bytes in ${save.ms.toFixed(1)} ms`;
  },

  async "reload reopens the saved project"({ context }) {
    const page = await openTab(context);
    const folder = await page.evaluate(() => lab.create("Reload"));
    await page.evaluate((f) => lab.open(f), folder);
    await page.evaluate(() => lab.rename("After reload"));
    await saved(page);
    await page.reload();
    await page.waitForFunction(() => window.pageReadyAt !== undefined);
    check((await page.evaluate((f) => lab.open(f), folder)) === "writer", "writer after reload");
    check((await page.evaluate(() => lab.name())) === "After reload", "name survived");
  },

  async "reload inside the debounce window keeps the edit"({ context }) {
    const ways = [];
    for (const flushOnHide of [true, false])
      ways.push(await reloadInsideDebounce(context, flushOnHide));
    return ways.join("; ");
  },

  async "a kept unsaved copy is dropped if the disk changed meanwhile"({ context }) {
    const page = await openTab(context);
    const folder = await page.evaluate(() => lab.create("Stale"));
    await page.evaluate((f) => lab.open(f, { debounce: 60_000 }, false), folder);
    await page.evaluate(() => lab.rename("Kept copy"));
    await page.reload();
    await page.waitForFunction(() => window.pageReadyAt !== undefined);
    await page.evaluate((f) => lab.writeOutside(f, "Changed outside"), folder);
    await page.evaluate((f) => lab.open(f), folder);
    check((await page.evaluate(() => lab.name())) === "Changed outside", "the disk version wins");
    check(!(await page.evaluate(() => lab.recovered() !== null)), "nothing recovered");
  },
};

/** Edit, reload before the debounce fires, reopen: the edit must be there. */
async function reloadInsideDebounce(context, flushOnHide) {
  const page = await openTab(context);
  const folder = await page.evaluate((n) => lab.create(n), `Quick ${flushOnHide}`);
  await page.evaluate(
    ([f, flush]) => lab.open(f, { debounce: 5000 }, flush),
    [folder, flushOnHide],
  );
  await page.evaluate(() => lab.rename("Quick edit"));
  check(
    (await page.evaluate(() => lab.status())) === "unsaved",
    "edit is waiting for the debounce",
  );
  await page.reload(); // pagehide flushes
  await page.waitForFunction(() => window.pageReadyAt !== undefined);
  await page.evaluate((f) => lab.open(f), folder);
  check((await page.evaluate(() => lab.name())) === "Quick edit", "the edit survived the reload");
  const recovered = await page.evaluate(() => lab.recovered() !== null);
  await saved(page);
  const disk = await page.evaluate((f) => lab.readDisk(f), folder);
  check(disk.includes('"name": "Quick edit"'), "and is written to disk");
  if (!flushOnHide) check(recovered, "with the flush off, the kept copy must have been adopted");
  const how = recovered ? "kept in localStorage, adopted on open" : "the pagehide flush finished";
  return `flush ${flushOnHide ? "on" : "off"}: ${how}`;
}

Object.assign(scenarios, {
  async "a second tab follows read-only"({ context }) {
    const one = await openTab(context);
    const folder = await one.evaluate(() => lab.create("Two tabs"));
    await one.evaluate((f) => lab.open(f), folder);
    const two = await openTab(context);
    check((await two.evaluate((f) => lab.open(f), folder)) === "follower", "second tab follows");
    const refused = await two.evaluate(() => lab.rename("Nope"));
    check(!refused.ok && refused.error.code === "read-only", "follower edits are refused");
    await one.evaluate(() => lab.rename("Shared"));
    await until(two, () => lab.name() === "Shared");
    check(
      (await two.evaluate(() => lab.revision())) === (await one.evaluate(() => lab.revision())),
      "same revision",
    );
  },

  async "take-over keeps the writer's unsaved edit"({ context }) {
    const one = await openTab(context);
    const folder = await one.evaluate(() => lab.create("Take over"));
    await one.evaluate((f) => lab.open(f, { debounce: 60_000 }), folder);
    const two = await openTab(context);
    await two.evaluate((f) => lab.open(f), folder);
    await one.evaluate(() => lab.rename("Unsaved in one"));
    check((await one.evaluate(() => lab.status())) === "unsaved", "tab one has an unsaved edit");
    check((await two.evaluate(() => lab.takeOver())) === "writer", "tab two took over");
    check((await two.evaluate(() => lab.name())) === "Unsaved in one", "tab two has the edit");
    await until(one, () => lab.role() === "follower");
    await two.evaluate(() => lab.rename("By two"));
    await saved(two);
    await until(one, () => lab.name() === "By two");
    const disk = await two.evaluate((f) => lab.readDisk(f), folder);
    check(disk.includes('"name": "By two"'), "tab two's edit is on disk");
  },

  async "when the writer closes, a follower becomes the writer"({ context }) {
    const one = await openTab(context);
    const folder = await one.evaluate(() => lab.create("Hand over"));
    await one.evaluate((f) => lab.open(f), folder);
    const two = await openTab(context);
    await two.evaluate((f) => lab.open(f), folder);
    await one.close(); // the tab goes away; the browser releases its lock
    await until(two, () => lab.role() === "writer");
    await two.evaluate(() => lab.rename("Written by two"));
    await saved(two);
    // And a graceful close (the Close button) hands over too.
    const three = await openTab(context);
    await three.evaluate((f) => lab.open(f), folder);
    await two.evaluate(() => lab.close());
    await until(three, () => lab.role() === "writer");
    check(
      (await three.evaluate(() => lab.name())) === "Written by two",
      "the new writer has the latest save",
    );
  },

  async "an outside write is never overwritten silently"({ context }) {
    const page = await openTab(context);
    const folder = await page.evaluate(() => lab.create("Outside"));
    await page.evaluate((f) => lab.open(f, { debounce: 100 }), folder);
    await page.evaluate((f) => lab.writeOutside(f, "Changed outside"), folder);
    await page.evaluate(() => lab.rename("Mine"));
    await until(page, () => lab.status() === "conflict");
    let disk = await page.evaluate((f) => lab.readDisk(f), folder);
    check(disk.includes('"name": "Changed outside"'), "the outside version is untouched");
    await page.evaluate(() => lab.resolve("mine"));
    disk = await page.evaluate((f) => lab.readDisk(f), folder);
    check(disk.includes('"name": "Mine"'), "keep mine writes ours");

    const before = await page.evaluate(() => lab.revision());
    await page.evaluate((f) => lab.writeOutside(f, "Changed again"), folder);
    check(await page.evaluate(() => lab.checkOutside()), "the change is noticed");
    const result = await page.evaluate(() => lab.resolve("disk"));
    check(result.ok, "the disk version loads");
    check(
      (await page.evaluate(() => lab.name())) === "Changed again",
      "load disk version loads it",
    );
    check((await page.evaluate(() => lab.revision())) > before, "the revision still goes up");
    check((await page.evaluate(() => lab.status())) === "saved", "no conflict left");
  },

  async "killing the tab mid-save leaves a loadable project"({ context }) {
    const setup = await openTab(context);
    step("creating a big project");
    const folder = await setup.evaluate(() => lab.create("Crash", 200_000));
    const size = (await setup.evaluate((f) => lab.load(f), folder)).bytes;
    await setup.close();
    const results = [];
    let midWrite = 0;
    for (let round = 0; round < 6; round++) {
      const page = await openTab(context);
      await page.evaluate((f) => lab.open(f, { debounce: 0, backups: null }), folder);
      step(`round ${round}: rename`);
      await page.evaluate((n) => void lab.rename(`Round ${n}`), round);
      // Crash as soon as the write has started, a little later each round.
      await page.waitForFunction(() => lab.status() !== "unsaved", null, {
        polling: 1,
        timeout: 20_000,
      });
      await new Promise((r) => setTimeout(r, round * 8));
      const status = await page.evaluate(() => lab.status());
      step(`round ${round}: crash while ${status}`);
      const cdp = await context.newCDPSession(page);
      const crashed = page.waitForEvent("crash", { timeout: 10_000 });
      void cdp.send("Page.crash").catch(() => undefined); // never answers: the renderer is gone
      await crashed;
      await page.close().catch(() => undefined);
      const reader = await openTab(context);
      const loaded = await reader.evaluate((f) => lab.load(f), folder);
      await reader.close();
      if (status === "saving") midWrite++;
      results.push(`${status} → ${loaded.ok ? `"${loaded.name}"` : "BROKEN"}`);
      if (!loaded.ok)
        throw new Failure(`round ${round}: project.json did not load after the crash`);
    }
    check(
      midWrite >= 3,
      `only ${midWrite} of 6 crashes hit a write in progress: ${results.join(", ")}`,
    );
    return `${(size / 1e6).toFixed(1)} MB file; ${results.join(", ")}`;
  },

  async "backups roll and restore"({ context }) {
    const page = await openTab(context);
    const folder = await page.evaluate(() => lab.create("Backups"));
    await page.evaluate(
      (f) => lab.open(f, { debounce: 0, backups: { interval: 0, recent: 3, days: 0 } }),
      folder,
    );
    for (let i = 1; i <= 5; i++) {
      await page.evaluate((n) => lab.rename(`Version ${n}`), i);
      await saved(page);
      await new Promise((r) => setTimeout(r, 5)); // distinct backup times
    }
    const backups = await page.evaluate((f) => lab.backups(f), folder);
    check(backups.length === 3, `3 backups kept (found ${backups.length})`);
    const oldest = backups.at(-1);
    const before = await page.evaluate(() => lab.revision());
    await page.evaluate((name) => lab.restore(name), oldest.name);
    await saved(page);
    check(
      (await page.evaluate(() => lab.name())) === "Version 3",
      "restored the oldest kept backup",
    );
    check((await page.evaluate(() => lab.revision())) > before, "the revision still goes up");
    const disk = await page.evaluate((f) => lab.readDisk(f), folder);
    check(disk.includes('"name": "Version 3"'), "the restore is saved");
  },

  async "persistence and quota can be read"({ context }) {
    const page = await openTab(context);
    const quota = await page.evaluate(() => lab.quota());
    check(quota.quota > 0, "the browser reports a quota");
    return `usage ${(quota.usage / 1e6).toFixed(1)} MB of ${(quota.quota / 1e9).toFixed(1)} GB, persistence ${quota.granted ? "granted" : "not granted (headless)"}`;
  },
});

async function main() {
  log(`starting dev server; browser: ${channel}`);
  const server = await startServer();
  const browser = await chromium.launch({ channel, headless: true });
  let failed = 0;
  try {
    for (const [name, run] of Object.entries(scenarios)) {
      if (only && !name.includes(only)) continue;
      const context = await browser.newContext();
      const reset = await openTab(context);
      await reset.evaluate(() => lab.reset());
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
  log(failed === 0 ? "all scenarios passed" : `${failed} scenario(s) failed`);
  process.exit(failed === 0 ? 0 : 1);
}

await main();
