---
name: rough-cut-from-transcript
description: Build a rough cut of a clip in an open Renderly project by keeping the transcript segments the user asks for and removing the rest, with dry runs and approved deletions. Use when the user wants an edit driven by what is said (keep the intro and the three best answers, drop the rambling).
---

# Rough cut from transcript

Use the `renderly` MCP tools: `get_project`, `get_transcript`, `apply_command` (one command per
call), `render_frame`.

## When NOT to use this

- The user wants pauses removed without regard to content: use `remove-silences`.
- The user wants a result of a set length: use `fit-to-length`.
- There is no transcript yet and no speech to transcribe (music, B-roll only).
- If the project has no stored word-level transcript (`get_transcript` says `source: "whisper"`
  with no `words`), cut points fall on segment boundaries (plus a small pad). Do not promise a cut
  between two words of one sentence.
- Whisper may mishear. Read the segment text back to the user before cutting anything they might
  want.

## Procedure

1. **Read the project.** `get_project`: note the `media_id`, the clip that uses it (`clip_id`,
   `track_id`, `start_us`, `source_in_us`, `source_out_us`; integer microseconds), and `project_revision`.
2. **Transcribe.** `get_transcript` with the `media_id`. With a stored transcript it returns
   `{source: "project", transcript: {id, words: [{id, text, start_us, end_us, deleted, tags}]}}`
   in source time (integer microseconds); otherwise Whisper segments `{start_us, end_us, text}`.
   (`GenerateCaptions` on the media stores the word-level transcript.)

   **With a stored transcript, cut by words**: choose the words to remove, then one
   `ApplyTranscriptCuts { transcript_id, word_ids: [...] }` (or `tags: ["filler"]` for filler words)
   removes them from every clip of that media and closes the gaps, on every track the media is
   on. Dry-run it first and send `allow_deletion` with the exact ids the dry run lists.
   `RestoreTranscriptWords` undoes it for chosen words. Skip steps 4 to 6 below.
3. **Choose what to keep.** Pick the segments that match the request, in order. Keep each kept
   range's start and end by at least 0.1 s of pad where the neighbouring segment is a pause, so
   words are not clipped. Write the plan as a list of kept ranges in source time and show it to
   the user if the choice is judgement-heavy. Ask one question if it is ambiguous.
4. **Convert the plan into cuts.** Everything not kept is a cut. Map each cut to timeline time
   with `timeline = clip.start_us + (source - source_in_us)` (speed 1.0 only). Work with
   timeline times, latest first.
5. **Cut and delete.** For each cut, latest first, follow the `remove-silences` procedure: one
   `SplitClip` at the cut's end and one at its start (skip a split that falls on a clip's edge),
   then `DeleteClip` with `ripple: true` on the middle piece. Every call takes `expected_revision`
   set to the revision from the call before it. Dry-run the first call of each kind. Each
   deletion carries `allow_deletion` with a reason such as "rough cut the user asked for" and the
   exact ids of the piece being deleted.
6. **Keep audio in step.** Ripple moves only its own track. Repeat the cuts on the linked audio
   track, or pick the same cuts there.
7. **Verify.** `get_project`: check the remaining segments' order and total length. Spot-check a
   join with `render_frame`: decode the `png_base64` from the JSON to a `.png` file in your scratch
   folder and open it with `Read`.
8. **Report.** Summarise the result in one or two sentences: what was kept and the new length. Do
   not list ids or the segment list.

## Notes

- Each call is its own undo step. Say so when the cut is large.
- `revision_conflict` means the user changed the timeline. Re-read, recompute the remaining cuts
  from the kept ranges, and continue.
