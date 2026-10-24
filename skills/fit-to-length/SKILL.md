---
name: fit-to-length
description: Condense the open Renderly timeline to a target length (for example 60 seconds) by removing the least important pauses and, if needed, speeding up clips, with dry runs and approved deletions. Use when the user gives a length limit for a video, such as a 60-second short or a 30-second ad.
---

# Fit to length

Use the `renderly` MCP tools: `get_project`, `detect_silence`, `detect_scenes`, `get_transcript`,
`apply_command` (one command per call), `render_frame`.

## When NOT to use this

- The user wants a specific set of lines kept or dropped: use `rough-cut-from-transcript`.
- The user wants every pause removed regardless of length: use `remove-silences`.
- The target is far below what speech alone needs (for example 10 seconds for a 3-minute talk).
  Say so and ask what to keep.
- Speed-ups change how the speech sounds. Do not speed up clips without the user's agreement.

## Procedure

1. **Measure.** `get_project`: the timeline's length is the largest `start_us + duration` of
   the clips on the root tracks (clip duration = `source_out_us - source_in_us` divided by
   `speed`; all times are integer microseconds). Note the target `N` and the gap `G = length - N`. If `G <= 0`, say so and stop.
2. **Gather candidates.** For each media clip, call `detect_silence` (`min_duration` `"0.8s"`, so
   only real pauses are candidates). Optionally call `detect_scenes` on the same media: a cut at a
   scene change is a good place to join. Use `get_transcript` when content matters (cut silences
   between sentences, not inside them).
3. **Rank.** Sort candidate pauses by length, longest first. Drop any pause shorter than 0.6 s after
   the margin in step 4. Keep a 0.12 s margin on each side of each pause.
4. **Remove pauses until the gap is closed.** Accumulate the removed time until it reaches `G`. Use
   the `remove-silences` procedure for the chosen pauses: latest first, `SplitClip` at the pause's
   end and start, then `DeleteClip` with `ripple: true`, each deletion with `allow_deletion` listing
   exactly the piece's id. Every call takes `expected_revision` from the call before it. Dry-run
   the first call of each kind.
5. **Speed only if still long, and only with agreement.** If pauses cannot close the gap, ask the
   user before changing speed. If they agree, `SetClipSpeed` (0.25 to 4.0) on the relevant video
   clips. Speed changes the timeline span of each clip, so re-read `get_project` and recheck the
   total length after each change, and keep linked audio in step as in `remove-silences`.
6. **Verify.** `get_project`: the total length must be within 0.5 s of `N`, or say how far it is.
   Spot-check one join with `render_frame`: decode the `png_base64` from the JSON to a `.png` file in
   your scratch folder and open it with `Read`.
7. **Report.** The new length, what was removed (for example "removed 9 pauses, about 14 seconds"),
   and whether speed was changed. Do not list ids.

## Notes

- Length is a plan, not a guarantee: pauses removed mid-sentence can sound wrong. Offer to review
  the result with the user.
- Each call is its own undo step. The user can revert in order.
- On `revision_conflict`, the user changed the timeline. Re-read and recompute the gap from the
  current length.
