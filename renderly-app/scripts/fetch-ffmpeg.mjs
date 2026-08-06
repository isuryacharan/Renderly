// Fetch ffmpeg + ffprobe for the current platform into src-tauri/ffmpeg/, so the packaged
// app bundles them and users don't have to install ffmpeg themselves. Run automatically by
// `tauri build` (beforeBuildCommand) and available as `npm run fetch-ffmpeg`.
//
// Idempotent: skips the download if both binaries are already present. The binaries are
// gitignored and fetched fresh per build environment (Windows/macOS/Linux CI runners each
// grab their own). GPL builds are used because the LGPL builds omit the x264/x265 encoders
// export needs; GPL ffmpeg is compatible with Renderly's AGPL-3.0 (see NOTICE-ffmpeg.md).
//
// Sources: BtbN/FFmpeg-Builds (Windows, Linux) and evermeet.cx (macOS).

import { createWriteStream } from "node:fs";
import { mkdir, rm, access, readdir, copyFile, chmod } from "node:fs/promises";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";

const run = promisify(execFile);
const here = dirname(fileURLToPath(import.meta.url));
const outDir = join(here, "..", "src-tauri", "ffmpeg");
const EXE = process.platform === "win32" ? ".exe" : "";

// One archive that contains both binaries (Windows/Linux), or two single-binary archives
// (macOS). `strip` drops the leading archive directory when globbing for the binaries.
const PLATFORMS = {
  win32: {
    archives: [
      {
        url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip",
        kind: "zip",
      },
    ],
  },
  linux: {
    archives: [
      {
        url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linux64-gpl.tar.xz",
        kind: "tar.xz",
      },
    ],
  },
  darwin: {
    archives: [
      { url: "https://evermeet.cx/ffmpeg/getrelease/ffmpeg/zip", kind: "zip" },
      { url: "https://evermeet.cx/ffmpeg/getrelease/ffprobe/zip", kind: "zip" },
    ],
  },
};

async function exists(p) {
  try {
    await access(p);
    return true;
  } catch {
    return false;
  }
}

async function download(url, dest) {
  process.stdout.write(`  downloading ${url}\n`);
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText} for ${url}`);
  await pipeline(Readable.fromWeb(res.body), createWriteStream(dest));
}

async function extract(archive, kind, into) {
  if (kind === "zip") {
    if (process.platform === "win32") {
      await run("powershell", [
        "-NoProfile",
        "-Command",
        `Expand-Archive -LiteralPath '${archive}' -DestinationPath '${into}' -Force`,
      ]);
    } else {
      await run("unzip", ["-o", "-q", archive, "-d", into]);
    }
  } else if (kind === "tar.xz") {
    await run("tar", ["-xf", archive, "-C", into]);
  } else {
    throw new Error(`unknown archive kind ${kind}`);
  }
}

// Recursively find ffmpeg/ffprobe binaries anywhere under `dir` (archive layouts nest them
// in a bin/ subdir or the build's top folder).
async function findBinaries(dir) {
  const found = {};
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      Object.assign(found, await findBinaries(full));
    } else if (entry.name === `ffmpeg${EXE}`) {
      found.ffmpeg = full;
    } else if (entry.name === `ffprobe${EXE}`) {
      found.ffprobe = full;
    }
  }
  return found;
}

async function main() {
  const plat = PLATFORMS[process.platform];
  if (!plat) {
    console.error(`fetch-ffmpeg: unsupported platform ${process.platform}`);
    process.exit(1);
  }

  const ffmpeg = join(outDir, `ffmpeg${EXE}`);
  const ffprobe = join(outDir, `ffprobe${EXE}`);
  if ((await exists(ffmpeg)) && (await exists(ffprobe))) {
    console.log("fetch-ffmpeg: binaries already present, skipping.");
    return;
  }

  await mkdir(outDir, { recursive: true });
  const work = join(tmpdir(), `renderly-ffmpeg-${Date.now()}`);
  await mkdir(work, { recursive: true });

  try {
    const collected = {};
    for (let i = 0; i < plat.archives.length; i++) {
      const { url, kind } = plat.archives[i];
      const archivePath = join(work, `a${i}.${kind.replace(".", "_")}`);
      await download(url, archivePath);
      const dest = join(work, `x${i}`);
      await mkdir(dest, { recursive: true });
      await extract(archivePath, kind, dest);
      Object.assign(collected, await findBinaries(dest));
    }

    if (!collected.ffmpeg || !collected.ffprobe) {
      throw new Error("ffmpeg/ffprobe not found in the downloaded archive(s)");
    }
    await copyFile(collected.ffmpeg, ffmpeg);
    await copyFile(collected.ffprobe, ffprobe);
    if (process.platform !== "win32") {
      await chmod(ffmpeg, 0o755);
      await chmod(ffprobe, 0o755);
    }
    console.log(`fetch-ffmpeg: installed ffmpeg + ffprobe into ${outDir}`);
  } finally {
    await rm(work, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error(`fetch-ffmpeg: ${e.message}`);
  process.exit(1);
});
