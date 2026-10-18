---
name: remove-silences
description: Cut silent pauses out of a clip in an open Renderly project, using detect_silence, dry runs, and deletions the user has asked for. Use when the user asks to remove silences, dead air, or long pauses from a talking-head or voiceover edit.
---

# Remove silences

Use the `renderly` MCP tools: `get_project`, `detect_silence`, `apply_command` (one command per
call), `render_frame`. Renderly's engine does the cuts; you plan them from the detected spans.

## When NOT to use this

- The user wants a shorter edit by content (keep this line, drop that one): use
  `rough-cut-from-transcript`.
- The user wants the whole video to fit a length: use `fit-to-length`.
- The clip is not at speed 1.0, or it is a nested sequence: stop and ask. The time mapping below
  assumes a media clip at speed 1.0.
- Silence detection measures level, not meaning. Do not cut a pause that is a deliberate beat or
  sits under music. If the result looks wrong, confirm the threshold with the user.

## Procedure

1. **Read the project.** `get_project` returns `project_revision` and the project. Note the track
   id, the clip ids, `position_secs`, `source_in_secs`, `source_out_secs`, `media_id` and `speed`
   for each clip you will cut.
2. **Detect silence.** `detect_silence` with the clip's `media_id`. Use the defaults, with
   `min_duration_secs` 0.6 so breaths are kept. Each span has `start_secs` and `end_secs`.
3. **Map to timeline time.** Keep spans that lie fully inside a clip's source range. Shrink each by
   0.12 s on both sides, then convert:
   `timeline_start = clip.position_secs + (span_start - source_in_secs)`, and the same for the end.
   Drop what is shorter than 0.3 s afterwards. Sort the cuts by `timeline_start`, latest first.
4. **Cut the pauses, latest first.** For each cut, in this order, one `apply_command` each:
   - `SplitClip` at the cut's end, on the clip that contains it.
   - `SplitClip` at the cut's start, on the same clip. The left piece keeps the clip's id, so the
     id of the clip containing the start is the one you split. Its `changed.created` list has
     exactly one new id: the middle piece. Record it, with the cut's `timeline_start`.
   Use `expected_revision` on every call, set to the `revision` returned by the call before it.
   Before the first cut, send one call with `dry_run: true` to check the summary.
   A split must fall strictly inside a clip. If a cut starts at a clip's first frame, skip the
   start split: the piece to delete is the clip that now starts at that time, which keeps the
   original id. If a cut ends at a clip's last frame, skip the end split. Find that piece with
   `get_project` (its `position_secs` equals the cut start). If a cut covers a whole clip, skip
   both splits and delete that clip.
5. **Delete the middle pieces, latest first.** For each recorded middle piece, one `DeleteClip`
   with `ripple: true` on its track, and `allow_deletion` with a `reason` such as "remove silences
   the user asked for" and `ids` set to that one clip id. Ripple shifts later clips on the same
   track only. Because you delete latest first, earlier positions stay valid. Dry-run the first
   deletion; if the error names different ids, use those.
6. **Keep audio in step.** A ripple moves only the track you delete from. If a clip has a linked
   audio clip on another track, repeat steps 4 and 5 on that track with the same timeline times,
   or the two drift apart.
7. **Verify.** `get_project` and check the new total length. To look at a join, call
   `render_frame` at a time just after it. It returns the frame as base64 PNG inside JSON: decode
   the `png_base64` value into a `.png` file in your scratch folder, then open that file with
   `Read`.
8. **Report.** Say how many pauses were removed and how much time was saved. Do not list ids.

## Notes

- Each call is its own undo step. Tell the user that they can revert the run one step at a time,
  or in full by undoing until the first cut.
- On `revision_conflict`, the user has edited the timeline. Call `get_project`, recompute the
  cuts that remain, and continue.
