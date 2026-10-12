# ML dependencies — licence review

Status: **review, 2026-10-11; owner decisions recorded 2026-10-11.** Renderly is
AGPL-3.0-or-later and ships installers, so everything we *distribute* must carry no terms
beyond the AGPL's. This review gates the ML work on the parity board (denoise, background
removal, subject detection) and replaces the "pending licence review" note in the recorded
inference-runtime decision.

Rule applied throughout: where a licence is ambiguous, unstated, or could not be checked
against a primary source, the verdict is **uncertain — treat as not distributable**.
Code licence and weights licence are judged separately; they often differ.

## Recommended stack

| Role | Choice | Why |
|---|---|---|
| Inference runtime | [`tract`](https://github.com/sonos/tract) (MIT OR Apache-2.0), pure Rust | No native library, no build-time download, nothing extra to package. Operator coverage for the matting models must be proven by a spike first |
| Runtime fallback | [`ort`](https://github.com/pykeio/ort) (MIT OR Apache-2.0) + ONNX Runtime (MIT) | Only for models `tract` cannot run. Still `2.0.0-rc`; default build downloads binaries from a third-party CDN |
| Audio denoise | [`nnnoiseless`](https://github.com/jneem/nnnoiseless) (BSD-3-Clause) | Pure-Rust RNNoise port with the network built in; no runtime needed |
| Background removal | RVM-MobileNetV3 (video) and BiRefNet-lite (stills), **not bundled** | See "Matting weights" |
| Subject detection | [YuNet](https://github.com/opencv/opencv_zoo/tree/main/models/face_detection_yunet) (MIT, covers the model files) | The only detector whose weights sit under an explicit permissive statement |

## Verdicts

### Runtimes

| Candidate | Licence | Distributable | Use |
|---|---|---|---|
| `tract` | MIT OR Apache-2.0 | Yes | Primary |
| `ort` + ONNX Runtime | MIT OR Apache-2.0; runtime MIT (third-party notices not read) | Yes | Fallback |
| `candle` | Apache-2.0 / MIT | Yes | No — ONNX coverage unverified |
| `burn` | MIT / Apache-2.0 | Yes | No — importer supports a limited operator set |
| `wonnx` | Apache-2.0 / MIT | Yes | No — archived 2025-05 |

### Denoise

| Candidate | Code | Weights | Distributable | Use |
|---|---|---|---|---|
| `nnnoiseless` | BSD-3-Clause | Not stated separately; RNNoise-derived (BSD-3) | Yes, on that reading | **Yes** |
| DeepFilterNet | MIT / Apache-2.0 | None stated | Uncertain | No |
| FFmpeg `arnndn` + `.rnnn` file | FFmpeg | Model repos carry no usable licence | User-supplied file only | Fallback |

### Matting / background removal

| Model | Code | Weights | Distributable | Use |
|---|---|---|---|---|
| Robust Video Matting | GPL-3.0 | None stated; training data includes research-only matting sets | Uncertain | Downloaded or user-supplied only |
| BiRefNet | MIT | Hugging Face tag `mit`; training-data terms unverified | Uncertain | Downloaded or user-supplied only |
| MODNet | Apache-2.0 ("code, models") | Checkpoints on third-party hosts | Uncertain | Optional |
| U²-Net / U²-Netp | Apache-2.0 | None stated | Uncertain | Optional |
| IS-Net / DIS | Apache-2.0 | "for academic comparisons"; dataset terms unread | Uncertain | No |
| RMBG-1.4 / RMBG-2.0 | — | **Non-commercial (CC BY-NC)** | **No** | **Never ship or auto-download** |
| MediaPipe Selfie Segmentation | Apache-2.0 | Model card not readable | Uncertain | No |
| PP-HumanSeg / PP-Matting | Apache-2.0 | None stated | Uncertain | No |
| BEN2 | not checked | `mit` tag beside a commercial-only full model | Uncertain | No |

### Subject detection

| Model | Code | Weights | Distributable | Use |
|---|---|---|---|---|
| YuNet | MIT | MIT (directory statement) | Yes | **Yes** |
| YOLOX | Apache-2.0 | None stated | Uncertain | Later, after a weights check |
| Ultralytics YOLO | AGPL-3.0 | Implied, not stated | Compatible, but pins the combined work to AGPL-3.0-only | No |
| RT-DETR, NanoDet | Apache-2.0 | None stated | Uncertain | No |
| SCRFD (InsightFace) | MIT | **Non-commercial** | **No** | **No** |
| BlazeFace | Apache-2.0 | Model card not read | Uncertain | No |

## Matting weights

No matting model has weights with a verified, unambiguous permissive licence. Three ways to
handle weights, in decreasing order of exposure:

1. **Bundle in the installer** — we convey the file, so its terms must add nothing to the
   AGPL. Not available for any matting model today.
2. **Download from the upstream host on first use, with consent** — we convey nothing;
   Renderly fetches from the owner's own URL, pinned by tag and SHA-256.
3. **User-supplied path** (`RENDERLY_MATTE_MODEL`, like `RENDERLY_WHISPER_MODEL`) — the
   user obtains the file; Renderly is a generic loader.

Non-commercial weights (RMBG, SCRFD) are acceptable only under 3, are never a default, and
the docs must say that their output cannot be used commercially.

RVM specifically: loading its ONNX file is using data, not combining code. Copying RVM's
GPL-3.0 code into core would make that part GPL-3.0-only and cost the project its
"or later". Do not copy it; write the inference glue ourselves.

## Mechanics

- **Notices.** A third-party licence file listing every dependency with SPDX id and text
  (MIT, BSD-3 and Apache-2.0 all require the notice in binary distributions).
- **Integrity.** A manifest with a pinned upstream URL and SHA-256 per model; verify before
  loading; fail closed; never auto-update.
- **No native plugins (AGENTS.md §0.6).** A runtime we pin and ship is a dependency, not a
  plugin. `ort`'s `load-dynamic` mode reading `ORT_DYLIB_PATH` would let a user load an
  arbitrary native library — if the fallback is ever used, the path must be fixed to our
  bundled copy and the env var ignored.
- **wasm32.** All ML code is native-only.

## Decisions

Made by the project owner on 2026-10-11:

1. **Weights with no stated licence** (RVM, and BiRefNet's training data): downloaded from
   the upstream host on first use, with user consent and a pinned SHA-256. Never bundled.
2. **`nnnoiseless` built-in network**: the implied BSD-3 licence is accepted.
3. **YuNet** is bundled in the installer under its MIT statement.
4. **`ort` fallback**: allowed only for a model `tract` cannot run, and only with the native
   library path fixed to our own bundled copy (`ORT_DYLIB_PATH` ignored).

## What shipped (2026-10)

- **Robust Video Matting** (MobileNetV3, fp32, GPL-3.0 weights) runs in-process under `tract`
  (`renderly-core/src/ml/rvm.rs`, pre/post-processing written from the model's documented I/O
  contract; no upstream code copied) and powers background-removal `model_id: "rvm"` as a per-frame
  matte. The weights are **never bundled or committed**: the app shows a consent dialog (model,
  size, upstream host, licence link, local cache path) and downloads from the project's own
  GitHub release, pinned by tag and SHA-256, through `ml::download_model` (atomic, fail-closed).
  `renderly models download <id> --yes` and the MCP `download_model` tool (`user_consented: true`)
  do the same for CLI and agents; nothing in a render or command path downloads implicitly - a
  missing model is the typed error `ModelNotAvailable`. `RENDERLY_MODEL_DIR` points at an
  existing copy.
- **BiRefNet-lite was dropped**: ~100 s/frame and ~14 GB under `tract` made it impractical, so
  its manifest entry was removed instead of shipping a dead option. (`model_id: "birefnet"` on a
  clip still means "external binary via `RENDERLY_SEG_CLI`".) The "stills" role in the stack table
  above is therefore unfilled.
- Mattes are a derived cache, not part of the conveyed work: sequences live in the temp dir and
  record the weights' SHA-256 in `matte.json`.

## Not verified

ONNX Runtime's third-party notices; `ort` prebuilt availability per platform; `tract`
operator coverage for each model; MediaPipe model cards; DIS5K terms; every training-data
licence; most model file sizes. GPL/AGPL §13 was applied from knowledge of the text, not
re-read.
