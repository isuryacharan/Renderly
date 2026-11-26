# Property row

The property row is the one pattern for editing a numeric value in the inspector: label, slider, numeric field, keyframe control and reset, on a single `control-sm` line.

Use it for every numeric property, whether built in or generated from a pack's parameter list. Do not invent a different layout for a single property.

- **Label**: `small` in `text-secondary`, truncated with an ellipsis. Dragging on the label scrubs the value.
- **Slider**: the Field slider — a 4px `groove` track with an `accent` fill and a 14px `thumb` knob with `shadow-thumb`. A drag is one undo step and pauses playback when it starts.
- **Field**: the Field numeric well at `control-sm` — `field` fill, 1px `border` hairline, `radius-sm`, value in `timecode`, right-aligned and tabular, unit in `text-tertiary`. Drag sideways to scrub, click to type. Enter commits, Escape cancels, arrow keys step, Shift steps finely.
- **Keyframe**: an 8px diamond with three states — filled `accent` when the playhead is on a key, outlined `accent-text` when the property is animated, outlined `text-tertiary` when it is not. A click adds a key at the playhead, or removes the one there; right-click (or the menu key) opens the easing menu: linear, ease in, ease out, ease in and out, auto, hold. When the property is animated, narrow arrows either side jump to the previous and next key; they are hidden when there is none that way. A section header may carry the same control as a group key for all its properties.
- **Reset**: returns the property to its default. Double-clicking the slider does the same.

With several clips selected and differing values, the field shows an en dash, the track has no fill and the knob is a hollow `groove` ring.

The consumer provides the label, value, range, step, unit, default, the keyframe state and the change handlers (begin, preview, commit).

## In code

- **React**: `PropertyRow`, `PropertySection` and `KeyframeControl` in `@renderly/ui/react`. A row reports a drag as `onBegin`, `onPreview` and one `onCommit` (make it one undo step), and typing or reset as a single `onCommit`. Dragging the label moves one step per pixel (at least 1/2000 of the range); Shift is ten times finer.
- **Styles**: `packages/ui/src/styles/components/property-row.css`
- **Classes**: `.r-property-section`, `.r-property-row`, `.r-property-row__label`, `.r-keyframe` (with `--animated`, `--on`)
- **Preview**: `docs/design-system/preview/index.html`
