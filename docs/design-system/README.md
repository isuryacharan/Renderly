# Renderly design system

Minimal, modern and native-feeling. This is the single reference for how Renderly looks and
behaves. Read it before building or changing any interface.

| Where | What |
|---|---|
| `packages/ui/tokens/tokens.json` | **Source of truth** for every colour, size, radius, shadow and text style, each with a usage note |
| `packages/ui/src/styles/tokens.css` | Generated CSS variables and `.text-*` classes. Do not edit |
| `packages/ui/src/tokens.ts` | Generated resolved values, for canvas drawing (timeline, scopes, waveforms). Do not edit |
| `packages/ui/src/styles/` | Base styles and component styles (`.r-*` classes) |
| `docs/design-system/components/` | Guidelines per component |
| `docs/design-system/preview/index.html` | Live preview of everything, with a theme switch. Open it in a browser |

To change a token, edit `tokens.json`, then run `pnpm --filter @renderly/ui build:tokens`
(or `node packages/ui/scripts/build-tokens.mjs`). Never hard-code a colour, size or radius in a
component: use the variable, and add a token with a usage note if one is missing.

Renderly is a video editor that runs in the browser and should feel like software installed on the machine. The interface is quiet, dense and neutral so the picture is the only colourful thing on screen.

## Principles

1. **The video is the content. The interface is a tool.** Chrome is neutral grey. Colour appears only where it carries meaning: the accent for selection, the clip colours for track kinds, and the three status colours.
2. **Look native, not like a website.** Use compact controls, hairline dividers, real menus and one plain, precise typeface. No hero sections, no marketing gradients, no oversized rounded cards inside the editor.
3. **One accent, amber.** `accent` marks what is selected, active or primary. Amber was chosen because it is far from every clip colour and from most footage, so a selection is never lost against the picture. If two things on screen are accent-coloured and neither is selected or primary, one of them is wrong.
4. **Dense, but calm.** Controls are 22–28px tall and text is 11–13px. Calm comes from alignment and consistent spacing, not from empty space.
5. **Dark first.** Dark is the default theme because picture work is judged against a dark, neutral surround. Light is a full theme, not an inversion.

## Colour

- Build every screen from four neutral layers, back to front: `viewer` → `bg` → `surface` → `surface-raised`. A layer may only sit on a layer behind it in that order.
- Put the viewer and the timeline track area on `viewer`. Never tint it, and never place a coloured panel edge against the picture.
- Put panels on `surface`, separated by 1px `border` lines or by a gap that shows `bg`. Do not give panels shadows.
- Put secondary buttons, menus, popovers and dialogs on `surface-raised`.
- Put text, number and select fields on `field`: a recessed well, darker than the panel in the dark theme and white in the light theme.
- Set text in `text`, labels in `text-secondary`, and hints, units and shortcuts in `text-tertiary`. `text-disabled` is for disabled controls only.
- Use `accent` as a fill with `on-accent` on top. `on-accent` is near-black in the dark theme and white in the light theme, so never hard-code white on the accent. For accent-coloured text or icons on a neutral surface use `accent-text`, never `accent`.
- On timeline clips and on the viewer picture, draw selection outlines, keyframes and handles in `selection` with a 1px `selection-edge` line. These two are the same in both themes.
- Use `accent-subtle` behind a selected list row or an active toggle; keep its text in `text`.
- Edge a field with a 1px `border` hairline at rest and `border-strong` on hover; its `field` fill and its label mark it as a control. Edge a checkbox with `border-strong`.
- Draw slider tracks and switch pills in `groove`, filled with `accent` up to the value. Knobs are `thumb` with `shadow-thumb`, white in both themes.
- Draw the playhead in `playhead` and use that colour for nothing else.
- Fill timeline clips with `clip-video`, `clip-audio`, `clip-text` and `clip-effect`, and label them in `on-clip`. The clip colours are the same in both themes. A clip's kind is also shown by its track and its icon, never by colour alone.
- Show `danger`, `warning` and `success` always with an icon or a word. `warning` is yellow and sits next to the amber accent, so a warning is never shown as a bare coloured dot or fill. `success` sits toward blue so it stays distinct from `danger` for people who cannot tell red from green.
- Do not add gradients, tinted panels or a second accent. Blue in the interface means a video clip, nothing else.

## Type

- Set the interface in **Geist** through `--font-sans`, and numbers in **Geist Mono** through `--font-mono`. Both are free under the SIL Open Font License and available from Google Fonts. Use no other typeface in the interface.
- Ship the font files with the app (the variable files, weights 400–600) so the editor works offline and never waits on a font request. The licence allows this. The system typeface is only the fallback while the files load.
- Fonts chosen for captions and titles belong to the user's video, not to the interface.
- Set numbers that change or line up — timecodes, frame counts, numeric fields, ruler labels — in `timecode` (`--font-mono`) with `font-variant-numeric: tabular-nums`.
- Default to `body` (13px). Use `small` (12px) for property labels and clip names, and `caption` (11px) for track headers, badges and units.
- Use `body-strong` for inspector section titles and `title` for dialog and panel titles. `heading` and `display` are for the home screen and the store only.
- Use weight, not size, for emphasis inside the editor. Two weights only: 400 and 600 (500 for `caption` and the mono styles).
- Write labels in sentence case: "Export video", not "Export Video". No uppercase labels.

## Spacing and size

- Everything sits on a 4px grid, with `space-2` and `space-6` for tight interiors.
- Controls are `control-md` (28px) by default and `control-sm` (22px) inside the inspector and track headers. Dialog primary buttons may use `control-lg`.
- Toolbars and panel title bars are `toolbar` (36px) tall.
- Pad controls horizontally with `space-8`, panels with `space-12`, dialogs with `space-16`.
- Separate property rows by `space-6` and inspector sections by `space-16` with a `border` line between them.
- Icons are `icon` (16px). Icon-only buttons are square at the control height.
- Pointer targets are at least 22px. On touch devices raise controls to 32px or more.

## Shape and elevation

- Use `radius-sm` for clips, menu items, badges and checkboxes; `radius-md` for buttons and fields; `radius-lg` for menus, popovers, dialogs and cards; `radius-full` for sliders, switches and progress bars.
- Panels have square corners where they meet the window edge.
- Only things that float have a shadow, plus slider and switch knobs (`shadow-thumb`): `shadow-popover` for menus, popovers, tooltips and dragged items; `shadow-dialog` for dialogs and the command palette. Each shadow includes a 1px ring, so floating surfaces need no border.
- Put `scrim` behind a dialog. Menus and popovers have no scrim.

## States

- **Hover**: neutral controls go to `surface-hover`; accent controls go to `accent-hover`. Hover never changes size or adds a shadow.
- **Pressed**: one step darker than hover, with no movement.
- **Selected**: `accent` fill with `on-accent` for the single current item (active tab pill, selected clip outline, checked box); `accent-subtle` for selected rows in a list.
- **Focus**: a 2px solid `focus` ring, offset 1px, shown for keyboard focus only (`:focus-visible`). Never remove it. Fields are the exception: their edge turns into a 1px `focus` line with a 2px `accent-subtle` halo, shown whenever the caret is inside.
- **Disabled**: `text-disabled` for label and icon, no hover, cursor unchanged. Do not lower opacity of the whole control.
- **Mixed value** (multi-selection): show an en dash in the field and a half-filled slider track.

## Motion

- Keep motion short and functional: 120ms for hover and press, 180ms for menus, popovers and panel resize, with an ease-out curve.
- Nothing bounces, and nothing animates on a loop except a progress indicator.
- The timeline, playhead and viewer never animate between states. They show the exact frame immediately.
- Honour `prefers-reduced-motion` by removing all transitions.

## Iconography

- Outline icons on a 16px grid with a 1.5px stroke, round caps and joins, drawn in `currentColor`. No filled or two-tone icons, and no emoji in the interface.
- An icon without a visible label has a tooltip that names the action and its shortcut.
- No icon set has been chosen or drawn yet. Until one is, use plain text labels where an icon would be ambiguous.

## Writing

- Name things by what the person sees: "Export video", "Remove background", "Missing media".
- Buttons say what happens: "Export", "Delete clip", "Relink". After the action, say what happened: "Exported", "Clip deleted".
- Errors say what went wrong and what to do: "This file uses a codec your browser cannot decode. Convert it, or try another browser."
- No exclamation marks and no apologies.
- Write shortcuts with the platform's own symbols and order.

## The editor's own surfaces

- **Timeline**: tracks sit on `viewer`, separated by 1px `border` lines. Clips have `radius-sm`, a `space-2` gap between neighbours, and their name in `small` at the top left. A selected clip gets a 2px `selection` outline inside its bounds with a 1px `selection-edge` line; its fill does not change. Video tracks are `track-video` tall, audio tracks `track-audio`, text and effect tracks `track-aux`.
- **Thumbnails**: every video and image clip shows a filmstrip of its own frames between its name and its audio strip. Frames keep the clip's aspect ratio and are never stretched, tinted or dimmed; they are the user's picture.
- **Waveforms**: every audio clip shows its waveform, always. Draw it as a filled shape mirrored about the clip's centre line, in `on-clip` at about 70% strength, filling the clip under its name. A video clip with sound shows a slim waveform along its bottom edge. The waveform follows the clip's gain and its fades, so what is drawn is what will be heard.
- **Keyframes**: 8px diamonds. In the inspector: filled `accent` on a key, outlined `accent-text` when the property is animated but the playhead is between keys, outlined `text-tertiary` when it is not animated. On clips: filled `selection` with a `selection-edge` ring.
- **Viewer**: the picture sits on `viewer` with no border and no shadow. Overlay handles are 1px `thumb` lines with `selection` square handles edged in `selection-edge`.
- **Property row**: label, slider, numeric field, keyframe control, reset, in that order, on one `control-sm` line. Every numeric property in the inspector uses this row.
- **Meters and scopes**: draw on `viewer`. Levels use `success`, then `warning`, then `danger` for clipping.
