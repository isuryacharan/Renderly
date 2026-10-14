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

## Optional in-process FFmpeg build (`linked-ffmpeg`)

The `renderly-core` cargo feature `linked-ffmpeg` is **optional and off by default**. When it is
enabled, FFmpeg's libraries (libavcodec, libavformat, libavutil, libswscale) are linked into the
build in-process through the `ffmpeg-the-third` crate, instead of being run as the `ffmpeg` and
`ffprobe` command-line tools. No release installer enables this feature today.

Any build that enables `linked-ffmpeg` must follow these rules:

- **Use an FFmpeg configured with `--enable-version3` when GPL components are enabled.** A GPL
  build without `--enable-version3` is licensed GPL-2.0-or-later, which is incompatible with
  AGPL-3.0 distribution.
- **Never use `--enable-nonfree`.** Nonfree components cannot be distributed with Renderly.
- Linked libraries must ship with their license texts and corresponding source, the same as the
  bundled CLI binaries above.

See [docs/linked-ffmpeg.md](docs/linked-ffmpeg.md) for the current scope.
