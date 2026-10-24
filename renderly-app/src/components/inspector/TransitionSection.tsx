import { useState } from "react";
import { setClipTransition } from "../../lib/commands";
import {
  TRANSITION_EASING_MENU,
  effectLabel,
  findEntry,
  paramValue,
  transitionEasingFromMenu,
  transitionEasingMenuValue,
} from "../../lib/effects";
import {
  MIN_TRANSITION_US,
  maxTransitionUs,
  nextVideoClip,
  transitionDurationFor,
  transitionEntries,
} from "../../lib/transitions";
import { secsToUs, usToSecs } from "../../lib/time";
import type { TransitionLayer } from "../../timeline/layout";
import type { EffectParamValue, Track } from "../../lib/types";
import { useCatalogue, useEffectStatus } from "../../lib/useCatalogue";
import { useEditorStore } from "../../store/editorStore";
import { MenuSelect } from "../ui/MenuSelect";
import { statusBadge } from "./EffectStack";
import { ParamRow } from "./ParamRow";

/// Outgoing transition of a video clip: picker (by ref), duration, easing and the transition's
/// own parameters. Every change is one `SetClipTransition`.
export function TransitionSection({ track, clip }: { track: Track; clip: TransitionLayer }) {
  const dispatch = useEditorStore((s) => s.dispatch);
  const catalogue = useCatalogue();
  const statuses = useEffectStatus(clip);
  const current = clip.outgoing_transition ?? null;
  const next = nextVideoClip(track, clip);
  const maxDur = maxTransitionUs(track, clip);
  const entries = transitionEntries(catalogue);
  const entry = current ? findEntry(catalogue, current.ref) : undefined;
  const locked = track.locked;
  // A slider drag keeps its value here until release (one command, one undo step).
  const [draft, setDraft] = useState<Record<string, EffectParamValue>>({});

  if (!next) {
    return (
      <p className="empty-hint">
        {current
          ? "The transition needs a following clip on this track."
          : "No following clip - add one after this clip to set a transition."}
      </p>
    );
  }

  const options = [
    { value: "", label: "None" },
    ...entries.map((e) => ({ value: e.ref, label: e.label })),
  ];
  if (current && !options.some((o) => o.value === current.ref)) {
    options.push({ value: current.ref, label: effectLabel(catalogue, current.ref) });
  }
  const easingValue = transitionEasingMenuValue(current?.easing);
  const easingOptions = [
    ...TRANSITION_EASING_MENU,
    ...(easingValue === "custom" ? [{ value: "custom" as const, label: "Custom" }] : []),
  ];
  const badge = current
    ? statusBadge(
        statuses.get("transition") ?? (entry ? undefined : "missing"),
        effectLabel(catalogue, current.ref),
      )
    : null;

  const commitParam = (key: string, v: EffectParamValue) => {
    setDraft(({ [key]: _dropped, ...rest }) => rest);
    if (!current) return;
    void dispatch(
      setClipTransition(track.id, clip.id, current.ref, {
        params: { ...current.params, [key]: v },
      }),
      false,
      { mergeKey: `transition:${clip.id}` },
    );
  };

  return (
    <div className="transition-section">
      <label className="insp-cell">
        <span>Transition</span>
        <MenuSelect
          value={current?.ref ?? ""}
          disabled={locked}
          options={options}
          onChange={(ref) => {
            if (ref === "") void dispatch(setClipTransition(track.id, clip.id, null));
            else
              void dispatch(
                setClipTransition(track.id, clip.id, ref, {
                  durationUs: transitionDurationFor(track, clip),
                }),
              );
          }}
        />
      </label>
      {badge && (
        <p className="effect-badge missing" role="status">
          {badge}
        </p>
      )}
      {current && (
        <>
          <label className="insp-cell">
            <span>Duration (seconds)</span>
            <input
              type="number"
              step="0.05"
              min={usToSecs(MIN_TRANSITION_US)}
              max={usToSecs(maxDur)}
              value={usToSecs(current.duration_us)}
              disabled={locked}
              onChange={(e) => {
                const durationUs = Math.min(
                  maxDur,
                  Math.max(MIN_TRANSITION_US, secsToUs(parseFloat(e.target.value) || 0)),
                );
                void dispatch(setClipTransition(track.id, clip.id, current.ref, { durationUs }), false, {
                  mergeKey: `transition:${clip.id}`,
                });
              }}
            />
          </label>
          <label className="insp-cell">
            <span>Easing</span>
            <MenuSelect
              value={easingValue}
              disabled={locked}
              options={easingOptions}
              onChange={(v) => {
                if (v === "custom") return;
                void dispatch(
                  setClipTransition(track.id, clip.id, current.ref, {
                    easing: transitionEasingFromMenu(v as Exclude<typeof easingValue, "custom">),
                  }),
                );
              }}
            />
          </label>
          {entry?.params.map((spec) => (
            <ParamRow
              key={spec.key}
              spec={spec}
              value={draft[spec.key] ?? paramValue(current, spec)}
              disabled={locked}
              animated={false}
              canKeyframe={false}
              lutChoices={catalogue.filter((c) => c.kind === "lut")}
              onPreview={(v) => setDraft((d) => ({ ...d, [spec.key]: v }))}
              onCommit={(v) => commitParam(spec.key, v)}
              onKeyframe={() => {}}
            />
          ))}
        </>
      )}
    </div>
  );
}
