// Inline controls: button, slider, numeric field, tabs, splitter, and toasts.

import {
  createContext,
  useCallback,
  useContext,
  useId,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { Icon } from "./icons";

// --- button ---

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: "secondary" | "primary" | "ghost" | "danger";
  size?: "sm" | "md" | "lg";
}

export function Button({
  variant = "secondary",
  size = "md",
  className,
  type,
  ...rest
}: ButtonProps) {
  const classes = ["r-button"];
  if (variant !== "secondary") classes.push(`r-button--${variant}`);
  if (size !== "md") classes.push(`r-button--${size}`);
  if (className) classes.push(className);
  return <button type={type ?? "button"} className={classes.join(" ")} {...rest} />;
}

// --- slider ---

export interface SliderProps {
  value: number;
  min: number;
  max: number;
  step?: number;
  label: string;
  onChange: (value: number) => void;
}

export function Slider({ value, min, max, step = 1, label, onChange }: SliderProps) {
  const share = max === min ? 0 : ((value - min) / (max - min)) * 100;
  return (
    <input
      type="range"
      className="r-slider"
      aria-label={label}
      min={min}
      max={max}
      step={step}
      value={value}
      style={{ "--value": `${share}%` } as CSSProperties}
      onChange={(event) => onChange(Number(event.target.value))}
    />
  );
}

// --- numeric field ---

export interface NumericFieldProps {
  /** null: several values differ (shown as an en dash). */
  value: number | null;
  onChange: (value: number) => void;
  label: string;
  min?: number;
  max?: number;
  step?: number;
  /** Digits after the point when shown. */
  precision?: number;
  unit?: string;
  size?: "sm" | "md";
}

/**
 * A number typed or stepped: Up and Down step (Shift ×10), Enter or leaving the field commits,
 * Escape puts the value back.
 */
export function NumericField({
  value,
  onChange,
  label,
  min = -Infinity,
  max = Infinity,
  step = 1,
  precision = 0,
  unit,
  size = "md",
}: NumericFieldProps) {
  const [draft, setDraft] = useState<string | null>(null);
  const clamp = (n: number) => Math.min(max, Math.max(min, n));
  const shown = draft ?? (value === null ? "–" : value.toFixed(precision));
  const commit = () => {
    if (draft === null) return;
    const parsed = Number(draft.replace(",", "."));
    if (Number.isFinite(parsed)) onChange(clamp(parsed));
    setDraft(null);
  };
  return (
    <div className={`r-field r-field--number${size === "sm" ? " r-field--sm" : ""}`}>
      <input
        type="text"
        inputMode="decimal"
        aria-label={label}
        value={shown}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={commit}
        onKeyDown={(event) => {
          if (event.key === "Enter") commit();
          else if (event.key === "Escape") setDraft(null);
          else if (event.key === "ArrowUp" || event.key === "ArrowDown") {
            event.preventDefault();
            const delta = (event.key === "ArrowUp" ? step : -step) * (event.shiftKey ? 10 : 1);
            setDraft(null);
            if (value !== null) onChange(clamp(value + delta));
          }
        }}
      />
      {unit && <span className="r-field__unit">{unit}</span>}
    </div>
  );
}

// --- tabs ---

export interface TabsProps {
  label: string;
  tabs: { id: string; label: string }[];
  value: string;
  onChange: (id: string) => void;
}

/** Tab ids for the panels: `tabId(prefix, id)` labels the matching role="tabpanel". */
export function Tabs({ label, tabs, value, onChange }: TabsProps) {
  const prefix = useId();
  const list = useRef<HTMLDivElement>(null);
  return (
    <div
      ref={list}
      className="r-tabs"
      role="tablist"
      aria-label={label}
      onKeyDown={(event) => {
        const index = tabs.findIndex((t) => t.id === value);
        const moves: Record<string, number> = {
          ArrowRight: 1,
          ArrowLeft: -1,
          Home: -index,
          End: tabs.length - 1 - index,
        };
        const move = moves[event.key];
        if (move === undefined) return;
        event.preventDefault();
        const next = tabs[(index + move + tabs.length) % tabs.length];
        if (!next) return;
        onChange(next.id);
        list.current?.querySelector<HTMLButtonElement>(`[data-tab="${next.id}"]`)?.focus();
      }}
    >
      {tabs.map((tab) => (
        <button
          key={tab.id}
          type="button"
          role="tab"
          data-tab={tab.id}
          id={`${prefix}-${tab.id}`}
          aria-selected={tab.id === value}
          tabIndex={tab.id === value ? 0 : -1}
          onClick={() => onChange(tab.id)}
        >
          {tab.label}
        </button>
      ))}
    </div>
  );
}

// --- splitter ---

export interface SplitterProps {
  /** "vertical": a line between columns that changes a width; "horizontal": between rows. */
  orientation: "vertical" | "horizontal";
  /** The size it controls, in pixels. */
  value: number;
  min: number;
  max: number;
  /** True when dragging right (or down) makes the controlled size smaller. */
  invert?: boolean;
  label: string;
  onChange: (value: number) => void;
}

/** A resize handle. Arrow keys move it 16 px (Shift: 64), Home and End go to the limits. */
export function Splitter({
  orientation,
  value,
  min,
  max,
  invert = false,
  label,
  onChange,
}: SplitterProps) {
  const start = useRef<{ pointer: number; value: number } | null>(null);
  const [dragging, setDragging] = useState(false);
  const clamp = useCallback((n: number) => Math.round(Math.min(max, Math.max(min, n))), [min, max]);
  const axis = (event: ReactPointerEvent) =>
    orientation === "vertical" ? event.clientX : event.clientY;
  const sign = invert ? -1 : 1;
  return (
    <div
      role="separator"
      tabIndex={0}
      aria-orientation={orientation}
      aria-label={label}
      aria-valuenow={value}
      aria-valuemin={min}
      aria-valuemax={max}
      data-dragging={dragging ? "" : undefined}
      className={`r-splitter r-splitter--${orientation}`}
      onPointerDown={(event) => {
        event.currentTarget.setPointerCapture(event.pointerId);
        start.current = { pointer: axis(event), value };
        setDragging(true);
      }}
      onPointerMove={(event) => {
        if (!start.current) return;
        onChange(clamp(start.current.value + sign * (axis(event) - start.current.pointer)));
      }}
      onPointerUp={() => {
        start.current = null;
        setDragging(false);
      }}
      onKeyDown={(event) => {
        const grow = orientation === "vertical" ? "ArrowRight" : "ArrowDown";
        const shrink = orientation === "vertical" ? "ArrowLeft" : "ArrowUp";
        const amount = event.shiftKey ? 64 : 16;
        let next: number | null = null;
        if (event.key === grow) next = value + sign * amount;
        else if (event.key === shrink) next = value - sign * amount;
        else if (event.key === "Home") next = min;
        else if (event.key === "End") next = max;
        if (next === null) return;
        event.preventDefault();
        onChange(clamp(next));
      }}
    />
  );
}

// --- toasts ---

export interface ToastOptions {
  kind?: "info" | "danger";
  action?: { label: string; onSelect: () => void };
}

interface Toast extends ToastOptions {
  id: number;
  message: string;
}

const ToastContext = createContext<(message: string, options?: ToastOptions) => void>(
  () => undefined,
);

/** Shows a short message about what just happened: `useToast()("Project deleted", { action })`. */
export const useToast = () => useContext(ToastContext);

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const next = useRef(0);
  const dismiss = useCallback(
    (id: number) => setToasts((all) => all.filter((t) => t.id !== id)),
    [],
  );
  const show = useCallback(
    (message: string, options: ToastOptions = {}) => {
      const id = next.current++;
      setToasts((all) => [...all.slice(-2), { id, message, ...options }]);
      setTimeout(() => dismiss(id), options.action ? 8000 : 4000);
    },
    [dismiss],
  );
  return (
    <ToastContext.Provider value={show}>
      {children}
      <div className="r-toasts" role="status" aria-live="polite">
        {toasts.map((toast) => (
          <div
            key={toast.id}
            className={`r-toast${toast.kind === "danger" ? " r-toast--danger" : ""}`}
          >
            {toast.kind === "danger" && (
              <span className="r-toast__kind">
                <Icon name="error" />
                Error
              </span>
            )}
            <span>{toast.message}</span>
            {toast.action && (
              <button
                type="button"
                className="r-button r-button--ghost r-button--sm"
                onClick={() => {
                  dismiss(toast.id);
                  toast.action?.onSelect();
                }}
              >
                {toast.action.label}
              </button>
            )}
          </div>
        ))}
      </div>
    </ToastContext.Provider>
  );
}
