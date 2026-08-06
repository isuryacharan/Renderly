# FFmpeg — bundled binary notice

Renderly's packaged installers bundle **FFmpeg** (`ffmpeg` and `ffprobe`) so the app works
without a separate install. FFmpeg is a separate work, not part of Renderly, and is licensed
under its own terms.

- **Build used:** GPL builds from [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds)
  (Windows, Linux) and [evermeet.cx](https://evermeet.cx/ffmpeg/) (macOS). The GPL build is
  required because the LGPL build omits the x264/x265 encoders that H.264/HEVC export needs.
- **License:** FFmpeg configured as GPL is licensed under the
  [GNU GPL v3](https://www.gnu.org/licenses/gpl-3.0.html). This is compatible with Renderly's
  AGPL-3.0 license.
- **Source:** FFmpeg's complete corresponding source is available from
  [ffmpeg.org](https://ffmpeg.org/download.html) and the build repositories linked above.
- **How it's bundled:** the binaries are **not** stored in this repository. They are fetched
  at build time by [`renderly-app/scripts/fetch-ffmpeg.mjs`](renderly-app/scripts/fetch-ffmpeg.mjs)
  for the platform being built, and copied into the installer as an app resource.

If you build Renderly from source without running a packaged `tauri build`, no FFmpeg binary
is bundled and the app uses whatever `ffmpeg`/`ffprobe` it finds on your `PATH`.
