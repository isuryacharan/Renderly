// The editor's four panels: library, viewer and inspector above, the timeline below. The three
// splitters resize them, by pointer or keyboard, and the sizes are kept across reloads.

import type { Project, ViewStore } from "@renderly/core";
import type { ProjectSession } from "@renderly/storage";
import { Splitter } from "@renderly/ui/react";
import { useEffect, useState } from "react";
import { Timeline } from "../timeline/Timeline";
import { Viewer } from "../viewer/Viewer";
import { MediaBin } from "./MediaBin";
import type { MediaStatus } from "./media-status";

interface Sizes {
  library: number;
  inspector: number;
  timeline: number;
}

const KEY = "renderly:layout:v1";
const DEFAULTS: Sizes = { library: 280, inspector: 320, timeline: 280 };
const LIMITS = { library: [160, 640], inspector: [200, 640], timeline: [120, 720] } as const;

function loadSizes(): Sizes {
  try {
    const saved = JSON.parse(localStorage.getItem(KEY) ?? "null") as Partial<Sizes> | null;
    const sizes = { ...DEFAULTS };
    for (const key of Object.keys(DEFAULTS) as (keyof Sizes)[]) {
      const value = saved?.[key];
      if (typeof value === "number" && Number.isFinite(value)) sizes[key] = value;
    }
    return sizes;
  } catch {
    return DEFAULTS;
  }
}

export function Layout({
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
  const [sizes, setSizes] = useState(loadSizes);
  useEffect(() => {
    try {
      localStorage.setItem(KEY, JSON.stringify(sizes));
    } catch {
      // private mode: sizes last for this page only
    }
  }, [sizes]);
  const set = (key: keyof Sizes) => (value: number) => setSizes((s) => ({ ...s, [key]: value }));

  return (
    <div className="editor">
      <div className="editor__top">
        <section className="r-panel" style={{ width: sizes.library }} aria-label="Library">
          <h2 className="r-panel__header text-body-strong">Library</h2>
          <div className="r-panel__body">
            <MediaBin session={session} project={project} readOnly={readOnly} status={status} />
          </div>
        </section>
        <Splitter
          orientation="vertical"
          label="Resize the library"
          value={sizes.library}
          min={LIMITS.library[0]}
          max={LIMITS.library[1]}
          onChange={set("library")}
        />
        <Viewer store={session.store} project={project} view={view} status={status} />
        <Splitter
          orientation="vertical"
          label="Resize the inspector"
          value={sizes.inspector}
          min={LIMITS.inspector[0]}
          max={LIMITS.inspector[1]}
          invert
          onChange={set("inspector")}
        />
        <section className="r-panel" style={{ width: sizes.inspector }} aria-label="Inspector">
          <h2 className="r-panel__header text-body-strong">Inspector</h2>
          <div className="r-panel__body">
            <ProjectProperties session={session} project={project} readOnly={readOnly} />
          </div>
        </section>
      </div>
      <Splitter
        orientation="horizontal"
        label="Resize the timeline"
        value={sizes.timeline}
        min={LIMITS.timeline[0]}
        max={LIMITS.timeline[1]}
        invert
        onChange={set("timeline")}
      />
      <div className="editor__timeline" style={{ height: sizes.timeline }}>
        <Timeline
          session={session}
          project={project}
          readOnly={readOnly}
          view={view}
          status={status}
        />
      </div>
    </div>
  );
}

/** The project's own properties. The name is changed through the rename command. */
function ProjectProperties({
  session,
  project,
  readOnly,
}: {
  session: ProjectSession;
  project: Project;
  readOnly: boolean;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const commit = () => {
    const name = draft?.trim();
    setDraft(null);
    if (name && name !== project.name)
      session.store.dispatch({ type: "project.rename", payload: { name } });
  };
  const main = project.sequences[0];
  return (
    <div className="r-form">
      <label className="r-label" htmlFor="project-name">
        Name
      </label>
      <div className={`r-field${readOnly ? " r-field--disabled" : ""}`}>
        <input
          id="project-name"
          value={draft ?? project.name}
          disabled={readOnly}
          onChange={(event) => setDraft(event.target.value)}
          onBlur={commit}
          onKeyDown={(event) => {
            if (event.key === "Enter") commit();
            if (event.key === "Escape") setDraft(null);
          }}
        />
      </div>
      {main && (
        <>
          <span className="r-label">Sequence</span>
          <span className="r-tabular editor__fact">
            {main.width} × {main.height} · {main.fps[0] / main.fps[1]} fps
          </span>
        </>
      )}
    </div>
  );
}
