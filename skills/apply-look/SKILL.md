---
name: apply-look
description: Give clips of the open Renderly timeline a look: a LUT or filter, effects, a blend mode, transitions between clips, or an animated effect parameter, applied by ref with dry runs and a frame check. Use when the user asks for a colour grade, a film look, a blur, a fade or dissolve between clips, or an effect that changes over time.
---

# Apply a look

Use the `renderly` MCP tools: `describe_effects`, `get_project`, `apply_command`, `render_frame`.

## When NOT to use this

- The user wants to change timing, cuts or captions: use the other skills.
- The user names an effect that `describe_effects` does not list. Say it is not installed and
  offer the closest listed one; do not invent a ref.
- Several clips need different looks: do them one clip at a time, not with `targets`.

## Procedure

1. **Find what exists.** `describe_effects`. Each line is `ref kind "label" state slot=... | params`.
   Only use items whose state is `ok`. Effects are applied by ref (`builtin:blur@1`,
   `pack:<pack>/<item>@1`); never by file path. The `slot=` is where it normally goes: `filter`
   (a LUT or look), `key` (chroma key), `adjust` (colour adjustment), `effects` (anything else).
2. **Read the timeline.** `get_project`: note the track and clip ids to change, and
   `project_revision`. If `effect_status` is present, some effect on the timeline is `missing`,
   `changed` or `disabled`; tell the user which pack is missing and leave those effects in place
   (they keep their settings and come back when the pack is installed).
3. **Apply.** One `SetClipEffects` per change, with `expected_revision`. Slots run in the order
   filter, key, adjust, then the `effects` list, so choose the slot by when the effect should
   run. Omitted fields stay as they are; `null` clears a slot; `effects` replaces the whole list,
   so read the current list from `get_project` and send it back with your change (use an
   instance's `id` to edit it in place). Leave `params` out to get the effect's initial values.
   - Same look on many clips: `targets: [{ "track_id", "clip_id" }, ...]` in one command (one
     undo step; if any target is invalid none change).
   - Blend a clip over the layers below: `"blend_mode": "screen"` (or `multiply`, `overlay`, ...).
   - Dry-run the first command of each kind (`dry_run: true`), then send it for real.
4. **Transitions.** `SetClipTransition { track_id, clip_id, ref: "builtin:crossfade@1",
   duration: "0.5s" }` on the first of two touching clips. `ref: null` removes it. The duration
   must be at most half of each clip. For every cut: `targets` with the same `ref`.
5. **Animate.** To change a parameter over time, send `SetClipKeyframes` with the clip's full
   track list (read it first) plus a track whose `property` is `"effect:<instance-id>/<param>"`
   (the instance `id` from `get_project`); times are relative to the clip start. Numbers for
   numeric params, `[r,g,b,a]` / `[x,y]` for colour and position. Bool, choice and LUT params
   cannot be animated. Shape a span afterwards with `SetKeyframeEasing { property, time, easing }`
   (`linear`, `hold` for steps, `auto` for smooth, `ease_in`, `ease_out`, `ease_in_out`).
6. **Check.** `render_frame` at one time inside the clip (and one inside a transition): decode the
   `png_base64` to a `.png` in your scratch folder and open it with `Read`. If the look is wrong,
   change the params rather than stacking more effects.
7. **Report.** One or two sentences: what look went on which clips. Do not list ids or refs
   unless asked.

## Notes

- A param outside its range is clamped; an unknown param name is an error that lists the valid
  ones. A `blur` with `radius` 0 does nothing; ask for a visible amount.
- Each command is its own undo step. On `revision_conflict`, re-read and redo the edit.
- Do not delete effects the user did not ask you to remove; clear a slot only when asked.
