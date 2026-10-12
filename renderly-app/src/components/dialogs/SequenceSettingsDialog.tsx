import { useEffect, useRef, useState } from "react";
import { useEditorStore } from "../../store/editorStore";
import { setSequenceSettings } from "../../lib/commands";
import { findSequence } from "../../lib/container";

/// Small dialog for a sequence's output size (`SetSequenceSettings`): leave a field empty to
/// inherit the project's size. Same native `<dialog>` pattern as ExportDialog.
export function SequenceSettingsDialog({
  sequenceId,
  onClose,
}: {
  sequenceId: string | null;
  onClose: () => void;
}) {
  const dialogRef = useRef<HTMLDialogElement | null>(null);
  const project = useEditorStore((s) => s.project);
  const dispatch = useEditorStore((s) => s.dispatch);
  const seq = project && sequenceId ? findSequence(project, sequenceId) : undefined;
  const [width, setWidth] = useState("");
  const [height, setHeight] = useState("");

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (seq && !dialog.open) {
      setWidth(seq.width != null ? String(seq.width) : "");
      setHeight(seq.height != null ? String(seq.height) : "");
      dialog.showModal();
    }
    if (!seq && dialog.open) dialog.close();
    // Re-run only when the dialog opens/closes for a sequence.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [seq?.id]);

  function parseDim(v: string): number | null | undefined {
    const t = v.trim();
    if (t === "") return null;
    const n = Number(t);
    return Number.isInteger(n) && n > 0 ? n : undefined;
  }

  const w = parseDim(width);
  const h = parseDim(height);
  const valid = w !== undefined && h !== undefined;

  async function save() {
    if (!seq || !valid) return;
    const ok = await dispatch(setSequenceSettings(seq.id, { width: w, height: h }));
    if (ok) onClose();
  }

  return (
    <dialog ref={dialogRef} onClose={onClose} aria-label="Sequence settings">
      <h3>Sequence settings</h3>
      <p className="dialog-sub">
        {seq ? `“${seq.name}” renders at its own size. ` : ""}
        Leave a field empty to inherit the project size ({project?.settings.width}×
        {project?.settings.height}).
      </p>
      <div className="seq-settings-grid">
        <div className="field">
          <label htmlFor="seq-width">Width (px)</label>
          <input
            id="seq-width"
            type="number"
            min={1}
            step={1}
            placeholder="inherit"
            value={width}
            onChange={(e) => setWidth(e.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor="seq-height">Height (px)</label>
          <input
            id="seq-height"
            type="number"
            min={1}
            step={1}
            placeholder="inherit"
            value={height}
            onChange={(e) => setHeight(e.target.value)}
          />
        </div>
      </div>
      <menu>
        <button type="button" className="btn" onClick={onClose}>
          Cancel
        </button>
        <button type="button" className="btn-primary" disabled={!valid} onClick={() => void save()}>
          Save
        </button>
      </menu>
    </dialog>
  );
}
