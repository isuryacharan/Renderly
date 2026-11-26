// Property rows and sections (docs/design-system/components/property-row.md): the one pattern for
// a numeric value in the inspector. A drag on the slider or the label is a begin, previews and
// one commit, so the consumer can make it a single undo step; typing commits once.

import { useRef, type CSSProperties, type ReactNode } from "react";
import { NumericField } from "./controls";
import { IconButton } from "./icons";

export interface PropertyRowProps {
  label: string;
  /** The value, or null when several selected things differ ("mixed"). */
  value: number | null;
  min: number;
  max: number;
  step?: number;
  /** Shown value = value × display, e.g. 100 for a 0–1 value shown in percent. */
  display?: number;
  precision?: number;
  unit?: string;
  defaultValue: number;
  /** A drag started (slider or label). */
  onBegin?: () => void;
  /** During a drag: show this value without recording it. */
  onPreview: (value: number) => void;
  /** Record this value: the end of a drag, a typed value, a step or a reset. */
  onCommit: (value: number) => void;
  /** The drag was cancelled (Escape): put back what was there. */
  onCancel?: () => void;
  disabled?: boolean;
}

/** Label (drag to scrub), slider, numeric field, keyframe slot, reset — on one line. */
export function PropertyRow({
  label,
  value,
  min,
  max,
  step = 0.01,
  display = 1,
  precision = 0,
  unit,
  defaultValue,
  onBegin,
  onPreview,
  onCommit,
  onCancel,
  disabled = false,
}: PropertyRowProps) {
  const drag = useRef<{ last: number } | null>(null);
  const clamp = (n: number) => Math.min(max, Math.max(min, n));
  const shown = value ?? (min + max) / 2;
  const share = max === min ? 0 : ((shown - min) / (max - min)) * 100;

  const begin = (start: number) => {
    drag.current = { last: start };
    onBegin?.();
    // Escape cancels a drag in progress.
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || !drag.current) return;
      event.preventDefault();
      event.stopPropagation();
      drag.current = null;
      onCancel?.();
    };
    addEventListener("keydown", escape, { capture: true });
    return () => removeEventListener("keydown", escape, { capture: true });
  };
  const preview = (next: number) => {
    if (!drag.current) return;
    drag.current.last = next;
    onPreview(next);
  };
  const end = () => {
    const current = drag.current;
    drag.current = null;
    if (current) onCommit(current.last);
  };

  return (
    <div className="r-property-row">
      <span
        className="r-property-row__label"
        title={label}
        onPointerDown={(event) => {
          if (disabled || event.button !== 0) return;
          const target = event.currentTarget;
          target.setPointerCapture(event.pointerId);
          const startX = event.clientX;
          const startValue = shown;
          const stop = begin(startValue);
          // One pixel moves one step, or 1/2000 of the range when steps are finer than that
          // (a 1920 px wide position then moves about 4 px a pixel); Shift is ten times finer.
          const coarse = Math.max(step, (max - min) / 2000);
          const move = (e: PointerEvent) => {
            const perPixel = coarse * (e.shiftKey ? 0.1 : 1);
            const raw = startValue + (e.clientX - startX) * perPixel;
            preview(clamp(Math.round(raw / step) * step));
          };
          const up = () => {
            target.removeEventListener("pointermove", move);
            target.removeEventListener("pointerup", up);
            target.removeEventListener("pointercancel", up);
            stop();
            end();
          };
          target.addEventListener("pointermove", move);
          target.addEventListener("pointerup", up);
          target.addEventListener("pointercancel", up);
        }}
      >
        {label}
      </span>
      <input
        type="range"
        className={`r-slider${value === null ? " r-slider--mixed" : ""}`}
        aria-label={value === null ? `${label}, mixed values` : label}
        min={min}
        max={max}
        step={step}
        value={shown}
        disabled={disabled}
        style={{ "--value": `${share}%` } as CSSProperties}
        onPointerDown={(event) => {
          if (event.button !== 0) return;
          const stop = begin(shown);
          const up = () => {
            removeEventListener("pointerup", up, true);
            removeEventListener("pointercancel", up, true);
            stop();
            end();
          };
          addEventListener("pointerup", up, true);
          addEventListener("pointercancel", up, true);
        }}
        onChange={(event) => {
          const next = Number(event.target.value);
          // A drag previews; a key on the focused slider commits at once.
          if (drag.current) preview(next);
          else onCommit(next);
        }}
        onDoubleClick={() => onCommit(defaultValue)}
      />
      <NumericField
        size="sm"
        label={`${label} value`}
        value={value === null ? null : value * display}
        min={min * display}
        max={max * display}
        step={step * display}
        precision={precision}
        {...(unit ? { unit } : {})}
        onChange={(typed) => onCommit(clamp(typed / display))}
      />
      <button
        type="button"
        className="r-button r-button--icon r-button--sm"
        aria-label={`Keyframes for ${label} (available in a later version)`}
        disabled
      >
        <span className="r-keyframe" />
      </button>
      <IconButton
        icon="reset"
        label={`Reset ${label}`}
        size="sm"
        disabled={disabled || value === defaultValue}
        onClick={() => onCommit(defaultValue)}
      />
    </div>
  );
}

/** A titled group of property rows, with a reset for the whole section. */
export function PropertySection({
  title,
  onReset,
  resetDisabled = false,
  children,
}: {
  title: string;
  onReset?: () => void;
  resetDisabled?: boolean;
  children: ReactNode;
}) {
  return (
    <section className="r-property-section" aria-label={title}>
      <div className="r-property-section__header">
        <h3 className="r-property-section__title">{title}</h3>
        {onReset && (
          <IconButton
            icon="reset"
            label={`Reset ${title.toLowerCase()}`}
            size="sm"
            disabled={resetDisabled}
            onClick={onReset}
          />
        )}
      </div>
      {children}
    </section>
  );
}
