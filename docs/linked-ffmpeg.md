# Linked FFmpeg (in-process libav)

Status: foundation only. Off by default. The media I/O path is still the FFmpeg CLI.

## What exists now

- **Backend seam** (`renderly-core/src/media/backend.rs`): `MediaBackend`, `selected_backend()`,
  and the public `probe_video`, `has_audio_stream` and `VideoReader`. Call sites are unchanged.
  The CLI implementation lives in `media/ffmpeg_cli.rs` (`CliReader` is the private reader).
- **Feature flag** `linked-ffmpeg` in `renderly-core/Cargo.toml`, off by default. It pulls in the
  optional `ffmpeg-the-third = "=6.0.0"` crate with `codec`, `format` and `software-scaling`.
- **Linked probe** (`renderly-core/src/media/linked.rs`): width, height, fps and duration, plus
  the audio-stream check. Compiled only with the feature. It is not compiled or tested in the
  development environment yet. Any error falls back to the CLI probe.
- **Informational CI job** `linked-ffmpeg` in `.github/workflows/ci.yml`: Ubuntu 24.04,
  `continue-on-error: true`. It runs `cargo check` and `cargo test ... --lib media::` with the
  feature on. Its failures do not block merges.

Selecting the linked probe needs both the feature **and** `RENDERLY_MEDIA_BACKEND=linked`.
Without the env var, the CLI is used even when the feature is compiled in.

## Try it locally (Linux)

```sh
sudo apt-get install -y libavcodec-dev libavformat-dev libavutil-dev libswscale-dev \
  libswresample-dev libavfilter-dev libavdevice-dev libclang-dev pkg-config clang ffmpeg
cargo test -p renderly-core --features linked-ffmpeg --lib media::
RENDERLY_MEDIA_BACKEND=linked cargo run -p renderly-cli --features renderly-core/linked-ffmpeg -- <args>
```

The test generates clips with the `ffmpeg` CLI and checks that the linked and CLI probes agree
on width, height, fps (within 1e-3) and duration (within 0.05 s). It skips when the CLI is absent.

Windows and macOS are not wired up yet. The feature needs FFmpeg dev libraries and libclang at
build time, which are not set up for those platforms.

## Remaining stages, in order

1. **Linked software frame decode.** Output must match the CLI's scale and colour conversion
   (bicubic, `accurate_rnd`, `full_chroma_int`, BT.709 limited to full RGB) within 1 LSB. Validate
   with the golden-frame suite and a dual-backend test that decodes the same frames both ways.
2. **Thumbnails** through the linked backend.
3. **Windows and macOS** dev-library setup in CI.
4. **Packaging** the shared libraries with the installers, with the licence rules in NOTICE-ffmpeg.md.
5. **Hardware decode** (`-hwaccel` equivalents) on the linked path.
6. **Flip the default** to linked, once the earlier stages are proven, and make the CI job a gate.

Encode, mux and the audio filter graph are intended to stay on the CLI. The linked backend is
not planned to replace them.
