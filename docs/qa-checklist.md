# Manual QA checklist (GUI rebuild M7)

Run after a fresh clone (`npm i && npm run tauri dev` from `renderly-app/`). Mark each
item pass/fail. Failures that crash the app or freeze the window are blockers.

## Smoke

- [ ] Fresh clone: `cargo build --workspace`, `cargo test --workspace`, `npm i && npm run tauri
      dev` start without errors
- [ ] Window opens ~1600×950; cannot shrink below ~1200px wide
- [ ] Welcome screen → **Import video to start** creates a project and places the clip

## Edit path

- [ ] Import a second clip (drop or Media panel); both appear in the bin
- [ ] Video bin cards show a shimmer skeleton, then a hover-scrub filmstrip
- [ ] Drag a clip onto the timeline; move / trim / snap; undo (Ctrl+Z) / redo (Ctrl+Y)
- [ ] Split (S or razor tool C); copy / paste / duplicate (Ctrl+C/V/D)
- [ ] Mute / lock / hide a track; rename via double-click on the header
- [ ] Aspect ratio menu: switch 9:16 ↔ 16:9; **Original** restores first video dims
- [ ] Editable timecode + ±1 frame; Space play/pause; Esc exits fullscreen preview

## Captions & audio

- [ ] Text panel → add caption; style gallery updates burn-in look
- [ ] Auto-captions (Whisper available) or graceful error toast if missing
- [ ] Audio panel → TTS voiceover (Piper or OpenAI key); clip lands on an audio track

## Export (M6)

- [ ] Export dialog: TikTok / YouTube / Project size presets
- [ ] Progress bar + ETA while rendering; Cancel mid-export cleans up (no orphan temp MP4)
- [ ] Completed export plays in an external player

## Stability

- [ ] Play a ≥60s 1080p clip and **drag the window** — UI stays responsive; ffmpeg process
      count stays flat (PlaybackEngine, not per-frame spawn)
- [ ] Edit while playing pauses playback cleanly (no stuck audio)
- [ ] Kill `ffmpeg` mid-play → toast via `playback:error`, app does not crash
- [ ] Stickers / Effects / Transitions / Filters / Adjust / Extensions panels work (no Coming Soon stubs)
- [ ] Empty timeline / empty media / empty audio show clear empty states
- [ ] Stickers place on a Stickers overlay track without overlap errors on Video 1
- [ ] Extensions can Load the starter pack and gain/invert plugins from the local registry

## Phase 4 (as features land)

- [ ] Clip mask / chroma key / background removal affect preview and export alpha
- [ ] **Mask tool (M):** paused preview — drag to create rect (Shift=ellipse, Alt=from
      center); move/resize handles; inspector Enable / Invert / Feather / shape switch;
      undo after mouseup commit (live drag does not spam undo); play hides overlay;
      Select (V) restores transform handles
- [ ] Audio denoise (afftdn) on an audio-track clip survives export
- [ ] Templates apply via command; multicam angle switch changes active angle
- [ ] **Linux (WebKitGTK):** preview canvas renders (WebGPU/WebCodecs support varies by
      distro — see docs/preview-webview.md "Risks"); handle/mask overlays stay above the
      canvas and clickable; resize realigns the canvas immediately (plain DOM, no
      OS-composited surface to lag)

## Nested sequences & adjustment layers (schema v7)

Use the browser harness sample project (it ships an "Intro" sequence on the "Nested" track and
an adjustment layer on "Adjustments") or any project with a few clips.

- [ ] **Open / breadcrumb:** double-click the nested clip -> a breadcrumb `Project > Intro`
      appears above the ruler, the timeline shows only that sequence's tracks, the transport
      duration is the sequence's, the preview shows the sequence at the local playhead (an
      adjustment layer inside it is visible). Click the project crumb -> back to the root.
- [ ] **Depth:** open a sequence, double-click a nested clip inside it -> breadcrumb grows;
      clicking the middle crumb truncates it.
- [ ] **Undo across containers:** inside a sequence add a clip / adjustment layer, `Ctrl+Z` ->
      it disappears and the sequence stays open. Undo a "create sequence" while that sequence
      is open -> the editor returns to the root (no blank timeline).
- [ ] **Edits inside:** drag a clip from the media panel into the open sequence, add a track
      (no "Caption track" offered inside a sequence), split / trim / move / copy-paste there;
      none of it touches the root timeline.
- [ ] **Nest:** select 2+ clips (Shift-click), right-click -> "Nest N clips" (or `Ctrl+G`) ->
      they are replaced by one clip, a new sequence appears in the media panel, one undo
      restores everything.
- [ ] **Unnest:** right-click the nested clip -> "Unnest" (`Ctrl+Shift+G`) -> the inner clips
      return on new tracks. Trim or change the speed of the nested clip first -> a
      "cannot unnest" error toast, nothing changes.
- [ ] **Media panel:** "Sequences" chip filters; the "New sequence" button adds an empty one;
      rename (pencil, Enter commits / Esc cancels); settings dialog sets width/height or
      inherits (empty field); delete a sequence that is placed -> "still used by a clip"
      toast; delete an unused one -> gone. Drag a sequence onto a video lane (picture clip)
      and onto an audio lane (audio clip); an empty sequence is refused with a toast.
- [ ] **Sequence size:** give a sequence its own square size (settings dialog, e.g. 1080x1080
      in a 1080x1920 project) and open it -> the preview canvas becomes square, the frame is not
      stretched, and the overlays line up with it; close it -> the project aspect returns.
- [ ] **Adjustment layer:** track-header menu (...) on a video track -> "Add adjustment layer"
      adds a 5 s amber block at the playhead and selects it; where a clip already sits at the
      playhead an overlap toast appears. Drag its edges (trim handles) and body; undo each.
- [ ] **Adjustment inspector:** timing fields, opacity slider (preview mix changes),
      "Add effects" -> Effects tab offers built-in effects and pack LUTs but no WASM plugins;
      stack shows in the inspector; opacity keyframe; mask presets (Mask tool M draws
      full-frame UVs); the "On" toggle disables it in the preview.
- [ ] **Preview vs export:** export a project with a nested sequence and an adjustment layer
      and compare a frame against the preview.
- [ ] **Audio:** with a sequence open, Play / scrub plays that sequence's mix (not the root's).

## Shortcuts quick check

| Key | Action |
|---|---|
| Space | Play / pause |
| V / C / M | Select / razor / mask tool |
| S | Split at playhead |
| ← / → | Step ±1 frame |
| Ctrl+Z / Y | Undo / redo |
| Ctrl+S | Save |
| Ctrl+C / V / D | Copy / paste / duplicate |
| Ctrl+G / Ctrl+Shift+G | Nest selection / unnest clip |
| Alt+Up | Close the open sequence |
| Delete | Delete selection |
| Esc | Exit fullscreen preview / close dialog |
