import {
  FilePlus,
  FolderOpen,
  Save,
  Undo2,
  Redo2,
  Download,
  FileInput,
  FileOutput,
  House,
  Sun,
  Moon,
  Monitor,
} from "lucide-react";
import { useEditorStore } from "../store/editorStore";
import { getAction, invokeAction } from "../lib/actions";
import { IconButton } from "./ui/IconButton";
import { WindowControls } from "./ui/WindowControls";
import * as ipc from "../lib/ipc";
import { getThemePreference } from "../lib/theme";
import logoSmall from "../assets/logo-64.png";

function BrandMark() {
  return <img className="brand-mark-img" src={logoSmall} width={18} height={18} alt="" aria-hidden />;
}

function actionTooltip(id: string, fallback: string): string {
  const a = getAction(id);
  if (!a) return fallback;
  return a.shortcut ? `${a.label} (${a.shortcut})` : a.label;
}

export function TopBar({ onExport }: { onExport: () => void }) {
  const project = useEditorStore((s) => s.project);
  const canUndo = useEditorStore((s) => s.canUndo);
  const canRedo = useEditorStore((s) => s.canRedo);
  const saveStatus = useEditorStore((s) => s.saveStatus);
  // Subscribe (without binding) so the theme icon re-renders when the theme cycles;
  // getThemePreference() below is not reactive on its own.
  useEditorStore((s) => s.themeEpoch);

  const hasProject = !!project;

  return (
    <header
      className="topbar"
      data-tauri-drag-region
      onDoubleClick={() => void ipc.toggleMaximizeWindow()}
    >
      <div className="brand" data-tauri-drag-region>
        <BrandMark />
        <span className="brand-name">Renderly</span>
      </div>

      <div className="topbar-group">
        <IconButton
          icon={House}
          iconOnly
          tooltip={actionTooltip("project.close", "All projects")}
          disabled={!hasProject}
          onClick={() => invokeAction("project.close")}
        />
        <IconButton
          icon={FilePlus}
          iconOnly
          tooltip={actionTooltip("project.new", "New project")}
          onClick={() => invokeAction("project.new")}
        />
        <IconButton
          icon={FolderOpen}
          iconOnly
          tooltip={actionTooltip("project.open", "Open project")}
          onClick={() => invokeAction("project.open")}
        />
        <IconButton
          icon={Save}
          iconOnly
          tooltip={actionTooltip("project.save", "Save")}
          onClick={() => invokeAction("project.save")}
        />
        <IconButton
          icon={FileInput}
          iconOnly
          tooltip={actionTooltip("project.import-otio", "Import timeline (OTIO)")}
          onClick={() => invokeAction("project.import-otio")}
        />
        <IconButton
          icon={FileOutput}
          iconOnly
          tooltip={actionTooltip("project.export-otio", "Export timeline (OTIO)")}
          disabled={!hasProject}
          onClick={() => invokeAction("project.export-otio")}
        />
      </div>

      <div className="topbar-group">
        <IconButton
          icon={Undo2}
          iconOnly
          tooltip={actionTooltip("edit.undo", "Undo")}
          disabled={!canUndo}
          onClick={() => invokeAction("edit.undo")}
        />
        <IconButton
          icon={Redo2}
          iconOnly
          tooltip={actionTooltip("edit.redo", "Redo")}
          disabled={!canRedo}
          onClick={() => invokeAction("edit.redo")}
        />
      </div>

      <span className="spacer" data-tauri-drag-region />

      <div className="topbar-group">
        <IconButton
          icon={
            getThemePreference() === "dark" ? Moon : getThemePreference() === "light" ? Sun : Monitor
          }
          iconOnly
          tooltip={actionTooltip("view.cycle-theme", "Cycle theme")}
          onClick={() => invokeAction("view.cycle-theme")}
        />
      </div>

      <div className={`project-chip${hasProject ? " live" : ""}`} data-tauri-drag-region>
        <span className="dot" />
        <span className="label">{project ? project.name : "No project"}</span>
        {hasProject && saveStatus === "saved" ? (
          <span className="save-status">Saved</span>
        ) : hasProject && saveStatus === "saving" ? (
          <span className="save-status">Saving…</span>
        ) : null}
      </div>

      <IconButton
        icon={Download}
        label="Export"
        variant="primary"
        tooltip={actionTooltip("project.export", "Export video")}
        disabled={!hasProject}
        onClick={() => invokeAction("project.export", { openExport: onExport })}
      />

      <WindowControls />
    </header>
  );
}
