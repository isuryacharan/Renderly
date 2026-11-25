# Media bin and badge

## Media bin

The library panel lists the project's media. It sits on `surface` like any panel.

- **Toolbar:** **Import** (icon and word: the panel's main action), an icon-only button to import
  a folder, the "Copy into project" checkbox, and a grid/list switch with icons alone.
- **Grid:** pictures at least 120px wide, 16:9, on `viewer`. The thumbnail keeps its aspect ratio
  and is never stretched, tinted or dimmed. The length sits in the picture's bottom-right corner in
  `timecode`, on `scrim` with `on-clip` text.
- **List:** a 40px picture, then the name over a line of facts.
- **Facts:** length and picture size in `timecode` and `text-tertiary`, then badges: the codec,
  "In timeline" when clips use the item, and the status.
- **Status:** **Offline** (`warning` icon and word) when the file cannot be read, with the reason
  in its tooltip; **Cannot play** (`error` icon and word) when this browser cannot decode it, with
  the codec named. Above the items, a notice says how many files are offline, with **Reconnect**
  when the browser only needs permission again and **Find in a folder…** to relink.
- **More actions** (icon-only menu, top right in the grid, end of the row in the list): Relink… or
  Reconnect when offline, then **Remove from project** (destructive, last). Removing an item that
  clips use asks first and says how many clips go with it; the file on disk is never touched.
- **Drop:** files and folders can be dropped anywhere on the panel. While something is dragged
  over it, the panel shows a dashed `accent` outline.

## Badge

A short fact about an item: 16px tall, `radius-sm`, caption text at weight 500 in
`text-secondary`, on `surface-raised` with a 1px `border` ring. A status badge has a 12px icon and a
word, coloured `warning` or `danger`; never colour alone.

## In code

- **Styles**: `packages/ui/src/styles/components/bin.css`
- **Classes**: `.r-badge` (`--warning`, `--danger`), `.r-bin` (`__toolbar`, `__notice`, `__drop`,
  `__items--grid`, `__items--list`, `__item`, `__thumb`, `__duration`, `__text`, `__name`,
  `__facts`, `__detail`, `__menu`)
- **App**: `apps/editor/src/editor/MediaBin.tsx`
- **Preview**: `docs/design-system/preview/index.html`
