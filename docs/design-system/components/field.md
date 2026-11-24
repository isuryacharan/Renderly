# Field, slider, checkbox and switch

Fields take typed or chosen values: text, numbers, a choice from a list, a slider, a checkbox or a switch. They share one look so a form reads as one object.

**Text, number and select fields** are recessed wells: a `field` fill, `radius-md` corners and a 1px inset `border` hairline. The well is darker than the panel in the dark theme and white in the light theme, the way a system text box is. Height is `control-md`, or `control-sm` in the inspector with `radius-sm`.

- Hover strengthens the edge to `border-strong`.
- Focus replaces the edge with a 1px `focus` line plus a 2px `accent-subtle` halo. The ring hugs the field instead of sitting 1px outside it.
- Selected text is `accent` with `on-accent`. Placeholders and units are `text-tertiary`.
- Numeric fields are right-aligned in `timecode` with tabular figures. Dragging sideways on one scrubs its value; clicking edits it.
- A select shows a 16px chevron in `text-tertiary` at the right and opens a Menu.
- An invalid field has a 1px `danger` edge, and a message underneath in `danger` with an icon that says how to fix it.
- A disabled field loses its fill and sets its value in `text-disabled`.

**Slider**: a 4px `groove` track with an `accent` fill up to the value, and a 14px `thumb` knob carrying `shadow-thumb`. Hover and drag add a 4px `accent-subtle` halo around the knob. Keyboard focus puts a `focus` ring around the knob. The hit area is the full `control-sm` height, not just the track.

**Checkbox**: 16px, `radius-sm`, a `field` fill with a `border-strong` edge. Checked, it fills with `accent` and shows an `on-accent` tick.

**Switch**: 28 × 16px pill in `groove` with a 12px `thumb` knob. On, the pill is `accent` and the knob slides right. Use a switch for a setting that applies immediately and a checkbox for an option that applies when a dialog is confirmed.

Every field has a visible label, to the left in forms and the inspector. Never use the placeholder as the label.

The consumer provides the label, the value, the change handlers, and where relevant the unit, range, step, options, validity message and disabled state.

## In code

- **Styles**: `field.css`, `slider.css` and `choice.css` in `packages/ui/src/styles/components/`
- **Classes**: `.r-field` (with `--number`, `--sm`, `--invalid`, `--disabled`), `.r-field__unit`, `.r-field-message`, `.r-form`, `.r-slider` (set `--value`), `.r-checkbox`, `.r-switch`, `.r-choice`
- **Preview**: `docs/design-system/preview/index.html`
