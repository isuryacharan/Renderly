# Dialog, popover, tooltip and toast

Things that float above the interface. All sit on `surface-raised` with `radius-lg` (tooltips:
`radius-md`) and carry their shadow's 1px ring instead of a border. Only the dialog has a scrim.
They open in 180ms with a fade and close immediately.

## Dialog

A dialog asks for a decision or a few values before something happens: a project name, a
confirmation before deleting for good.

- Use the native `<dialog>` opened with `showModal()`. It traps focus, makes the page behind inert,
  and closes on Escape.
- Shadow `shadow-dialog`, scrim `scrim`, padding `space-16`, at most 440px wide.
- Title in `title`. Explaining text in `text-secondary`.
- Actions at the bottom right, primary last. The primary button says what happens ("Create",
  "Delete"), never "OK".
- Focus starts on the first field, or on the primary action when there is no field.
- A destructive confirmation names what will be lost and says it cannot be undone.

## Popover

A popover shows a small form or a list next to the control that opened it. Use the `popover`
attribute (`popover="auto"`) for light dismiss and Escape. Padding `space-12`, shadow
`shadow-popover`. Menus are popovers that hold an `.r-menu`.

## Tooltip

A tooltip names an icon button's action and its shortcut. It appears after 500ms on hover and at
once on keyboard focus, sits below the control (above if there is no room), and never holds
anything interactive. Text `small`, shortcut in `text-tertiary`.

## Toast

A toast says what just happened: "Project duplicated", "Moved Holiday edit to the recycle bin". It
appears at the bottom centre for 4 seconds (8 with an action), at most three at a time, in a
`role="status"` region so screen readers announce it.

- One optional action, usually "Undo".
- An error toast starts with the word "Error" in `danger`. Errors say what went wrong and what to
  do.

## In code

- **Styles**: `packages/ui/src/styles/components/overlay.css`
- **Classes**: `.r-dialog` (`__body`, `__title`, `__text`, `__actions`), `.r-popover`
  (`--menu`), `.r-tooltip` (`__shortcut`), `.r-toasts`, `.r-toast` (`--danger`, `__kind`)
- **React**: `Dialog`, `MenuButton`, `PopoverButton`, `Tooltip`, `ToastProvider` and `useToast`
  from `@renderly/ui/react`
- **Preview**: `docs/design-system/preview/index.html`
