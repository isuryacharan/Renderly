# Bundled FFmpeg

Platform-specific `ffmpeg` / `ffprobe` binaries are fetched into this directory by
`renderly-app/scripts/fetch-ffmpeg.mjs` (via `npm run fetch-ffmpeg` or Tauri's
`beforeBuildCommand`). They are gitignored and must not be committed.

This README exists so Tauri's `bundle.resources` glob (`ffmpeg/*`) always matches at
least one file during `cargo build` / clippy when the binaries have not been fetched
yet — otherwise the build script fails with "glob pattern ffmpeg/* path not found".
