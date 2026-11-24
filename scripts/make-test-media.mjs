// Generates synthetic test media into test-media/ (git-ignored) with ffmpeg.
// Every frame carries a burned-in timecode and the audio beeps once per second, so sync and
// frame accuracy can be checked by eye and by script.
//
//   node scripts/make-test-media.mjs [name...]   (no names: all files)
//
// Needs ffmpeg on PATH. Uses NVENC when available (fast), otherwise libx264.

import { execFileSync, spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import { join } from "node:path";

const outDir = join(import.meta.dirname, "..", "test-media");

const media = {
  // > 6 GB, camera-grade bitrate, 2 s GOP with B-frames: open time, heap and exact-seek tests.
  "a_4k60_h264_120mbps_7min.mp4": {
    size: "3840:2160",
    rate: 60,
    seconds: 420,
    bitrate: "120M",
    gop: 120,
  },
  // Short 1080p clip for quick play and leak tests.
  "b_1080p60_h264_1min.mp4": { size: "1920:1080", rate: 60, seconds: 60, bitrate: "12M", gop: 120 },
};

const hasNvenc = () =>
  spawnSync("ffmpeg", ["-hide_banner", "-encoders"], { encoding: "utf8" }).stdout.includes(
    "h264_nvenc",
  );

// drawtext cannot take a Windows drive path without fragile escaping, so the font is copied
// next to the output and referenced relative to it.
const systemFont =
  process.platform === "win32"
    ? "C:/Windows/Fonts/arialbd.ttf"
    : "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf";

function generate(name, { size, rate, seconds, bitrate, gop }, nvenc) {
  const label = name.split("_")[0].toUpperCase();
  const text = `drawtext=fontfile=timecode-font.ttf:text='${label}  %{pts\\:hms}  f%{n}':fontsize=40:fontcolor=white:box=1:boxcolor=black@0.6:x=20:y=20`;
  const video = nvenc
    ? [
        "-c:v",
        "h264_nvenc",
        "-preset",
        "p4",
        "-rc",
        "cbr",
        "-b:v",
        bitrate,
        "-maxrate",
        bitrate,
        "-bufsize",
        bitrate,
      ]
    : ["-c:v", "libx264", "-preset", "veryfast", "-b:v", bitrate];
  execFileSync(
    "ffmpeg",
    [
      "-hide_banner",
      "-loglevel",
      "error",
      "-stats",
      "-y",
      "-f",
      "lavfi",
      "-i",
      `testsrc2=size=960x540:rate=${rate}:duration=${seconds}`,
      "-f",
      "lavfi",
      "-i",
      `sine=frequency=440:beep_factor=4:sample_rate=48000:duration=${seconds}`,
      "-vf",
      `${text},scale=${size}:flags=bicubic,format=yuv420p`,
      ...video,
      "-g",
      String(gop),
      "-bf",
      "2",
      "-c:a",
      "aac",
      "-b:a",
      "192k",
      join(outDir, name),
    ],
    { stdio: "inherit", cwd: outDir },
  );
}

mkdirSync(outDir, { recursive: true });
copyFileSync(systemFont, join(outDir, "timecode-font.ttf"));
const wanted = process.argv.slice(2);
const nvenc = hasNvenc();
for (const [name, spec] of Object.entries(media)) {
  if (wanted.length > 0 && !wanted.includes(name)) continue;
  if (existsSync(join(outDir, name))) {
    console.log(`exists: ${name}`);
    continue;
  }
  console.log(`generating ${name} (${nvenc ? "NVENC" : "libx264"})`);
  generate(name, spec, nvenc);
}
