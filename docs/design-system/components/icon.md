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

## Use

- **Icon and word** (buttons, segmented controls, banners, status): the icon comes first, with a
  `space-4` gap. The icon is hidden from screen readers.
- **Icon only** (toolbars, track headers, card menus): use `IconButton`, which takes a `label`. The
  label is the button's accessible name and its tooltip, with the shortcut when there is one.
  Icon-only buttons are square at the control height.
- One meaning, one icon. If two controls need the same icon, they do the same thing.
- Status: `warning`, `error` and `info` always come with a word ("Changed outside", "Error").

## In code

- **List**: `packages/ui/icons/icons.json`. **Generated**: `packages/ui/src/icons.ts` (`ICONS`,
  `IconName`)
- **Styles**: `.r-icon` in `packages/ui/src/styles/base.css`
- **React**: `Icon` (`name`, optional `label`) and `IconButton` (`icon`, `label`, `shortcut`,
  `size`) from `@renderly/ui/react`
- **Preview**: `docs/design-system/preview/index.html` shows every icon with its name
