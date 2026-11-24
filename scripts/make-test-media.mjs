// Generates synthetic test media into test-media/ (git-ignored) with ffmpeg.
// Every frame carries a burned-in timecode and the audio beeps once per second, so sync and
// frame accuracy can be checked by eye and by script.
//
//   node scripts/make-test-media.mjs [name...]   (no names: all files)
//
// Needs ffmpeg on PATH. Uses NVENC when available (fast), otherwise libx264.

import { execFileSync, spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, rmSync } from "node:fs";
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
  // 4K60 HEVC and a second H.264 file for the cut stress list (S0.5).
  "f_4k60_hevc_60mbps_3min.mp4": {
    size: "3840:2160",
    rate: 60,
    seconds: 180,
    bitrate: "60M",
    gop: 120,
    codec: "hevc",
  },
  "g_4k60_h264_50mbps_3min.mp4": {
    size: "3840:2160",
    rate: 60,
    seconds: 180,
    bitrate: "50M",
    gop: 120,
  },
  // Long enough for the 10-minute audio/video sync run.
  "e_1080p60_h264_12min.mp4": {
    size: "1920:1080",
    rate: 60,
    seconds: 720,
    bitrate: "8M",
    gop: 120,
  },
  // Phone-style portrait clips: `size` is the upright picture. The coded frames are landscape and
  // the file carries a clockwise display rotation, as phones record them. "TOP" marks the top.
  "c_portrait_rot90_h264_20s.mp4": {
    size: "1080:1920",
    rate: 30,
    seconds: 20,
    bitrate: "10M",
    gop: 60,
    rotation: 90,
  },
  "d_portrait_rot270_h264_20s.mp4": {
    size: "1080:1920",
    rate: 30,
    seconds: 20,
    bitrate: "10M",
    gop: 60,
    rotation: 270,
  },
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

function generate(
  name,
  { size, rate, seconds, bitrate, gop, rotation = 0, codec = "h264" },
  nvenc,
) {
  const label = name.split("_")[0].toUpperCase();
  const text = `drawtext=fontfile=timecode-font.ttf:text='${label}  %{pts\\:hms}  f%{n}':fontsize=40:fontcolor=white:box=1:boxcolor=black@0.6:x=20:y=20`;
  const top =
    "drawtext=fontfile=timecode-font.ttf:text='TOP':fontsize=96:fontcolor=white:box=1:boxcolor=red@0.8:x=(w-tw)/2:y=80";
  // Store the upright picture turned anticlockwise, so a clockwise display rotation restores it.
  const store = rotation === 90 ? ",transpose=2" : rotation === 270 ? ",transpose=1" : "";
  const coded = rotation ? `coded-${name}` : name;
  const hevc = codec === "hevc";
  const video = nvenc
    ? [
        "-c:v",
        hevc ? "hevc_nvenc" : "h264_nvenc",
        // hvc1 is the HEVC sample entry browsers accept in MP4.
        ...(hevc ? ["-tag:v", "hvc1"] : []),
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
    : hevc
      ? ["-c:v", "libx265", "-tag:v", "hvc1", "-preset", "veryfast", "-b:v", bitrate]
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
      `testsrc2=size=${rotation ? "540x960" : "960x540"}:rate=${rate}:duration=${seconds}`,
      "-f",
      "lavfi",
      "-i",
      `sine=frequency=440:beep_factor=4:sample_rate=48000:duration=${seconds}`,
      "-vf",
      `${text}${rotation ? `,${top}` : ""},scale=${size}:flags=bicubic${store},format=yuv420p`,
      ...video,
      "-g",
      String(gop),
      "-bf",
      "2",
      "-c:a",
      "aac",
      "-b:a",
      "192k",
      join(outDir, coded),
    ],
    { stdio: "inherit", cwd: outDir },
  );
  if (!rotation) return;
  // Tag the display rotation without re-encoding. ffmpeg counts this angle anticlockwise.
  execFileSync(
    "ffmpeg",
    [
      "-hide_banner",
      "-loglevel",
      "error",
      "-y",
      "-display_rotation:v:0",
      String(-rotation),
      "-i",
      coded,
      "-c",
      "copy",
      name,
    ],
    { stdio: "inherit", cwd: outDir },
  );
  rmSync(join(outDir, coded));
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
