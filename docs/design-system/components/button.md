# Button

A button starts an action. Renderly has four kinds and one accent rule: at most one primary button per view.

- **Primary** (`accent` fill, `on-accent` label, weight 600): the single main action of a dialog or screen, such as "Export".
- **Secondary** (`surface-raised` with a 1px inset `border`): every other action that needs a visible button.
- **Ghost** (no fill until hover): actions inside toolbars and dense panels, and "Cancel".
- **Icon** (square, `text-secondary` icon): toolbar tools. Always give it an accessible name and a tooltip with the shortcut.

Destructive actions use the secondary shape with a `danger` label, never a red fill.

Height is `control-md`; use `control-sm` inside the inspector and track headers, and `control-lg` for a dialog's primary button. Corners are `radius-md`. Hover moves neutral buttons to `surface-hover` and the primary to `accent-hover`. Disabled buttons keep their shape and switch label and icon to `text-disabled`.

The consumer provides the label (a verb, sentence case), an optional leading 16px icon, the kind, the size and the action.

## In code

- **Styles**: `packages/ui/src/styles/components/button.css`
- **Classes**: `.r-button`, with `--primary`, `--ghost`, `--danger`, `--icon`, and sizes `--sm`, `--lg`
- **Preview**: `docs/design-system/preview/index.html`
