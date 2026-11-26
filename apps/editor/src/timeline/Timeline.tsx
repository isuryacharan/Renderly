// The timeline panel: a toolbar, track headers (React, so they are real buttons for keyboard and
// screen readers) and the canvas track area (TimelineView).

import { createId, type Project, type Track, type ViewStore } from "@renderly/core";
import type { ProjectSession } from "@renderly/storage";
import { Icon, IconButton, Tooltip, useToast, type IconName } from "@renderly/ui/react";
import { useEffect, useReducer, useRef, useState } from "react";
import type { MediaStatus } from "../editor/media-status";
import { layoutRows, RULER_HEIGHT } from "./geometry";
import { TimelineView, type Tool } from "./view";

declare global {
  interface Window {
    /** Dev builds only: the timeline, for the end-to-end checks and the benchmark. */
    renderlyTimeline?: TimelineView;
  }
}

const KIND_LETTER: Record<Track["kind"], string> = {
  video: "V",
  audio: "A",
  text: "T",
  effect: "E",
};
const KIND_ICON: Record<Track["kind"], IconName> = {
  video: "video",
  audio: "audio",
  text: "text",
  effect: "effect",
};

export function Timeline({
  session,
  project,
  readOnly,
  view,
  status,
}: {
  session: ProjectSession;
  project: Project;
  readOnly: boolean;
  view: ViewStore;
  status: MediaStatus;
}) {
  const host = useRef<HTMLDivElement>(null);
  const [timeline, setTimeline] = useState<TimelineView | null>(null);
  const [, refresh] = useReducer((n: number) => n + 1, 0);
  const toast = useToast();
  const latest = useRef({ readOnly, toast });
  latest.current = { readOnly, toast };

  useEffect(() => {
    if (!host.current) return;
    const created = new TimelineView(host.current, {
      store: session.store,
      view,
      status,
      readOnly: () => latest.current.readOnly,
      onError: (message) => latest.current.toast(message, { kind: "danger" }),
      onChange: refresh,
    });
    setTimeline(created);
    if (import.meta.env.DEV) window.renderlyTimeline = created;
    return () => {
      created.destroy();
      if (window.renderlyTimeline === created) delete window.renderlyTimeline;
    };
  }, [session, view, status]);

  const sequence =
    project.sequences.find((s) => s.id === view.get().sequence) ?? project.sequences[0];
  const rows = sequence ? layoutRows(sequence) : [];
  const counts = new Map<string, number>();
  const labels = new Map<string, string>();
  for (const track of sequence?.tracks ?? []) {
    const n = (counts.get(track.kind) ?? 0) + 1;
    counts.set(track.kind, n);
    labels.set(track.id, `${KIND_LETTER[track.kind]}${n}`);
  }

  const setFlags = (track: Track, flags: Partial<Pick<Track, "locked" | "hidden" | "muted">>) => {
    const result = session.store.dispatch({
      type: "track.set",
      payload: { tracks: [track.id], ...flags },
    });
    if (!result.ok) toast(result.error.message, { kind: "danger" });
  };

  const addTrack = (kind: Track["kind"]) => {
    if (!sequence) return;
    const result = session.store.dispatch({
      type: "track.add",
      payload: { sequence: sequence.id, id: createId(), kind },
    });
    if (!result.ok) toast(result.error.message, { kind: "danger" });
  };

  const tool: Tool = timeline?.tool ?? "select";
  const snapping = timeline?.snapping ?? true;
  const scrollY = timeline?.scrollY ?? 0;
  const empty = rows.length === 0;

  return (
    <section className="r-panel timeline" aria-label="Timeline">
      <div className="r-panel__header timeline__toolbar">
        <h2 className="text-body-strong timeline__title">Timeline</h2>
        <div className="r-segmented r-segmented--icons" role="group" aria-label="Tool">
          {(
            [
              ["select", "select-tool", "Selection tool", "V"],
              ["blade", "blade-tool", "Blade tool", "B"],
            ] as const
          ).map(([id, icon, label, key]) => (
            <Tooltip key={id} label={label} shortcut={key}>
              <button
                type="button"
                aria-label={label}
                aria-pressed={tool === id}
                onClick={() => timeline?.setTool(id)}
              >
                <Icon name={icon} />
              </button>
            </Tooltip>
          ))}
        </div>
        <IconButton
          icon="split"
          label="Split at the playhead"
          shortcut="S"
          size="sm"
          disabled={readOnly || empty}
          onClick={() => timeline?.splitAtPlayhead()}
        />
        <IconButton
          icon="delete"
          label="Delete the selected clips"
          shortcut="Delete"
          size="sm"
          disabled={readOnly || empty}
          onClick={() => timeline?.deleteSelected(false)}
        />
        <IconButton
          icon="magnetic"
          label="Snapping"
          size="sm"
          aria-pressed={snapping}
          onClick={() => timeline?.setSnapping(!snapping)}
        />
        <span className="r-toolbar__spacer" />
        <IconButton
          icon="zoom-out"
          label="Zoom out"
          shortcut="−"
          size="sm"
          onClick={() => timeline?.zoomBy(1.5)}
        />
        <IconButton
          icon="zoom-in"
          label="Zoom in"
          shortcut="="
          size="sm"
          onClick={() => timeline?.zoomBy(1 / 1.5)}
        />
        <IconButton
          icon="zoom-fit"
          label="Zoom to fit"
          shortcut="Shift Z"
          size="sm"
          onClick={() => timeline?.fit()}
        />
      </div>
      <div className="timeline__body">
        <div className="timeline__heads">
          <div className="timeline__corner" style={{ height: RULER_HEIGHT }}>
            <IconButton
              icon="video"
              label="Add a video track"
              size="sm"
              disabled={readOnly}
              onClick={() => addTrack("video")}
            />
            <IconButton
              icon="audio"
              label="Add an audio track"
              size="sm"
              disabled={readOnly}
              onClick={() => addTrack("audio")}
            />
          </div>
          <div
            className="timeline__heads-scroll"
            onWheel={(event) => timeline?.scrollRowsBy(event.deltaY)}
          >
            <ul
              className="timeline__heads-list"
              aria-label="Tracks"
              style={{ transform: `translateY(${-scrollY}px)` }}
            >
              {rows.map(({ track, height }) => {
                const label = labels.get(track.id) ?? "";
                const name = track.name || label;
                return (
                  <li
                    key={track.id}
                    className={`timeline__head${track.hidden ? " timeline__head--hidden" : ""}`}
                    style={{ height }}
                  >
                    <span className="timeline__head-name text-caption" title={name}>
                      <Icon name={KIND_ICON[track.kind]} />
                      {name}
                    </span>
                    <span className="timeline__head-buttons">
                      <IconButtonPressed
                        icon={track.locked ? "locked" : "unlocked"}
                        label={track.locked ? `Unlock ${name}` : `Lock ${name}`}
                        pressed={track.locked}
                        disabled={readOnly}
                        onClick={() => setFlags(track, { locked: !track.locked })}
                      />
                      {track.kind !== "audio" && (
                        <IconButtonPressed
                          icon={track.hidden ? "hidden" : "visible"}
                          label={track.hidden ? `Show ${name}` : `Hide ${name}`}
                          pressed={track.hidden}
                          disabled={readOnly}
                          onClick={() => setFlags(track, { hidden: !track.hidden })}
                        />
                      )}
                      {(track.kind === "audio" || track.kind === "video") && (
                        <IconButtonPressed
                          icon={track.muted ? "muted" : "volume"}
                          label={track.muted ? `Unmute ${name}` : `Mute ${name}`}
                          pressed={track.muted}
                          disabled={readOnly}
                          onClick={() => setFlags(track, { muted: !track.muted })}
                        />
                      )}
                    </span>
                  </li>
                );
              })}
            </ul>
          </div>
        </div>
        <div className="timeline__tracks" ref={host}>
          {empty && (
            <p className="timeline__empty">
              {readOnly ? "The timeline is empty." : "Drag media here from the library."}
            </p>
          )}
        </div>
      </div>
    </section>
  );
}

function IconButtonPressed({
  pressed,
  ...rest
}: {
  icon: IconName;
  label: string;
  shortcut?: string;
  pressed: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  return <IconButton size="sm" aria-pressed={pressed} {...rest} />;
}
