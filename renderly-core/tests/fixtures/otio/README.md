# OpenTimelineIO sample files

These `.otio` files are **third-party test data**, copied unmodified from the
OpenTimelineIO project and used by `renderly-core/tests/otio_import.rs` to check that Renderly
imports files written by other tools.

- Source: <https://github.com/PixarAnimationStudios/OpenTimelineIO>, directory `tests/sample_data/`
- Commit: `00c22fa3a64256d8d3dcafbc7276492185dd970d` (main, fetched 2026-10-12)
- Licence: Apache License 2.0 — the licence text is in
  [`LICENSE-OpenTimelineIO.txt`](LICENSE-OpenTimelineIO.txt) and the upstream notice in
  [`NOTICE-OpenTimelineIO.txt`](NOTICE-OpenTimelineIO.txt), both copied from the same commit
  (`LICENSE.txt`, `NOTICE.txt`). Copyright Contributors to the OpenTimelineIO project.
- Renderly itself is AGPL-3.0-or-later; these data files stay Apache-2.0 and are used only as
  test inputs (nothing from OpenTimelineIO's code is linked or vendored).

| File | What it exercises |
|---|---|
| `simple_cut.otio` | `Clip.1` + `ExternalReference.1`, one track, a clip with no `source_range` (uses the available range, starting at frame 100) |
| `transition.otio` | `SMPTE_Dissolve` between two clips, `global_start_time` |
| `transition_test.otio` | legacy `Sequence.1` track, `MissingReference`-less clips (`media_reference: null`), transitions at the start/end of a track (no neighbour) |
| `nested_example.otio` | nested `Stack`s and a nested `Track`, `LinearTimeWarp`, `FreezeFrame`, `MissingReference` clips |
| `premiere_example.otio` | Premiere/FCP-XML-style export: `Clip.2` with `media_references`, mixed 15/30 fps ranges, `file://localhost/D%3a/...` URLs, markers, a generator, a nested stack, audio tracks, asymmetric transitions |
| `screening_example.otio` | `Clip.2` + `MissingReference`, markers, a track whose own `source_range` offsets its content by ~1 hour |

Do not edit these files; if a test needs a variant, build it in the test from the parsed JSON.
