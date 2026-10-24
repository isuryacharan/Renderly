import { useEffect, useState } from "react";
import { Upload, FolderOpen, FilePlus, Trash2 } from "lucide-react";
import {
  createNewProjectFlow,
  openExistingProjectFlow,
  quickStartWithImport,
} from "../lib/projectFlows";
import { useEditorStore } from "../store/editorStore";
import { IconButton } from "./ui/IconButton";
import { WindowControls } from "./ui/WindowControls";
import * as ipc from "../lib/ipc";
import logoLarge from "../assets/logo-256.png";

function formatModified(ms: number): string {
  if (!ms) return "";
  const d = new Date(ms);
  return d.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function formatDuration(secs: number): string {
  if (!secs || secs < 0.05) return "Empty";
  const m = Math.floor(secs / 60);
  const s = Math.floor(secs % 60);
  return m > 0 ? `${m}m ${s}s` : `${s}s`;
}

export function WelcomeScreen() {
  const hasProject = useEditorStore((s) => !!s.project);
  const loadProjectFromPath = useEditorStore((s) => s.loadProjectFromPath);
  const toast = useEditorStore((s) => s.toast);
  const [projects, setProjects] = useState<ipc.ProjectSummary[]>([]);
  const [thumbs, setThumbs] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(true);

  async function refresh() {
    setLoading(true);
    try {
      const list = await ipc.listProjects();
      setProjects(list);
      // `modifiedMs` (the project file's own mtime) doubles as a cache-busting query param:
      // the underlying thumb PNG is regenerated in place at the same path whenever the
      // project changes, and the webview's asset-protocol fetch would otherwise cache the
      // old bytes under that unchanged URL. Keying the query string off the project's own
      // mtime means the URL only changes when the project (and therefore the thumb) does.
      const bust = (path: string, ms: number) => `${ipc.assetUrl(path)}?t=${ms}`;
      for (const p of list) {
        if (p.thumbPath) {
          // Show the last-known-good thumb immediately...
          setThumbs((t) => ({ ...t, [p.path]: bust(p.thumbPath!, p.modifiedMs) }));
        }
        // ...then always re-check freshness in the background: `project_thumbnail`
        // regenerates when the project file is newer than the cached thumb (or the thumb
        // is missing), so a stale `thumbPath` from `list_projects` still gets refreshed.
        void ipc.projectThumbnail(p.path).then((path) => {
          if (path) setThumbs((t) => ({ ...t, [p.path]: bust(path, p.modifiedMs) }));
        });
      }
    } catch (e) {
      toast(`Could not list projects: ${e instanceof Error ? e.message : String(e)}`, "error");
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (!hasProject) void refresh();
  }, [hasProject]);

  async function openCard(path: string) {
    try {
      await loadProjectFromPath(path);
      toast("Project opened", "success");
    } catch (e) {
      toast(`Open failed: ${e instanceof Error ? e.message : String(e)}`, "error");
    }
  }

  async function removeCard(path: string, name: string) {
    if (!window.confirm(`Delete “${name}”? This cannot be undone.`)) return;
    try {
      await ipc.deleteProject(path);
      await refresh();
    } catch (e) {
      toast(`Delete failed: ${e instanceof Error ? e.message : String(e)}`, "error");
    }
  }

  return (
    <div className={`welcome-screen${hasProject ? " hidden" : ""}`}>
      <div className="welcome-titlebar" data-tauri-drag-region>
        <WindowControls />
      </div>
      <div className="welcome-home">
        <div className="welcome-home-header">
          <div className="welcome-brand" aria-hidden>
            <img src={logoLarge} width={40} height={40} alt="" />
          </div>
          <div>
            <h1>Renderly</h1>
            <p className="welcome-lead">Your projects</p>
          </div>
          <div className="welcome-home-actions">
            <IconButton
              icon={Upload}
              label="Import"
              variant="primary"
              onClick={() => void quickStartWithImport()}
            />
            <IconButton
              icon={FilePlus}
              label="New"
              onClick={() => void createNewProjectFlow()}
            />
            <IconButton
              icon={FolderOpen}
              label="Open…"
              variant="ghost"
              onClick={() => void openExistingProjectFlow()}
            />
          </div>
        </div>

        {loading ? (
          <p className="welcome-empty">Loading projects…</p>
        ) : projects.length === 0 ? (
          <div className="welcome-empty-card">
            <p>
              <strong>No projects yet</strong>
            </p>
            <p className="empty-hint">Import a clip or create a new project to get started.</p>
            <IconButton
              icon={Upload}
              label="Import video"
              variant="primary"
              onClick={() => void quickStartWithImport()}
            />
          </div>
        ) : (
          <div className="project-gallery">
            <button
              type="button"
              className="project-card project-card-new"
              onClick={() => void createNewProjectFlow()}
            >
              <FilePlus size={28} strokeWidth={1.5} />
              <span>New project</span>
            </button>
            {projects.map((p) => (
              <div key={p.path} className="project-card">
                <button
                  type="button"
                  className="project-card-main"
                  onClick={() => void openCard(p.path)}
                >
                  <div
                    className="project-card-thumb"
                    style={
                      thumbs[p.path]
                        ? {
                            backgroundImage: `url(${thumbs[p.path]})`,
                            backgroundSize: "cover",
                            backgroundPosition: "center",
                          }
                        : undefined
                    }
                  />
                  <div className="project-card-meta">
                    <div className="name">{p.name}</div>
                    <div className="sub">
                      {formatDuration(p.durationUs / 1_000_000)} · {p.width}×{p.height}
                    </div>
                    <div className="sub">{formatModified(p.modifiedMs)}</div>
                  </div>
                </button>
                <button
                  type="button"
                  className="project-card-delete"
                  title="Delete project"
                  onClick={(e) => {
                    e.stopPropagation();
                    void removeCard(p.path, p.name);
                  }}
                >
                  <Trash2 size={14} strokeWidth={2} />
                </button>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
