# Linked FFmpeg (in-process libav)

Status: stage 2 (linked software frame decode) is implemented behind the feature and is compared
against the CLI in dual-backend tests. Off by default. Default builds still use the FFmpeg CLI for
everything, and the linked decode is selected only with the feature and `RENDERLY_MEDIA_BACKEND=linked`.

## What exists now

- **Backend seam** (`renderly-core/src/media/backend.rs`): `MediaBackend`, `selected_backend()`,
  and the public `probe_video`, `has_audio_stream` and `VideoReader`. Call sites are unchanged.
  The CLI implementation lives in `media/ffmpeg_cli.rs` (`CliReader` is the private reader).
- **Linked decode** (`renderly-core/src/media/linked.rs`, `LinkedReader`): software decode
  through `ffmpeg-the-third`, with the `CliReader` contract. `VideoReader::open_with` tries it
  when the linked backend is selected and falls back to the CLI reader on any error.
  - Seek: input seek to the keyframe at or before `start_secs`, then frames before
    `start_secs - half a frame` are dropped.
  - Pixels: one `swscale` pass to RGBA with `BICUBIC | ACCURATE_RND | FULL_CHR_H_INT`. The input
    matrix is BT.709 limited range and the output is full range, set with
    `sws_setColorspaceDetails`. This is the single-pass form of the CLI's
    `scale=...,format=rgba` chain. The one `unsafe` call is documented in the code.
  - Frame rate: when `output_fps` is unset, the stream's `r_frame_rate` is used. The CLI's
    rawvideo output is constant-rate, so variable-rate sources are resampled to it. The CLI
    does the same, so this is a deliberate match to the CLI rather than "frames as decoded".
  - Tail: the last frame is held for its own gap, rounded to output slots.
  - Only the `codec`, `format` and `software-scaling` features of `ffmpeg-the-third` are used.
    No extra features were enabled.
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

### Dual-backend decode tests

`media::linked::decode_tests::linked_decode_matches_cli_decode` decodes the same fixtures through
`CliReader` and `LinkedReader` and compares every frame. The fixtures are generated with the
`ffmpeg` CLI: 322x240 FFV1 (lossless, 30 fps), 322x240 H.264 with `-bf 2 -g 30`, and a VFR clip
(1 s at 30 fps then 1 s at 15 fps, joined with stream copy). Each fixture is decoded from 0 s and
from a mid-GOP start, with no options, `target_height=120`, `output_fps` 15, 60 and 24, and
`target_height=120` with `output_fps=24`. The test asserts the same frame count, the same
dimensions, and a per-channel difference of at most 1 for FFV1 and 2 for H.264.

Run it locally on Linux:

```sh
cargo test -p renderly-core --features linked-ffmpeg --lib media::linked::decode_tests -- --nocapture
```

The printed `dual-backend ...` lines give the observed maximum difference for each case.

### Measured results (CI run 38692141666, Ubuntu 24.04, FFmpeg 6.1)

Exact match (0 difference, same frame counts) for all of these:

- native rate, with and without `target_height=120`, FFV1 and H.264 from 0 s and mid-GOP;
- VFR, native rate (the CLI resamples both to 30 fps, and so does the linked reader), with and without scaling;
- `output_fps=60` on FFV1, H.264 and VFR.

Not matching yet, with `output_fps` at 15 and 24 (from 30 fps sources):

| Case | CLI frames | Linked frames | Max abs diff |
|---|---|---|---|
| FFV1 0 s, fps 15 | 47 | 46 | 255 |
| FFV1 1.5 s, fps 15 | 24 | 23 | 255 |
| FFV1 0 s, 120 px, fps 24 | 74 | 72 | 255 |
| FFV1 1.5 s, 120 px, fps 24 | 38 | 36 | 255 |
| H.264 0 s, fps 15 | 47 | 46 | 255 |
| H.264 1.5 s, fps 15 | 24 | 23 | 255 |
| H.264 0 s, 120 px, fps 24 | 74 | 72 | 255 |
| H.264 1.5 s, 120 px, fps 24 | 38 | 36 | 255 |
| VFR 0 s, fps 15 | 32 | 30 | 255 |
| VFR 0.5 s, fps 15 | 24 | 22 | 255 |
| VFR 0 s, 120 px, fps 24 | 48 | 48 | 255 (same count, different frames) |
| VFR 0.5 s, 120 px, fps 24 | 36 | 36 | 255 (same count, different frames) |

Known difference: the CLI's `fps` filter (libavfilter `vf_fps.c`) picks source frames for these
conversions with a two-slot buffer, and its frame placement is not the simple rule the linked
reader uses (round the frame time to the slot grid; each slot shows the latest frame placed at or
before it). The two agree when the output rate is an integer multiple of the source rate, which is
why 60 fps and native rate match. For FFV1 at 15 fps, the CLI picks source frames 0, 1, 2, 3, 5, 7, 9, ...
(measured with a local FFmpeg 7.1.1 CLI, per-frame MD5 mapping), which the linked reader does not
reproduce.

Other known differences: none measured in the fixtures above. The frame-rate conversion for
`output_fps` unset is a deliberate match to the CLI, and it differs from the earlier plan of
emitting frames as decoded.

Work to close the gap: port the `vf_fps` buffer and rounding logic exactly (the buffer logic is in
`libavfilter/vf_fps.c` `write_frame`/`activate`), then re-run the test.

## Remaining stages, in order

1. **Linked software frame decode.** Done for decode, with the 15/24 fps conversion gap above.
   The golden-frame suite is not wired to the linked path yet.
2. **Thumbnails** through the linked backend.
3. **Windows and macOS** dev-library setup in CI.
4. **Packaging** the shared libraries with the installers, with the licence rules in NOTICE-ffmpeg.md.
5. **Hardware decode** (`-hwaccel` equivalents) on the linked path.
6. **Flip the default** to linked, once the earlier stages are proven, and make the CI job a gate.

Encode, mux and the audio filter graph are intended to stay on the CLI. The linked backend is
not planned to replace them.
