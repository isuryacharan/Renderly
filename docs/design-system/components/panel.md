# Window, panels and splitters

The editor window is a column: a 36px toolbar, any banners, then the panels. Panels sit on
`surface`, separated by a 1px gap that shows `bg`. No panel has a shadow or rounded corners.

## Toolbar and panel header

`toolbar` (36px) tall. The title sits on the left in `body-strong`; controls sit on the right.
Panel headers have a 1px `border` line underneath. The viewer and the timeline track area put their
body on `viewer`.

## Splitter

The 1px gap between two panels is also the handle that resizes them. Its hit area is 6px wide; it
turns `border-strong` on hover and while dragging.

- It is a focusable `role="separator"` with its value, minimum and maximum, and a label that says
  what it resizes ("Resize the timeline").
- Arrow keys move it 16px, Shift with an arrow 64px, Home and End go to the limits.
- Panel sizes are kept across reloads.

## Banner

A full-width notice under the toolbar for a state that lasts: "Read-only", "Changed outside". It
starts with a word in `body-strong`, coloured `warning` or `danger`, then one sentence, then the
actions. Never a coloured bar without a word.

## Save status

A word at the right of the toolbar: "Saved", "Unsaved", "Saving…" in `text-tertiary`; "Not saved"
in `danger` and "Changed outside" in `warning`.

## In code

- **Styles**: `packages/ui/src/styles/components/panel.css`
- **Classes**: `.r-app`, `.r-toolbar` (`__title`, `__spacer`), `.r-panel` (`__header`, `__body`,
  `__body--viewer`, `__empty`, `--viewer`), `.r-splitter` (`--vertical`, `--horizontal`),
  `.r-banner` (`--warning`, `--danger`, `__kind`), `.r-status` (`--warning`, `--danger`)
- **React**: `Splitter` from `@renderly/ui/react`
- **Preview**: `docs/design-system/preview/index.html`
