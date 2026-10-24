import { useEffect, useState } from "react";
import { FolderOpen, LayoutTemplate, Package, Puzzle, Trash2 } from "lucide-react";
import {
  applyTemplate,
  loadAssetPack,
  loadWasmPlugin,
  unloadAssetPack,
  unloadWasmPlugin,
} from "../../lib/commands";
import * as ipc from "../../lib/ipc";
import type { ExtensionCatalog, RegistryEntry } from "../../lib/ipc";
import { normalizeFps, snapToFrameUs } from "../../lib/time";
import { useCatalogueStore } from "../../store/catalogueStore";
import { useEditorStore } from "../../store/editorStore";

export function ExtensionsPanel() {
  const project = useEditorStore((s) => s.project);
  const rawPlayhead = useEditorStore((s) => s.playhead);
  const playhead = snapToFrameUs(normalizeFps(project?.settings.fps), rawPlayhead);
  const dispatch = useEditorStore((s) => s.dispatch);
  const toast = useEditorStore((s) => s.toast);
  const [catalog, setCatalog] = useState<ExtensionCatalog | null>(null);
  const [registry, setRegistry] = useState<RegistryEntry[]>([]);
  const [busy, setBusy] = useState(false);

  async function refresh() {
    try {
      const [ext, reg] = await Promise.all([
        ipc.listExtensions(),
        ipc.listRegistry().catch(() => [] as RegistryEntry[]),
      ]);
      setCatalog(ext);
      setRegistry(reg);
      // The effect catalogue (inspector, effect panels, preview pack files) follows the library.
      void useCatalogueStore.getState().refresh();
    } catch {
      setCatalog(null);
    }
  }

  useEffect(() => {
    if (!project) {
      setCatalog(null);
      return;
    }
    void refresh();
  }, [project, project?.legacy_pack_paths?.join("|")]);

  if (!project) {
    return (
      <div className="panel-body">
        <h3>Extensions</h3>
        <p className="empty-hint">Open a project to load asset packs and WASM plugins.</p>
      </div>
    );
  }

  async function addPack() {
    const path = await ipc.pickExtensionFolder("Choose asset pack folder (pack.json)");
    if (!path) return;
    setBusy(true);
    try {
      const ok = await dispatch(loadAssetPack(path));
      if (ok) {
        toast("Asset pack loaded", "success");
        await refresh();
      }
    } finally {
      setBusy(false);
    }
  }

  async function addPlugin() {
    const path = await ipc.pickExtensionFolder("Choose plugin folder (plugin.json)");
    if (!path) return;
    setBusy(true);
    try {
      const ok = await dispatch(loadWasmPlugin(path));
      if (ok) {
        toast("Plugin loaded", "success");
        await refresh();
      }
    } finally {
      setBusy(false);
    }
  }

  async function loadRegistryPath(path: string, kind: "pack" | "plugin") {
    setBusy(true);
    try {
      const cmd = kind === "pack" ? loadAssetPack(path) : loadWasmPlugin(path);
      const ok = await dispatch(cmd);
      if (ok) {
        toast(`Loaded ${kind}`, "success");
        await refresh();
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="panel-body">
      <h3>Extensions</h3>
      <p className="empty-hint">Load and unload asset packs and WASM plugins.</p>
      <div className="effects-catalog" style={{ marginBottom: "0.75rem" }}>
        <button type="button" disabled={busy} onClick={() => void addPack()}>
          <FolderOpen size={14} strokeWidth={1.75} />
          Add pack folder…
        </button>
        <button type="button" disabled={busy} onClick={() => void addPlugin()}>
          <FolderOpen size={14} strokeWidth={1.75} />
          Add plugin folder…
        </button>
      </div>

      <h4>Loaded packs</h4>
      {(catalog?.packs.length ?? 0) === 0 ? (
        <p className="empty-hint">No packs loaded.</p>
      ) : (
        <ul className="effect-list">
          {catalog!.packs.map((p) => (
            <li key={p.id}>
              <div className="effect-list-header">
                <span>
                  <Package size={14} strokeWidth={1.75} /> {p.name} ({p.id})
                </span>
                <button
                  type="button"
                  className="btn-ghost"
                  disabled={busy}
                  title="Unload"
                  onClick={() =>
                    void dispatch(unloadAssetPack(p.id)).then((ok) => {
                      if (ok) void refresh();
                    })
                  }
                >
                  <Trash2 size={14} strokeWidth={1.75} />
                </button>
              </div>
              <p className="empty-hint" style={{ margin: 0 }}>
                {p.stickers.length} stickers · {p.sfx.length} sfx · {p.luts.length} LUTs
                {(p.templates?.length ?? 0) > 0 ? ` · ${p.templates!.length} templates` : ""}
              </p>
              {(p.templates?.length ?? 0) > 0 && (
                <div className="effects-catalog" style={{ marginTop: "0.5rem" }}>
                  {p.templates!.map((t) => (
                    <button
                      key={`${p.id}:${t.id}`}
                      type="button"
                      disabled={busy}
                      onClick={() =>
                        void dispatch(applyTemplate(p.id, t.id, playhead)).then((ok) => {
                          if (ok) toast(`Applied template: ${t.label}`, "success");
                        })
                      }
                    >
                      <LayoutTemplate size={14} strokeWidth={1.75} />
                      {t.label}
                    </button>
                  ))}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}

      <h4 style={{ marginTop: "1rem" }}>Loaded plugins</h4>
      {(catalog?.plugins.length ?? 0) === 0 ? (
        <p className="empty-hint">No plugins loaded.</p>
      ) : (
        <ul className="effect-list">
          {catalog!.plugins.map((p) => (
            <li key={p.id}>
              <div className="effect-list-header">
                <span>
                  <Puzzle size={14} strokeWidth={1.75} /> {p.name} ({p.id})
                  {p.has_audio ? " · audio" : ""}
                  {p.has_frame ? " · video" : ""}
                </span>
                <button
                  type="button"
                  className="btn-ghost"
                  disabled={busy}
                  title="Unload"
                  onClick={() =>
                    void dispatch(unloadWasmPlugin(p.id)).then((ok) => {
                      if (ok) void refresh();
                    })
                  }
                >
                  <Trash2 size={14} strokeWidth={1.75} />
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}

      {registry.length > 0 && (
        <>
          <h4 style={{ marginTop: "1rem" }}>Registry (local)</h4>
          <ul className="effect-list">
            {registry.map((e) => (
              <li key={`${e.kind}:${e.id}`}>
                <div className="effect-list-header">
                  <span>
                    {e.kind === "pack" ? (
                      <Package size={14} strokeWidth={1.75} />
                    ) : (
                      <Puzzle size={14} strokeWidth={1.75} />
                    )}{" "}
                    {e.id} — {e.summary}
                  </span>
                  {e.resolved_path && (
                    <button
                      type="button"
                      className="btn-ghost"
                      disabled={busy}
                      onClick={() => void loadRegistryPath(e.resolved_path!, e.kind)}
                    >
                      Load
                    </button>
                  )}
                </div>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}
