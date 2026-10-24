---
name: captions-pass
description: Generate captions for a clip in an open Renderly project with local Whisper, restyle them, and check a frame. Use when the user asks for subtitles, auto-captions, or a caption style applied to a video.
---

# Captions pass

Use the `renderly` MCP tools: `get_project`, `apply_command` (one command per call),
`render_frame`. Captioning runs local Whisper, so it needs `whisper-cli` and a model
(`RENDERLY_WHISPER_MODEL`).

## When NOT to use this

- The user wants captions typed by hand or from a script file: use `AddCaption` per line instead,
  and say so.
- The captions must be animated word-by-word or karaoke-timed: the timings are stored
  (`body.words`) but the word-synced styles and animations render later. Say so, and offer the
  static result.
- The media has no speech.
- The user is only asking for a voiceover: that is `GenerateVoiceover`, not this skill.

## Procedure

1. **Read the project.** `get_project`: find the `media_id` of the clip to caption, and its
   position on the timeline (`start_us`, `source_in_us`, integer microseconds). Note `project_revision` for
   `expected_revision`.
2. **Caption track.** Find a track with `kind: "caption"` in the root timeline. If there is none,
   create one with `AddTrack` (`kind: "caption"`, `name: "Captions"`). Captions live on the root
   timeline only; a clip inside a sequence cannot be captioned directly.
3. **Pick a style.** Built-in style refs: `builtin:tiktok-bold-yellow@1` (default),
   `builtin:tiktok-minimal@1`, `builtin:tiktok-box@1`, `builtin:youtube-lower-thirds@1`. Use the
   one the user asked for, or `tiktok-bold-yellow` for short-form vertical video and
   `youtube-lower-thirds` for landscape. Set it once on the track with `SetTrackCaptionStyle
   { track_id, style: { "ref": "..." } }`; captions without their own style follow it.
4. **Generate.** `GenerateCaptions` with `media_id`, `track_id` (leave `style` out so the captions
   follow the track), and `timeline_offset` set to the clip's `start_us` minus its `source_in_us`, as an integer
   number of microseconds (so the
   captions line up with the timeline). Dry-run first: the dry run refuses file-writing commands,
   so run this one for real, with `expected_revision`. Its result's `changed.created` lists the new
   caption clips. Check `summary`.
5. **Restyle if needed.** To restyle every caption, send `SetTrackCaptionStyle` with
   `reset_clips: true` (one call). For some captions only, `SetTextStyle` with `targets` and a
   `ref` (one call, one undo step). To change the text of a caption, use `SetTextBody` with
   `text` (the word timings stay). To make pages shorter or longer, `SplitCaptions` with
   `max_chars`, `max_lines` and `max_duration`. Every change is one undo step.
6. **Check placement.** `render_frame` at a time when a caption is on screen. The result is a
   base64 PNG inside JSON: decode the `png_base64` value to a `.png` file in your scratch folder and
   open it with `Read`. Look for captions that are cut off at the edges or cover faces the user
   cares about. If so, say so; do not move captions without asking.
7. **Report.** How many captions were made, the style, and anything you could not check (for
   example, Whisper errors on a passage). Do not list ids.

## Notes

- If `GenerateCaptions` fails with a model or `whisper-cli` error, the error is an `internal`
  code with a hint; tell the user what is missing, and do not retry in a loop.
- `GenerateCaptions` transcribes the whole media file, so captions for speech outside the clip's
  `source_in_us` to `source_out_us` range are placed too. Check the new captions' times in
  `get_project` and delete the ones that fall outside the clip, with `allow_deletion` listing
  their ids.
- `GenerateCaptions` also stores the transcript (`get_transcript` returns it, with word ids) and
  each caption's `body.words`. If the Whisper build cannot report word timings, the captions come
  out sentence-level with no words; `SplitCaptions` then has nothing to re-page.
- Each call is its own undo step.
