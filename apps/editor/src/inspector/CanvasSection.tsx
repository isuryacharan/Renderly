// The sequence's canvas: aspect ratio and size, frame rate, and what fills the frame behind the
// clips. Every change is one `sequence.set` command; the blur amount's drag is one gesture.
// Clips are placed as fractions of the frame from its centre, so a new size keeps them centred.

import {
  SUPPORTED_RATES,
  type BackgroundSettings,
  type Gesture,
  type ProjectStore,
  type Sequence,
} from "@renderly/core";
import {
  Icon,
  NumericField,
  PropertyChoice,
  PropertyRow,
  PropertySection,
  useToast,
} from "@renderly/ui/react";
import { useRef } from "react";

/** Aspect ratio presets, as width : height. */
const RATIOS: { label: string; ratio: [number, number] }[] = [
  { label: "16:9 landscape", ratio: [16, 9] },
  { label: "9:16 portrait", ratio: [9, 16] },
  { label: "1:1 square", ratio: [1, 1] },
  { label: "4:5 portrait", ratio: [4, 5] },
  { label: "4:3", ratio: [4, 3] },
  { label: "21:9 wide", ratio: [21, 9] },
];

const even = (n: number) => Math.min(16384, Math.max(16, 2 * Math.round(n / 2)));

/**
 * The size for `ratio` that keeps the current shorter side: a 1080p frame becomes 1080 × 1920
 * portrait or 1080 × 1080 square, and a 4K frame keeps 2160.
 */
export function sizeForRatio(
  current: { width: number; height: number },
  [w, h]: readonly [number, number],
): { width: number; height: number } {
  const short = Math.min(current.width, current.height);
  return w >= h
    ? { width: even((short * w) / h), height: even(short) }
    : { width: even(short), height: even((short * h) / w) };
}

/** The preset matching a size exactly, if any. */
function ratioOf(sequence: Sequence): number {
  return RATIOS.findIndex(({ ratio: [w, h] }) => sequence.width * h === sequence.height * w);
}

const rateLabel = ([num, den]: readonly [number, number]) =>
  den === 1 ? `${num} fps` : `${(num / den).toFixed(2)} fps`;

export function CanvasSection({
  store,
  sequence,
  readOnly,
}: {
  store: ProjectStore;
  sequence: Sequence;
  readOnly: boolean;
}) {
  const toast = useToast();
  const gesture = useRef<Gesture | null>(null);
  const set = (fields: Record<string, unknown>) => {
    const result = store.dispatch({
      type: "sequence.set",
      payload: { sequence: sequence.id, ...fields },
    });
    if (!result.ok) toast(result.error.message, { kind: "danger" });
  };
  const background = sequence.background;
  const setBackground = (fields: Partial<BackgroundSettings>) => set({ background: fields });
  const ratio = ratioOf(sequence);
  const rateIndex = SUPPORTED_RATES.findIndex(
    ([n, d]) => n === sequence.fps[0] && d === sequence.fps[1],
  );
  const disabled = readOnly ? " r-field--disabled" : "";

  return (
    <PropertySection title="Canvas">
      <PropertyChoice label="Ratio" id="canvas-ratio">
        <div className={`r-field r-field--sm${disabled}`}>
          <select
            id="canvas-ratio"
            value={ratio}
            disabled={readOnly}
            onChange={(event) => {
              const preset = RATIOS[Number(event.target.value)];
              if (preset) set(sizeForRatio(sequence, preset.ratio));
            }}
          >
            {ratio < 0 && <option value={-1}>Custom</option>}
            {RATIOS.map(({ label }, i) => (
              <option key={label} value={i}>
                {label}
              </option>
            ))}
          </select>
          <Icon name="chevron-down" />
        </div>
      </PropertyChoice>
      <PropertyChoice label="Size">
        <div className="inspector__size">
          <NumericField
            label="Canvas width"
            size="sm"
            value={sequence.width}
            min={16}
            max={16384}
            step={2}
            unit="px"
            onChange={(width) => set({ width: even(width) })}
          />
          <span className="inspector__times" aria-hidden="true">
            ×
          </span>
          <NumericField
            label="Canvas height"
            size="sm"
            value={sequence.height}
            min={16}
            max={16384}
            step={2}
            unit="px"
            onChange={(height) => set({ height: even(height) })}
          />
        </div>
      </PropertyChoice>
      <PropertyChoice label="Frame rate" id="canvas-rate">
        <div className={`r-field r-field--sm${disabled}`}>
          <select
            id="canvas-rate"
            value={rateIndex}
            disabled={readOnly}
            onChange={(event) => {
              const rate = SUPPORTED_RATES[Number(event.target.value)];
              if (rate) set({ fps: [rate[0], rate[1]] });
            }}
          >
            {SUPPORTED_RATES.map((rate, i) => (
              <option key={rateLabel(rate)} value={i}>
                {rateLabel(rate)}
              </option>
            ))}
          </select>
          <Icon name="chevron-down" />
        </div>
      </PropertyChoice>
      <PropertyChoice label="Background">
        <div className="r-segmented r-segmented--sm" role="group" aria-label="Background">
          {(
            [
              ["color", "Colour"],
              ["blur", "Blur"],
            ] as const
          ).map(([kind, label]) => (
            <button
              key={kind}
              type="button"
              aria-pressed={background.kind === kind}
              disabled={readOnly}
              onClick={() => setBackground({ kind })}
            >
              {label}
            </button>
          ))}
        </div>
      </PropertyChoice>
      <PropertyChoice label="Colour" id="canvas-colour">
        <div className={`r-field r-field--sm inspector__colour${disabled}`}>
          <input
            id="canvas-colour"
            type="color"
            value={background.color}
            disabled={readOnly}
            onChange={(event) => setBackground({ color: event.target.value.toLowerCase() })}
          />
          <span className="r-tabular">{background.color}</span>
        </div>
      </PropertyChoice>
      {background.kind === "blur" && (
        <PropertyRow
          label="Blur"
          value={background.blur}
          min={0}
          max={1}
          step={0.01}
          display={100}
          unit="%"
          defaultValue={0.5}
          disabled={readOnly}
          onBegin={() => {
            try {
              gesture.current = store.beginGesture("canvas:blur", "sequence.set");
            } catch (error) {
              toast(error instanceof Error ? error.message : String(error), { kind: "danger" });
            }
          }}
          onPreview={(blur) =>
            gesture.current?.preview({ sequence: sequence.id, background: { blur } })
          }
          onCommit={(blur) => {
            const running = gesture.current;
            gesture.current = null;
            if (running) running.commit({ sequence: sequence.id, background: { blur } });
            else setBackground({ blur });
          }}
          onCancel={() => {
            gesture.current?.cancel();
            gesture.current = null;
          }}
        />
      )}
    </PropertySection>
  );
}
