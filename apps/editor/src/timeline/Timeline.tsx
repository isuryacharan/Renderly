// The timeline panel: a toolbar, track headers (React, so they are real buttons for keyboard and
// screen readers) and the canvas track area (TimelineView).

import { type Project, type Track, type ViewStore } from "@renderly/core";
import type { ProjectSession } from "@renderly/storage";
import { Icon, IconButton, useToast, type IconName } from "@renderly/ui/react";
import { ActionButton, ActionSegment, useActions } from "../actions/react";
import { useEffect, useReducer, useRef, useState } from "react";
import type { MediaStatus } from "../editor/media-status";
import { layoutRows, RULER_HEIGHT } from "./geometry";
import { TimelineView } from "./view";

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
  const actions = useActions();
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
      onChange: () => {
        refresh();
        actions.changed();
      },
    });
    setTimeline(created);
    actions.attach({ timeline: created });
    if (import.meta.env.DEV) window.renderlyTimeline = created;
    return () => {
      actions.attach({ timeline: null });
      created.destroy();
      if (window.renderlyTimeline === created) delete window.renderlyTimeline;
    };
  }, [session, view, status, actions]);

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

  const scrollY = timeline?.scrollY ?? 0;
  const empty = rows.length === 0;

  return (
    <section className="r-panel timeline" aria-label="Timeline">
      <div className="r-panel__header timeline__toolbar">
        <h2 className="text-body-strong timeline__title">Timeline</h2>
        <ActionSegment ids={["timeline.selectTool", "timeline.bladeTool"]} label="Tool" />
        <ActionButton id="timeline.split" />
        <ActionButton id="timeline.delete" />
        <ActionButton id="timeline.snapping" />
        <span className="r-toolbar__spacer" />
        <ActionButton id="timeline.zoomOut" />
        <ActionButton id="timeline.zoomIn" />
        <ActionButton id="timeline.fit" />
      </div>
      <div className="timeline__body">
        <div className="timeline__heads">
          <div className="timeline__corner" style={{ height: RULER_HEIGHT }}>
            <ActionButton id="timeline.addVideoTrack" />
            <ActionButton id="timeline.addAudioTrack" />
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
