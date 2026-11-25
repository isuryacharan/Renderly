// Home: the projects in one place (browser storage or a disk folder), and what can be done to
// them: create, open, rename, duplicate, delete to the recycle bin, restore.

import { newProject } from "@renderly/core";
import {
  createProject,
  duplicateProject,
  listProjects,
  listRecycled,
  purgeProject,
  readQuota,
  recycleProject,
  renameProject,
  restoreProject,
  type ProjectEntry,
} from "@renderly/storage";
import { Button, Dialog, Icon, MenuButton, Tooltip, useToast } from "@renderly/ui/react";
import { useCallback, useEffect, useId, useState, type FormEvent } from "react";
import { formatBytes, formatDate, formatDuration } from "../format";
import {
  chooseFolder,
  lastProject,
  openRoot,
  reconnectFolder,
  savedPlace,
  savePlace,
  type Place,
  type RootState,
} from "../places";
import { navigate } from "../route";
import { applyTheme, savedTheme, type Theme } from "../theme";

type Ask =
  | { kind: "create" }
  | { kind: "rename"; entry: ProjectEntry }
  | { kind: "bin" }
  | { kind: "purge"; entry: ProjectEntry }
  | null;

const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

export function Home() {
  const toast = useToast();
  const [place, setPlace] = useState<Place>(savedPlace);
  const [root, setRoot] = useState<RootState | null>(null);
  const [projects, setProjects] = useState<ProjectEntry[]>([]);
  const [recycled, setRecycled] = useState<ProjectEntry[]>([]);
  const [usage, setUsage] = useState<string>("");
  const [ask, setAsk] = useState<Ask>(null);
  const last = lastProject();

  const refresh = useCallback(async () => {
    const state = await openRoot(place);
    setRoot(state);
    if (state.status !== "ready") {
      setProjects([]);
      setRecycled([]);
      return;
    }
    setProjects(await listProjects(state.root));
    setRecycled(await listRecycled(state.root));
    if (place === "browser") {
      const quota = await readQuota();
      setUsage(
        `Using ${formatBytes(quota.usage)} of ${formatBytes(quota.quota)} in browser storage.`,
      );
    } else setUsage("");
  }, [place]);

  useEffect(() => {
    void refresh().catch((error: unknown) => toast(message(error), { kind: "danger" }));
    const onFocus = () => void refresh().catch(() => undefined);
    addEventListener("focus", onFocus);
    return () => removeEventListener("focus", onFocus);
  }, [refresh, toast]);

  /** Runs an action on the ready root, then refreshes; failures become an error toast. */
  const act = async (action: (root: FileSystemDirectoryHandle) => Promise<unknown>) => {
    if (root?.status !== "ready") return;
    try {
      await action(root.root);
    } catch (error) {
      toast(message(error), { kind: "danger" });
    }
    await refresh().catch(() => undefined);
  };

  const open = (folder: string) => navigate({ name: "editor", place, folder });

  const changePlace = (next: Place) => {
    savePlace(next);
    setPlace(next);
  };

  return (
    <div className="r-app">
      <header className="r-toolbar">
        <h1 className="r-toolbar__title text-body-strong home__brand">Renderly</h1>
        <span className="r-toolbar__spacer" />
        <ThemeSwitch />
      </header>

      <main className="home">
        <div className="home__main">
          <div className="home__bar">
            <h2 className="text-heading home__heading">Projects</h2>
            <div className="r-segmented" role="group" aria-label="Where projects are kept">
              <button
                type="button"
                aria-pressed={place === "browser"}
                onClick={() => changePlace("browser")}
              >
                <Icon name="storage" />
                Browser storage
              </button>
              <button
                type="button"
                aria-pressed={place === "disk"}
                onClick={() => changePlace("disk")}
              >
                <Icon name="folder" />
                Disk folder
              </button>
            </div>
            {place === "disk" && root?.status === "ready" && (
              <span className="home__folder">{root.label}</span>
            )}
            {place === "disk" && root?.status !== "unsupported" && (
              <Button
                size="sm"
                onClick={() =>
                  void chooseFolder().then(refresh, (e: unknown) =>
                    toast(message(e), { kind: "danger" }),
                  )
                }
              >
                <Icon name="folder" />
                {root?.status === "ready" ? "Change folder…" : "Choose folder…"}
              </Button>
            )}
            <span className="r-toolbar__spacer" />
            {last && (
              <Button
                onClick={() => navigate({ name: "editor", place: last.place, folder: last.folder })}
              >
                <Icon name="folder-open" />
                Continue {last.name}
              </Button>
            )}
            <Button
              variant="primary"
              disabled={root?.status !== "ready"}
              onClick={() => setAsk({ kind: "create" })}
            >
              <Icon name="add" />
              New project
            </Button>
          </div>

          {root?.status === "no-folder" && (
            <p className="home__note">
              Choose a folder on your disk to keep projects in. Renderly reads and writes only that
              folder.
            </p>
          )}
          {root?.status === "unsupported" && (
            <p className="home__note">
              This browser cannot open folders on disk. Use browser storage, or Chrome or Edge.
            </p>
          )}
          {root?.status === "needs-permission" && (
            <div className="home__note">
              <span>Renderly needs your permission again to open {root.label}.</span>{" "}
              <Button size="sm" onClick={() => void reconnectFolder().then(refresh)}>
                <Icon name="reconnect" />
                Reconnect
              </Button>
            </div>
          )}

          {root?.status === "ready" && projects.length === 0 && (
            <p className="home__note">No projects here yet. Create one to start.</p>
          )}

          <ul className="r-card-grid" aria-label="Projects">
            {projects.map((entry) => (
              <li key={entry.folder} className="r-card">
                <div className="r-card__cover" />
                <button type="button" className="r-card__open" onClick={() => open(entry.folder)}>
                  <span className="r-card__name">{entry.meta.name}</span>
                  <span className="r-card__meta">
                    <span className="r-tabular">{formatDuration(entry.meta.duration)}</span> ·
                    Edited {formatDate(entry.meta.modified)}
                  </span>
                </button>
                <div className="r-card__menu">
                  <MenuButton
                    label={<Icon name="more" />}
                    aria-label={`More actions for ${entry.meta.name}`}
                    className="r-button r-button--icon r-button--sm"
                    tooltip="More actions"
                    items={[
                      { label: "Open", onSelect: () => open(entry.folder) },
                      { label: "Rename…", onSelect: () => setAsk({ kind: "rename", entry }) },
                      {
                        label: "Duplicate",
                        onSelect: () =>
                          void act(async (r) => {
                            await duplicateProject(r, entry.folder, `${entry.meta.name} copy`);
                            toast("Project duplicated");
                          }),
                      },
                      "separator",
                      {
                        label: "Delete",
                        danger: true,
                        onSelect: () =>
                          void act(async (r) => {
                            await recycleProject(r, entry.folder);
                            toast(`Moved ${entry.meta.name} to the recycle bin`, {
                              action: {
                                label: "Undo",
                                onSelect: () =>
                                  void act((again) => restoreProject(again, entry.folder)),
                              },
                            });
                          }),
                      },
                    ]}
                  />
                </div>
              </li>
            ))}
          </ul>

          <div className="home__footer">
            <Button
              variant="ghost"
              size="sm"
              disabled={root?.status !== "ready"}
              onClick={() => setAsk({ kind: "bin" })}
            >
              <Icon name="recycle-bin" />
              Recycle bin ({recycled.length})
            </Button>
            <span className="home__quota">{usage}</span>
          </div>
        </div>
      </main>

      <NameDialog
        open={ask?.kind === "create"}
        title="New project"
        confirm="Create"
        initial="Untitled project"
        onClose={() => setAsk(null)}
        onConfirm={(name) =>
          void act(async (r) => {
            const folder = await createProject(r, newProject(name));
            setAsk(null);
            open(folder);
          })
        }
      />
      <NameDialog
        open={ask?.kind === "rename"}
        title="Rename project"
        confirm="Rename"
        initial={ask?.kind === "rename" ? ask.entry.meta.name : ""}
        onClose={() => setAsk(null)}
        onConfirm={(name) =>
          void act(async (r) => {
            if (ask?.kind !== "rename") return;
            setAsk(null);
            const result = await renameProject(r, ask.entry.folder, name);
            if (result === "open-elsewhere")
              toast("This project is open in another tab. Rename it there.", { kind: "danger" });
            else toast("Project renamed");
          })
        }
      />
      <Dialog
        open={ask?.kind === "bin"}
        onClose={() => setAsk(null)}
        title="Recycle bin"
        actions={
          <Button variant="primary" onClick={() => setAsk(null)}>
            Done
          </Button>
        }
      >
        {recycled.length === 0 ? (
          <p className="r-dialog__text">The recycle bin is empty.</p>
        ) : (
          <ul className="home__bin" aria-label="Deleted projects">
            {recycled.map((entry) => (
              <li key={entry.folder} className="home__bin-row">
                <span className="home__bin-name">{entry.meta.name}</span>
                <Button
                  size="sm"
                  onClick={() =>
                    void act((r) => restoreProject(r, entry.folder)).then(() =>
                      toast(`Restored ${entry.meta.name}`),
                    )
                  }
                >
                  <Icon name="restore" />
                  Restore
                </Button>
                <Button size="sm" variant="danger" onClick={() => setAsk({ kind: "purge", entry })}>
                  <Icon name="delete" />
                  Delete for good…
                </Button>
              </li>
            ))}
          </ul>
        )}
      </Dialog>
      <Dialog
        open={ask?.kind === "purge"}
        onClose={() => setAsk({ kind: "bin" })}
        title="Delete for good?"
        actions={
          <>
            <Button onClick={() => setAsk({ kind: "bin" })}>Cancel</Button>
            <Button
              variant="primary"
              onClick={() =>
                void act(async (r) => {
                  if (ask?.kind !== "purge") return;
                  await purgeProject(r, ask.entry.folder);
                  setAsk({ kind: "bin" });
                  toast("Project deleted");
                })
              }
            >
              Delete
            </Button>
          </>
        }
      >
        <p className="r-dialog__text">
          {ask?.kind === "purge" ? ask.entry.meta.name : "The project"} and every file in its folder
          will be deleted. This cannot be undone.
        </p>
      </Dialog>
    </div>
  );
}

interface NameDialogProps {
  open: boolean;
  title: string;
  confirm: string;
  initial: string;
  onClose: () => void;
  onConfirm: (name: string) => void;
}

function NameDialog({ open, title, confirm, initial, onClose, onConfirm }: NameDialogProps) {
  const [name, setName] = useState(initial);
  const id = useId();
  useEffect(() => {
    if (open) setName(initial);
  }, [open, initial]);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (name.trim()) onConfirm(name.trim());
  };
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={title}
      actions={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" type="submit" form={id} disabled={!name.trim()}>
            {confirm}
          </Button>
        </>
      }
    >
      <form id={id} className="r-form" onSubmit={submit}>
        <label className="r-label" htmlFor={`${id}-name`}>
          Name
        </label>
        <div className="r-field">
          <input
            id={`${id}-name`}
            autoFocus
            value={name}
            onFocus={(event) => event.target.select()}
            onChange={(event) => setName(event.target.value)}
          />
        </div>
      </form>
    </Dialog>
  );
}

function ThemeSwitch() {
  const [theme, setTheme] = useState<Theme>(savedTheme);
  const choose = (next: Theme) => {
    applyTheme(next);
    setTheme(next);
  };
  return (
    // Moon, sun and screen read clearly as a theme switch: icons alone, named by their tooltips.
    <div className="r-segmented r-segmented--icons" role="group" aria-label="Theme">
      {(["dark", "light", "system"] as const).map((option) => {
        const label =
          option === "dark"
            ? "Dark theme"
            : option === "light"
              ? "Light theme"
              : "Match the system";
        return (
          <Tooltip key={option} label={label}>
            <button
              type="button"
              aria-label={label}
              aria-pressed={theme === option}
              onClick={() => choose(option)}
            >
              <Icon name={`theme-${option}`} />
            </button>
          </Tooltip>
        );
      })}
    </div>
  );
}
