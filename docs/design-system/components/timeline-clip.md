# Timeline clip

A timeline clip is a block on a track that stands for a piece of media, text or an effect over a span of time.

The preview is a styling reference: in the app the track area is drawn on one canvas, using the same tokens.

- Tracks sit on `viewer`, separated by 1px `border` lines. Video tracks are `track-video` tall, audio tracks `track-audio`, and text and effect tracks `track-aux`. Track headers sit on `surface` with a `caption` label in `text-secondary`.
- A clip is filled by kind — `clip-video`, `clip-audio`, `clip-text`, `clip-effect` — with `radius-sm` corners, a `space-2` gap to its neighbours, and its name in `small` and `on-clip` at the top left, truncated with an ellipsis.
- **Every video and image clip shows thumbnails.** A filmstrip of frames fills the clip between its name row and its audio strip. Each tile keeps the source aspect ratio at the strip's height, with a 1px gap in the clip colour between tiles; the last tile is cut off by the clip edge, never squeezed.
- Tiles are pinned to source time: the first tile is the frame at the clip's in-point, and trimming or zooming reveals or hides tiles instead of sliding the pictures around.
- Thumbnails are shown at full strength, with no tint and no dimming. A still image repeats its one picture. A clip too narrow for one tile shows the left part of its first frame.
- While frames are loading, the strip shows the plain clip colour and tiles appear as they are decoded, nearest the centre of the view first. A coarser cached tile stands in until the exact one is ready. Never show a spinner on a clip.
- **Every audio clip shows its waveform.** It is a filled shape mirrored about the centre line, in `on-clip` at about 70% strength, filling the clip from under its name to its bottom edge. Silence is a hairline, never blank, so a quiet clip still reads as audio.
- A video clip that has sound shows a slim 14px waveform along its bottom edge, separated from the picture area by a faint line.
- The waveform shows what will be heard: it scales with clip gain, and fades are drawn as a 1px `on-clip` curve over it. Peaks that clip are drawn in `danger`.
- While peaks are still being computed, draw a flat centre line and fill the waveform in from left to right as data arrives. Never show a spinner on a clip.
- At low zoom draw the peak envelope; at high zoom, where one pixel covers a few samples, draw the actual sample line.
- **Selected**: a 2px `selection` outline drawn inside the clip's bounds with a 1px `selection-edge` line inside it, on top of the thumbnails. The fill does not change, so the clip's kind stays readable.
- **Keyframes**: 8px `selection` diamonds with a 1px `selection-edge` ring, over the filmstrip.
- **Offline media**: the clip keeps its position and size, shows a `warning` icon with the word "Offline", and loses its thumbnails.
- **Disabled or hidden**: the clip is drawn at half strength; its label stays readable.
- The playhead is a 1px `playhead` line with a small handle in the ruler. It is the only thing on the timeline in that colour.
- Ruler labels use `timecode` at 11px in `text-tertiary`.

Clips never animate into place. Moving, trimming and snapping show the result immediately.

The consumer provides the clip's kind, name, start and duration, selection state, keyframe times and media state.

## In code

- **Styles**: none: the track area is drawn on a canvas. Use the resolved values in `packages/ui/src/tokens.ts`
- **Classes**: see the reference markup in `docs/design-system/preview/timeline.fragment.html`
- **Preview**: `docs/design-system/preview/index.html`
