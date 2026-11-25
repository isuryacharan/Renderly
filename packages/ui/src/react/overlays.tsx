// Floating things: dialog, menu, popover, tooltip. They use the browser's own <dialog> and
// popover attribute, which give focus trapping, Escape and light dismiss without a library.

import {
  cloneElement,
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type ReactElement,
  type ReactNode,
} from "react";

/** Places a floating element below its anchor (or above, if there is no room), inside the window. */
export function place(anchor: Element, floating: HTMLElement, gap = 4): void {
  const a = anchor.getBoundingClientRect();
  const f = floating.getBoundingClientRect();
  const below = a.bottom + gap + f.height <= innerHeight;
  const top = below ? a.bottom + gap : Math.max(gap, a.top - gap - f.height);
  const left = Math.min(Math.max(gap, a.left), innerWidth - f.width - gap);
  floating.style.top = `${top}px`;
  floating.style.left = `${left}px`;
}

// --- dialog ---

export interface DialogProps {
  open: boolean;
  /** Called on Escape and when an action closes it. */
  onClose: () => void;
  title: string;
  children?: ReactNode;
  /** Buttons, primary last. */
  actions: ReactNode;
}

export function Dialog({ open, onClose, title, children, actions }: DialogProps) {
  const ref = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    if (!open && dialog.open) dialog.close();
  }, [open]);
  return (
    <dialog
      ref={ref}
      className="r-dialog"
      aria-labelledby={titleId}
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
    >
      {open && (
        <>
          <div className="r-dialog__body">
            <h2 id={titleId} className="r-dialog__title text-title">
              {title}
            </h2>
            {children}
          </div>
          <div className="r-dialog__actions">{actions}</div>
        </>
      )}
    </dialog>
  );
}

// --- menu ---

export type MenuEntry =
  | {
      label: string;
      onSelect: () => void;
      shortcut?: string;
      danger?: boolean;
      disabled?: boolean;
    }
  | "separator";

export interface MenuButtonProps {
  /** Visible text of the button. */
  label: ReactNode;
  /** Accessible name when the label is not enough (e.g. "Actions for Holiday edit"). */
  "aria-label"?: string;
  items: MenuEntry[];
  className?: string;
  /** For an icon-only button: the tooltip naming it. */
  tooltip?: string;
}

export function MenuButton({ label, items, className, tooltip, ...rest }: MenuButtonProps) {
  const button = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const id = useId();
  const [open, setOpen] = useState(false);

  const focusItem = useCallback((which: "first" | "last" | 1 | -1) => {
    const options = [
      ...(menu.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)') ??
        []),
    ];
    if (options.length === 0) return;
    const current = options.indexOf(document.activeElement as HTMLButtonElement);
    const next =
      which === "first"
        ? 0
        : which === "last"
          ? options.length - 1
          : (current + which + options.length) % options.length;
    options[next]?.focus();
  }, []);

  const show = (focus: "first" | "last") => {
    const el = menu.current;
    if (!el || !button.current) return;
    el.showPopover();
    place(button.current, el);
    focusItem(focus);
  };

  const trigger = (
    <button
      ref={button}
      type="button"
      className={className ?? "r-button"}
      aria-haspopup="menu"
      aria-expanded={open}
      aria-controls={id}
      aria-label={rest["aria-label"]}
      onClick={() => (open ? menu.current?.hidePopover() : show("first"))}
      onKeyDown={(event) => {
        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          event.preventDefault();
          show(event.key === "ArrowDown" ? "first" : "last");
        }
      }}
    >
      {label}
    </button>
  );

  return (
    <>
      {tooltip ? <Tooltip label={tooltip}>{trigger}</Tooltip> : trigger}
      <div
        ref={menu}
        id={id}
        popover="auto"
        className="r-popover r-popover--menu"
        onToggle={(event) => {
          const isOpen = (event.nativeEvent as ToggleEvent).newState === "open";
          setOpen(isOpen);
          // Closing by Escape or by choosing an item returns focus to the button.
          if (
            !isOpen &&
            (menu.current?.contains(document.activeElement) ||
              document.activeElement === document.body)
          )
            button.current?.focus();
        }}
      >
        <div
          className="r-menu"
          role="menu"
          onKeyDown={(event) => {
            const moves: Record<string, "first" | "last" | 1 | -1> = {
              ArrowDown: 1,
              ArrowUp: -1,
              Home: "first",
              End: "last",
            };
            const move = moves[event.key];
            if (move !== undefined) {
              event.preventDefault();
              focusItem(move);
            } else if (event.key === "Tab") {
              menu.current?.hidePopover();
            } else if (
              event.key.length === 1 &&
              !event.ctrlKey &&
              !event.metaKey &&
              !event.altKey
            ) {
              // Typing a letter jumps to the next item that starts with it.
              const options = [
                ...(menu.current?.querySelectorAll<HTMLButtonElement>(
                  '[role="menuitem"]:not(:disabled)',
                ) ?? []),
              ];
              const from = options.indexOf(document.activeElement as HTMLButtonElement);
              const letter = event.key.toLowerCase();
              for (let i = 1; i <= options.length; i++) {
                const option = options[(from + i) % options.length];
                if (option?.textContent?.trim().toLowerCase().startsWith(letter)) {
                  option.focus();
                  break;
                }
              }
            }
          }}
        >
          {items.map((item, i) =>
            item === "separator" ? (
              <div key={`s${i}`} className="r-menu__separator" role="separator" />
            ) : (
              <button
                key={item.label}
                type="button"
                role="menuitem"
                tabIndex={-1}
                disabled={item.disabled}
                className={`r-menu__item${item.danger ? " r-menu__item--danger" : ""}`}
                onClick={() => {
                  menu.current?.hidePopover();
                  button.current?.focus();
                  item.onSelect();
                }}
              >
                {item.label}
                {item.shortcut && <span className="r-menu__shortcut">{item.shortcut}</span>}
              </button>
            ),
          )}
        </div>
      </div>
    </>
  );
}

// --- popover ---

export interface PopoverButtonProps {
  label: ReactNode;
  children: ReactNode;
  className?: string;
}

/** A button that shows free content (a form, a list) in a popover. */
export function PopoverButton({ label, children, className }: PopoverButtonProps) {
  const button = useRef<HTMLButtonElement>(null);
  const popover = useRef<HTMLDivElement>(null);
  const id = useId();
  const [open, setOpen] = useState(false);
  return (
    <>
      <button
        ref={button}
        type="button"
        className={className ?? "r-button"}
        aria-expanded={open}
        aria-controls={id}
        onClick={() => {
          const el = popover.current;
          if (!el || !button.current) return;
          if (open) el.hidePopover();
          else {
            el.showPopover();
            place(button.current, el);
          }
        }}
      >
        {label}
      </button>
      <div
        ref={popover}
        id={id}
        popover="auto"
        className="r-popover"
        onToggle={(event) => setOpen((event.nativeEvent as ToggleEvent).newState === "open")}
      >
        {children}
      </div>
    </>
  );
}

// --- tooltip ---

export interface TooltipProps {
  /** Names the action. */
  label: string;
  shortcut?: string;
  /** One focusable element, usually an icon button. */
  children: ReactElement<{ "aria-describedby"?: string }>;
}

export function Tooltip({ label, shortcut, children }: TooltipProps) {
  const tip = useRef<HTMLDivElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const id = useId();
  const hide = () => {
    if (timer.current) clearTimeout(timer.current);
    if (tip.current?.matches(":popover-open")) tip.current.hidePopover();
  };
  const show = (anchor: Element, delay: number) => {
    hide();
    timer.current = setTimeout(() => {
      const el = tip.current;
      if (!el) return;
      el.showPopover();
      place(anchor, el);
    }, delay);
  };
  useEffect(() => hide, []);
  return (
    <span
      style={{ display: "contents" }}
      onPointerEnter={(event) => show(event.target as Element, 500)}
      onPointerLeave={hide}
      onFocus={(event) => {
        if ((event.target as Element).matches(":focus-visible")) show(event.target, 0);
      }}
      onBlur={hide}
      onKeyDown={(event) => event.key === "Escape" && hide()}
    >
      {cloneElement(children, { "aria-describedby": id })}
      <div ref={tip} id={id} role="tooltip" popover="manual" className="r-tooltip">
        {label}
        {shortcut && <span className="r-tooltip__shortcut">{shortcut}</span>}
      </div>
    </span>
  );
}
