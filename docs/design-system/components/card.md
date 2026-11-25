# Project card

The home screen shows each project as a card. Cards are for the home screen and the store only;
the editor never uses them.

- On `surface` with a 1px `border` ring and `radius-lg`; `surface-hover` on hover. No shadow.
- A 16:9 cover on `viewer`. The cover shows the project's picture when there is one.
- The name in `body`, one line, cut with an ellipsis. Under it, in `small` and `text-tertiary`: the
  length in `timecode` and when it was last edited.
- The whole card is one button that opens the project. Its focus ring goes around the whole card.
- A small "More" menu button sits in the cover's top-right corner, above the card's button:
  Open, Rename…, Duplicate, then Delete after a separator. Delete moves the project to the recycle
  bin and offers Undo.
- Cards fill a grid with columns at least 200px wide and a `space-16` gap.

## In code

- **Styles**: `packages/ui/src/styles/components/card.css`
- **Classes**: `.r-card-grid`, `.r-card` (`__cover`, `__open`, `__name`, `__meta`, `__menu`)
- **Preview**: `docs/design-system/preview/index.html`
