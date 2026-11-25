// The editor route: opens the project in a session (writer or read-only follower), shows its save
// status, and lays out the four panels. The panels are empty frames until M2 fills them.

import type { Project } from "@renderly/core";
import { ProjectSession, subdir, type Role, type SaveEvent } from "@renderly/storage";
import { Button, Icon, IconButton, useToast, type IconName } from "@renderly/ui/react";
import { useEffect, useState, useSyncExternalStore } from "react";
import { openRoot, reconnectFolder, saveLastProject, type Place } from "../places";
import { navigate } from "../route";
import { Layout } from "./Layout";

type State =
  | { status: "loading" }
  | { status: "ready"; session: ProjectSession }
  | { status: "needs-permission" }
  | { status: "error"; message: string };

export function Editor({ place, folder }: { place: Place; folder: string }) {
  const [state, setState] = useState<State>({ status: "loading" });
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    let session: ProjectSession | null = null;
    let cancelled = false;
    (async () => {
      const root = await openRoot(place);
      if (root.status === "needs-permission") return setState({ status: "needs-permission" });
      if (root.status !== "ready")
        return setState({ status: "error", message: "The project's folder is not available." });
      session = await ProjectSession.open(await subdir(root.root, folder, false));
      if (cancelled) return void session.close();
      saveLastProject({ place, folder, name: session.store.project.name });
      setState({ status: "ready", session });
    })().catch((error: unknown) =>
      setState({
        status: "error",
        message: error instanceof Error ? error.message : String(error),
      }),
    );
    return () => {
      cancelled = true;
      void session?.close();
    };
  }, [place, folder, attempt]);

  if (state.status === "ready")
    return <Frame session={state.session} place={place} folder={folder} />;
  return (
    <div className="r-app">
      <header className="r-toolbar">
        <IconButton icon="home" label="Home" size="sm" onClick={() => navigate({ name: "home" })} />
      </header>
      <main className="editor-message">
        {state.status === "loading" && <p className="r-panel__empty">Opening the project…</p>}
        {state.status === "error" && (
          <p>
            <span className="editor-message__kind">Cannot open the project.</span> {state.message}
          </p>
        )}
        {state.status === "needs-permission" && (
          <p>
            Renderly needs your permission again to open this folder.{" "}
            <Button
              size="sm"
              onClick={() => void reconnectFolder().then(() => setAttempt((n) => n + 1))}
            >
              Reconnect
            </Button>
          </p>
        )}
      </main>
    </div>
  );
}

function useProject(session: ProjectSession): Project {
  return useSyncExternalStore(
    (listener) => session.store.subscribe(listener),
    () => session.store.project,
  );
}

function Frame({
  session,
  place,
  folder,
}: {
  session: ProjectSession;
  place: Place;
  folder: string;
}) {
  const project = useProject(session);
  const toast = useToast();
  const [role, setRole] = useState<Role>(session.role);
  const [save, setSave] = useState<SaveEvent | null>(session.autosave?.state ?? null);

  useEffect(
    () =>
      session.on((event) => {
        if (event.type === "role") {
          setRole(event.role);
          setSave(session.autosave?.state ?? null);
        }
        if (event.type === "save") setSave(event.save);
      }),
    [session],
  );

  useEffect(() => {
    if (session.recovered !== null)
      toast("Recovered edits that were not saved when the page closed");
  }, [session, toast]);

  useEffect(
    () => saveLastProject({ place, folder, name: project.name }),
    [place, folder, project.name],
  );

  // Undo and redo. Text fields keep their own undo.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement;
      if (target.closest("input, textarea, [contenteditable='true']")) return;
      const mod = event.ctrlKey || event.metaKey;
      if (!mod) return;
      const key = event.key.toLowerCase();
      if (key === "z" && !event.shiftKey) session.store.undo();
      else if ((key === "z" && event.shiftKey) || key === "y") session.store.redo();
      else return;
      event.preventDefault();
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  }, [session]);

  useEffect(() => {
    document.title = `${project.name} · Renderly`;
    return () => {
      document.title = "Renderly";
    };
  }, [project.name]);

  return (
    <div className="r-app">
      <header className="r-toolbar">
        <IconButton icon="home" label="Home" size="sm" onClick={() => navigate({ name: "home" })} />
        <h1 className="r-toolbar__title text-body-strong">{project.name}</h1>
        <IconButton
          icon="undo"
          label="Undo"
          shortcut={shortcut("Z")}
          size="sm"
          disabled={!session.store.canUndo || role !== "writer"}
          onClick={() => session.store.undo()}
        />
        <IconButton
          icon="redo"
          label="Redo"
          shortcut={shortcut("Z", true)}
          size="sm"
          disabled={!session.store.canRedo || role !== "writer"}
          onClick={() => session.store.redo()}
        />
        <span className="r-toolbar__spacer" />
        <SaveStatus role={role} save={save} />
      </header>
      {role === "follower" && (
        <div className="r-banner r-banner--warning" role="status">
          <span className="r-banner__kind">
            <Icon name="locked" />
            Read-only
          </span>
          <span>This project is open in another tab.</span>
          <Button size="sm" onClick={() => void session.takeOver()}>
            Take over editing
          </Button>
        </div>
      )}
      {save?.status === "conflict" && (
        <div className="r-banner r-banner--warning" role="alert">
          <span className="r-banner__kind">
            <Icon name="warning" />
            Changed outside
          </span>
          <span>
            Another program changed this project&apos;s file. Choose which version to keep.
          </span>
          <Button size="sm" onClick={() => void session.autosave?.resolve("mine")}>
            Keep mine
          </Button>
          <Button
            size="sm"
            onClick={() =>
              void session.autosave?.resolve("disk").then((result) => {
                if (!result.ok)
                  toast("The version on disk is not a valid project. Keep yours.", {
                    kind: "danger",
                  });
              })
            }
          >
            Load the version on disk
          </Button>
        </div>
      )}
      <Layout session={session} project={project} readOnly={role !== "writer"} />
    </div>
  );
}

/** A shortcut written the platform's way: "Ctrl Z" or "⌘Z", "Ctrl Shift Z" or "⇧⌘Z". */
function shortcut(key: string, shift = false): string {
  if (/Mac|iPhone|iPad/.test(navigator.platform)) return `${shift ? "⇧" : ""}⌘${key}`;
  return `Ctrl ${shift ? "Shift " : ""}${key}`;
}

function SaveStatus({ role, save }: { role: Role; save: SaveEvent | null }) {
  if (role !== "writer" || !save)
    return (
      <span className="r-status">
        <Icon name="locked" />
        Read-only
      </span>
    );
  const words: Record<SaveEvent["status"], [string, string, IconName | null]> = {
    saved: ["Saved", "", "check"],
    unsaved: ["Unsaved", "", null],
    saving: ["Saving…", "", null],
    error: [
      `Not saved. Retrying${save.retryIn ? ` in ${Math.round(save.retryIn / 1000)} s` : ""}`,
      "r-status--danger",
      "error",
    ],
    conflict: ["Changed outside", "r-status--warning", "warning"],
  };
  const [text, tone, icon] = words[save.status];
  return (
    <span className={`r-status ${tone}`} role="status" title={save.error}>
      {icon && <Icon name={icon} />}
      {text}
    </span>
  );
}
