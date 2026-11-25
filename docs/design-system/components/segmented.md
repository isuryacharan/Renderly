# Segmented control and tabs

Segmented controls and tabs switch between a few views or modes of the same thing.

- Use a **segmented control** for two to four mutually exclusive options that change a setting in place, such as preview quality or fit mode. The group sits in a `bg` well with a 1px `border`; the chosen option is a `surface-raised` pill with `text` at weight 600. It is neutral, not accent, because it is a setting and not a selection.
- Use **tabs** to switch the content of a panel, such as the inspector's Basic, Audio, Speed, Animation and Adjust. The current tab has a 2px `accent` underline and `text` at weight 600; others are `text-secondary`.

Keep labels to one or two words. If the options do not fit on one line, use a menu instead. Arrow keys move between options; the focus ring uses `focus`.

The consumer provides the options (label and value), the current value and the change handler.

## In code

- **Styles**: `packages/ui/src/styles/components/segmented.css`
- **Classes**: `.r-segmented` with `aria-pressed` buttons (`.r-segmented--icons` when the options are icons alone, each with a tooltip and an accessible name); `.r-tabs` with `role="tab"` and `aria-selected`
- **Preview**: `docs/design-system/preview/index.html`
