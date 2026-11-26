// React side of the actions registry: a context, buttons and menu items made from actions, and
// the keyboard shortcuts list.

import { Button, Dialog, Icon, IconButton, Tooltip } from "@renderly/ui/react";
import { createContext, useContext, useSyncExternalStore, type ReactNode } from "react";
import { formatKey, type Action, type Actions } from "./registry";

const ActionsContext = createContext<Actions | null>(null);

export function ActionsProvider({ actions, children }: { actions: Actions; children: ReactNode }) {
  return <ActionsContext.Provider value={actions}>{children}</ActionsContext.Provider>;
}

/** The registry, re-rendering the caller whenever enabled or pressed states may have changed. */
export function useActions(): Actions {
  const actions = useContext(ActionsContext);
  if (!actions) throw new Error("useActions needs an ActionsProvider");
  useSyncExternalStore(
    (listener) => actions.subscribe(listener),
    () => actions.getVersion(),
  );
  return actions;
}

/** An icon button for an action: its icon, label, shortcut, enabled and pressed state. */
export function ActionButton({
  id,
  size = "sm",
  args,
}: {
  id: string;
  size?: "sm" | "md";
  args?: Record<string, unknown>;
}) {
  const actions = useActions();
  const action = actions.get(id);
  const pressed = actions.isPressed(id);
  const shortcut = actions.shortcut(id);
  return (
    <IconButton
      icon={action.icon ?? "more"}
      label={action.label}
      {...(shortcut ? { shortcut } : {})}
      size={size}
      disabled={!actions.isEnabled(id)}
      {...(pressed === undefined ? {} : { "aria-pressed": pressed })}
      onClick={() => actions.run(id, args)}
    />
  );
}

/** A text button for an action (main actions keep their word, per the icon rule). */
export function ActionTextButton({ id, children }: { id: string; children?: ReactNode }) {
  const actions = useActions();
  const action = actions.get(id);
  const shortcut = actions.shortcut(id);
  const button = (
    <Button size="sm" disabled={!actions.isEnabled(id)} onClick={() => actions.run(id)}>
      {action.icon && <Icon name={action.icon} />}
      {children ?? action.label}
    </Button>
  );
  return shortcut ? (
    <Tooltip label={action.label} shortcut={shortcut}>
      {button}
    </Tooltip>
  ) : (
    button
  );
}

/** Buttons for a segmented group of toggle actions (the timeline tools). */
export function ActionSegment({ ids, label }: { ids: readonly string[]; label: string }) {
  const actions = useActions();
  return (
    <div className="r-segmented r-segmented--icons" role="group" aria-label={label}>
      {ids.map((id) => {
        const action = actions.get(id);
        const shortcut = actions.shortcut(id);
        return (
          <Tooltip key={id} label={action.label} {...(shortcut ? { shortcut } : {})}>
            <button
              type="button"
              aria-label={action.label}
              aria-pressed={actions.isPressed(id) ?? false}
              onClick={() => actions.run(id)}
            >
              <Icon name={action.icon ?? "more"} />
            </button>
          </Tooltip>
        );
      })}
    </div>
  );
}

/** A menu item for an action, for `MenuButton`'s `items`. */
export function actionItem(
  actions: Actions,
  id: string,
  args?: Record<string, unknown>,
  danger = false,
): {
  label: string;
  onSelect: () => void;
  disabled?: boolean;
  shortcut?: string;
  danger?: boolean;
} {
  const shortcut = actions.shortcut(id);
  return {
    label: actions.get(id).label,
    onSelect: () => actions.run(id, args),
    disabled: !actions.isEnabled(id),
    ...(shortcut ? { shortcut } : {}),
    ...(danger ? { danger } : {}),
  };
}

const GROUPS: Action["group"][] = [
  "Playback",
  "Timeline",
  "Viewer",
  "Edit",
  "Media",
  "Project",
  "Help",
];

/** Every action with a shortcut, grouped. */
export function ShortcutsDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const actions = useActions();
  const all = actions.list().filter((action) => action.keys?.length);
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title="Keyboard shortcuts"
      actions={<Button onClick={onClose}>Close</Button>}
    >
      <div className="shortcuts">
        {GROUPS.map((group) => {
          const rows = all.filter((action) => action.group === group);
          if (rows.length === 0) return null;
          return (
            <section key={group} className="shortcuts__group">
              <h3 className="text-body-strong shortcuts__title">{group}</h3>
              <dl className="shortcuts__list">
                {rows.map((action) => (
                  <div key={action.id} className="shortcuts__row">
                    <dt>{action.label}</dt>
                    <dd>
                      {(action.keys ?? []).map((key) => (
                        <kbd key={key} className="shortcuts__key">
                          {formatKey(key)}
                        </kbd>
                      ))}
                    </dd>
                  </div>
                ))}
              </dl>
            </section>
          );
        })}
        <p className="shortcuts__note">
          Shortcuts do not fire while you type in a field. In the library, Enter or E adds the
          focused file to the timeline at the playhead, and Delete removes it from the project.
        </p>
      </div>
    </Dialog>
  );
}
