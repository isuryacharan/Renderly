# Menu

A menu lists actions for the thing that was clicked: a context menu on a clip, a toolbar drop-down, or a row in the command palette.

The menu floats on `surface-raised` with `radius-lg`, `space-4` padding and `shadow-popover`; it has no border because the shadow carries a 1px ring. Items are `control-md` tall with `radius-sm`. The highlighted item fills with `accent` and its label and shortcut switch to `on-accent`, the way a system menu does. Shortcuts are right-aligned in `text-tertiary`.

- Order items by how often they are used, and group related ones between 1px `border` separators.
- An item that opens a dialog ends with an ellipsis.
- A destructive item is labelled in `danger` and sits last, after a separator.
- Disabled items use `text-disabled` and do not highlight. Prefer disabling to hiding, so the menu's shape stays familiar.
- Menus open in 180ms with no slide, close immediately, and never have a scrim.
- Arrow keys move, Enter activates, Escape closes, and typing a letter jumps to the matching item.

The consumer provides the items (label, shortcut, action, disabled or destructive flags) and the anchor.

## In code

- **Styles**: `packages/ui/src/styles/components/menu.css`
- **Classes**: `.r-menu`, `.r-menu__item` (with `--danger`, `data-highlighted`), `.r-menu__shortcut`, `.r-menu__separator`
- **Preview**: `docs/design-system/preview/index.html`
