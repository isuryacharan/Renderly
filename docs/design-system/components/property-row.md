# Property row

The property row is the one pattern for editing a numeric value in the inspector: label, slider, numeric field, keyframe control and reset, on a single `control-sm` line.

Use it for every numeric property, whether built in or generated from a pack's parameter list. Do not invent a different layout for a single property.

- **Label**: `small` in `text-secondary`, truncated with an ellipsis. Dragging on the label scrubs the value.
- **Slider**: the Field slider — a 4px `groove` track with an `accent` fill and a 14px `thumb` knob with `shadow-thumb`. A drag is one undo step and pauses playback when it starts.
- **Field**: the Field numeric well at `control-sm` — `field` fill, 1px `border` hairline, `radius-sm`, value in `timecode`, right-aligned and tabular, unit in `text-tertiary`. Drag sideways to scrub, click to type. Enter commits, Escape cancels, arrow keys step, Shift steps finely.
- **Keyframe**: an 8px diamond with three states — filled `accent` when the playhead is on a key, outlined `accent-text` when the property is animated, outlined `text-tertiary` when it is not. Right-click opens the easing menu.
- **Reset**: returns the property to its default. Double-clicking the slider does the same.

With several clips selected and differing values, the field shows an en dash, the track has no fill and the knob is a hollow `groove` ring.

The consumer provides the label, value, range, step, unit, default, the keyframe state and the change handlers (begin, preview, commit).

## In code

- **Styles**: `packages/ui/src/styles/components/property-row.css`
- **Classes**: `.r-property-section`, `.r-property-row`, `.r-property-row__label`, `.r-keyframe` (with `--animated`, `--on`)
- **Preview**: `docs/design-system/preview/index.html`
