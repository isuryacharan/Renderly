import { Diamond } from "lucide-react";
import type { CatalogueEntry, CatalogueParam } from "../../lib/ipc";
import type { EffectParamValue } from "../../lib/types";
import { colorToHex, formatParam, hexToColor, paramStep, prettyName } from "../../lib/paramMath";
import { MenuSelect } from "../ui/MenuSelect";

/**
 * One generic parameter control, driven by the catalogue's param spec: a slider for float/int,
 * a checkbox for bool, a menu for choice (and LUT refs), a colour picker for colour, two number
 * fields for vec2. `onPreview` fires on every live change (drag), `onCommit` on release.
 */
export function ParamRow({
  spec,
  value,
  disabled,
  animated,
  canKeyframe,
  onPreview,
  onCommit,
  onKeyframe,
  lutChoices,
}: {
  spec: CatalogueParam;
  value: EffectParamValue;
  disabled: boolean;
  /** The param has keyframes; the slider shows the static base value. */
  animated: boolean;
  canKeyframe: boolean;
  onPreview(v: EffectParamValue): void;
  onCommit(v: EffectParamValue): void;
  onKeyframe(): void;
  lutChoices?: CatalogueEntry[];
}) {
  const label = prettyName(spec.key);
  const key = canKeyframe ? (
    <button
      type="button"
      className={`param-key${animated ? " on" : ""}`}
      disabled={disabled}
      title={animated ? "Animated - add a keyframe at the playhead" : "Add a keyframe at the playhead"}
      aria-label={`Keyframe ${label}`}
      onClick={onKeyframe}
    >
      <Diamond size={11} strokeWidth={1.75} />
    </button>
  ) : null;

  switch (spec.type) {
    case "bool":
      return (
        <div className="field param-row" data-param={spec.key}>
          <label className="toggle-row">
            <input
              type="checkbox"
              checked={value === true || value === 1}
              disabled={disabled}
              onChange={(e) => onCommit(e.target.checked)}
            />
            <span>{label}</span>
          </label>
        </div>
      );
    case "choice": {
      const options = (spec.options ?? []).map((id, i) => ({
        value: String(i),
        label: prettyName(id),
      }));
      return (
        <div className="field param-row" data-param={spec.key}>
          <label>{label}</label>
          <MenuSelect
            value={String(Math.round(Number(value) || 0))}
            disabled={disabled}
            options={options}
            onChange={(v) => onCommit(parseInt(v, 10))}
          />
        </div>
      );
    }
    case "lut": {
      const current = typeof value === "string" ? value : "";
      const options = [
        { value: "", label: "None" },
        ...(lutChoices ?? []).map((l) => ({ value: l.ref, label: l.label })),
      ];
      if (current && !options.some((o) => o.value === current)) {
        options.push({ value: current, label: current });
      }
      return (
        <div className="field param-row" data-param={spec.key}>
          <label>{label}</label>
          <MenuSelect value={current} disabled={disabled} options={options} onChange={onCommit} />
        </div>
      );
    }
    case "color": {
      const c = Array.isArray(value) ? value : [0, 0, 0, 1];
      const alpha = c[3] ?? 1;
      return (
        <div className="field param-row" data-param={spec.key}>
          <label>{label}</label>
          <input
            type="color"
            aria-label={label}
            value={colorToHex(c)}
            disabled={disabled}
            onInput={(e) => onPreview(hexToColor(e.currentTarget.value, alpha))}
            onChange={(e) => onCommit(hexToColor(e.target.value, alpha))}
          />
          <input
            type="range"
            aria-label={`${label} alpha`}
            min={0}
            max={1}
            step={0.01}
            value={alpha}
            disabled={disabled}
            onChange={(e) =>
              onPreview([c[0] ?? 0, c[1] ?? 0, c[2] ?? 0, parseFloat(e.target.value)])
            }
            onMouseUp={(e) =>
              onCommit([c[0] ?? 0, c[1] ?? 0, c[2] ?? 0, parseFloat(e.currentTarget.value)])
            }
            onKeyUp={(e) =>
              onCommit([c[0] ?? 0, c[1] ?? 0, c[2] ?? 0, parseFloat(e.currentTarget.value)])
            }
          />
          {key}
        </div>
      );
    }
    case "vec2": {
      const v = Array.isArray(value) ? value : [0, 0];
      const set = (i: 0 | 1, n: number) => (i === 0 ? [n, v[1] ?? 0] : [v[0] ?? 0, n]);
      const step = paramStep({ type: "float", min: spec.min, max: spec.max });
      return (
        <div className="field param-row" data-param={spec.key}>
          <label>{label}</label>
          {([0, 1] as const).map((i) => (
            <input
              key={i}
              type="number"
              aria-label={`${label} ${i === 0 ? "X" : "Y"}`}
              min={spec.min}
              max={spec.max}
              step={step}
              value={v[i] ?? 0}
              disabled={disabled}
              onChange={(e) => onPreview(set(i, parseFloat(e.target.value) || 0))}
              onBlur={(e) => onCommit(set(i, parseFloat(e.target.value) || 0))}
            />
          ))}
          {key}
        </div>
      );
    }
    default: {
      // float / int
      const step = paramStep(spec);
      const n = typeof value === "number" ? value : Number(value) || 0;
      const commit = (el: HTMLInputElement) => onCommit(parseFloat(el.value));
      return (
        <div className="field param-row" data-param={spec.key}>
          <label>{label}</label>
          <input
            type="range"
            aria-label={label}
            min={spec.min ?? 0}
            max={spec.max ?? 1}
            step={step}
            value={n}
            disabled={disabled}
            onChange={(e) => onPreview(parseFloat(e.target.value))}
            onMouseUp={(e) => commit(e.currentTarget)}
            onTouchEnd={(e) => commit(e.currentTarget)}
            onKeyUp={(e) => commit(e.currentTarget)}
          />
          <span title={animated ? "Base value; keyframes animate it" : undefined}>
            {formatParam(n, step)}
          </span>
          {key}
        </div>
      );
    }
  }
}
