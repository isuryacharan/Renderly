import { useEffect, useMemo, useState } from "react";
import { useEditorStore } from "../../store/editorStore";
import { applyTranscriptCuts, restoreTranscriptWords } from "../../lib/commands";
import { fileName } from "../../lib/format";
import {
  cuttableWordIds,
  deletedWordCount,
  restorableWordIds,
  wordRange,
} from "../../lib/text";
import type { Transcript } from "../../lib/types";
import { MenuSelect } from "../ui/MenuSelect";

/// Word-level transcripts of the project's media (made by Auto captions). Selecting words and
/// pressing "Cut selected" deletes them from the timeline (`ApplyTranscriptCuts`: real cuts
/// that ripple the gaps closed); deleted words show struck through and can be put back
/// (`RestoreTranscriptWords`). Filler words are underlined so they are easy to find.
export function TranscriptSection() {
  const project = useEditorStore((s) => s.project);
  const dispatch = useEditorStore((s) => s.dispatch);
  const transcripts = project?.transcripts ?? [];
  const [transcriptId, setTranscriptId] = useState("");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [anchor, setAnchor] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const current: Transcript | undefined = transcripts.find((t) => t.id === transcriptId) ?? transcripts[0];

  // A different transcript (or the transcript list changing under undo) clears the selection.
  useEffect(() => {
    setSelected(new Set());
    setAnchor(null);
  }, [current?.id]);

  const cut = useMemo(() => (current ? cuttableWordIds(current, selected) : []), [current, selected]);
  const restore = useMemo(() => (current ? restorableWordIds(current, selected) : []), [current, selected]);
  const fillers = useMemo(
    () => (current ? current.words.filter((w) => !w.deleted && w.tags.includes("filler")).length : 0),
    [current],
  );

  if (!project || transcripts.length === 0 || !current) {
    return (
      <div className="inspector-section">
        <h3>Transcript</h3>
        <p className="empty-hint">
          Generate captions from speech to get a transcript here. Cutting words removes them from the timeline.
        </p>
      </div>
    );
  }

  const media = project.media.find((m) => m.id === current.media_id);

  function toggleWord(id: string, shift: boolean) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (shift && anchor && current) {
        for (const w of wordRange(current, anchor, id)) next.add(w);
      } else if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
    setAnchor(id);
  }

  async function run(command: Record<string, unknown>) {
    setBusy(true);
    try {
      const ok = await dispatch(command);
      if (ok) setSelected(new Set());
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="inspector-section">
      <h3>Transcript</h3>
      {transcripts.length > 1 && (
        <div className="field">
          <label>Media</label>
          <MenuSelect
            value={current.id}
            options={transcripts.map((t) => {
              const m = project.media.find((x) => x.id === t.media_id);
              return { value: t.id, label: m ? fileName(m.path) : t.id.slice(0, 8) };
            })}
            onChange={setTranscriptId}
          />
        </div>
      )}
      <p className="insp-meta">
        {media ? fileName(media.path) : "Media"} · {current.words.length} words
        {deletedWordCount(current) > 0 ? ` · ${deletedWordCount(current)} cut` : ""}
      </p>
      <div className="transcript-words" role="group" aria-label="Transcript words">
        {current.words.map((w) => (
          <button
            key={w.id}
            type="button"
            className={`transcript-word${selected.has(w.id) ? " selected" : ""}${w.deleted ? " deleted" : ""}${w.tags.includes("filler") ? " filler" : ""}`}
            aria-pressed={selected.has(w.id)}
            title={w.deleted ? "Cut from the timeline" : w.tags.length ? w.tags.join(", ") : undefined}
            onClick={(e) => toggleWord(w.id, e.shiftKey)}
          >
            {w.text}
          </button>
        ))}
      </div>
      <div className="transcript-actions">
        <button
          type="button"
          className="btn-danger"
          disabled={busy || cut.length === 0}
          onClick={() => void run(applyTranscriptCuts(current.id, cut))}
        >
          Cut selected{cut.length ? ` (${cut.length})` : ""}
        </button>
        <button
          type="button"
          className="btn"
          disabled={busy || restore.length === 0}
          onClick={() => void run(restoreTranscriptWords(current.id, restore))}
        >
          Restore selected{restore.length ? ` (${restore.length})` : ""}
        </button>
        <button
          type="button"
          className="btn"
          disabled={busy || fillers === 0}
          onClick={() => void run(applyTranscriptCuts(current.id, [], ["filler"]))}
        >
          Cut fillers{fillers ? ` (${fillers})` : ""}
        </button>
        <button
          type="button"
          className="btn"
          disabled={busy || deletedWordCount(current) === 0}
          onClick={() => void run(restoreTranscriptWords(current.id, []))}
        >
          Restore all
        </button>
      </div>
      <p className="empty-hint">
        Click words to select (Shift-click for a range). Cuts are real edits: they close the gap on the timeline and undo with Ctrl+Z.
      </p>
    </div>
  );
}
