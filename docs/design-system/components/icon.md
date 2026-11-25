# Icon

Icons make controls quicker to find and recognise. They never replace a word where the meaning
would be unclear, and they never carry meaning by colour alone.

## The set

Icons come from Tabler Icons (outline, MIT licence). Only the icons in
`packages/ui/icons/icons.json` ship with the app. Each entry has:

- `name`: what the code uses, named for the meaning ("delete", "reconnect", "theme-dark");
- `tabler`: the source icon;
- `usage`: when to use it.

To add an icon, find it at tabler.io/icons, add an entry with a usage note, and run
`node packages/ui/scripts/build-icons.mjs`. That writes `src/icons.ts` and keeps Tabler's licence
in `icons/LICENSE-tabler.txt`. CI fails if the generated file is out of date
(`pnpm check:icons`). Do not paste SVG markup into components, and do not use an icon from another
set.

## Drawing

- 16px (`icon`), with a 1.5px line, round caps and joins, in `currentColor`. Tabler draws on a
  24-unit grid; `.r-icon` sets a 2.25-unit stroke so the line is 1.5px at 16px.
- Outline only: no filled or two-tone icons.
- Icon colour follows the text it sits with: `text-secondary` in icon buttons, `text` on hover,
  `accent-text` when pressed, and the status colour inside a status word.

## Icon alone, or icon and word

Show the icon alone when the word beside it would be too obvious to be worth the space: anyone
would read the same action from the icon in that place. Otherwise keep the word.

Icon alone:

- Universal actions: home, undo, redo, play, pause, go to start or end, close, more actions, zoom
  in and out, search.
- Choices where the pictures explain each other: the theme switch (moon, sun, screen), mute,
  hide and lock toggles in a track header.
- Repeated controls in a dense place, where the first one teaches the rest: track headers, the
  timeline toolbar.

Keep the word:

- Destructive or irreversible actions: "Delete", "Delete for good".
- The screen's main action: "New project", "Export".
- Status: "Saved", "Read-only", "Changed outside", "Error". A status colour or icon never stands
  alone.
- Choices whose icons look alike or mean nothing on their own: "Browser storage" and "Disk folder",
  "Reconnect", "Recycle bin".
- Actions people use rarely, and anything in a dialog.

When unsure, keep the word. Removing a label later is easy; recovering from a misread action is
not.

## Use

- **Icon and word** (buttons, segmented controls, banners, status): the icon comes first, with a
  `space-4` gap. The icon is hidden from screen readers.
- **Icon only**: use `IconButton`, which takes a `label`, or give a segmented control
  `.r-segmented--icons` with a `label` on each option. The label is the accessible name and the
  tooltip, with the shortcut when there is one. Icon-only buttons are square at the control height.
- One meaning, one icon. If two controls need the same icon, they do the same thing.
- Status: `warning`, `error` and `info` always come with a word ("Changed outside", "Error").

## In code

- **List**: `packages/ui/icons/icons.json`. **Generated**: `packages/ui/src/icons.ts` (`ICONS`,
  `IconName`)
- **Styles**: `.r-icon` in `packages/ui/src/styles/base.css`
- **React**: `Icon` (`name`, optional `label`) and `IconButton` (`icon`, `label`, `shortcut`,
  `size`) from `@renderly/ui/react`
- **Preview**: `docs/design-system/preview/index.html` shows every icon with its name
