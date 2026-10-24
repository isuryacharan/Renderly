import { useEffect, useState } from "react";
import { setWordEmphasis, setTextBody } from "../../lib/commands";
import { animationEntries, styleLabel } from "../../lib/text";
import type { CaptionClip, TextAnimations, TextClip, Track } from "../../lib/types";
import { useStyleStatus } from "../../lib/useCatalogue";
import { formatTimecode } from "../../lib/format";
import { useEditorStore } from "../../store/editorStore";

/** Text of a caption or title. Committed with one `SetTextBody` when the field loses focus (or
 *  on Ctrl+Enter); the speech words of a caption are never touched by the engine. */
export function BodyTextField({
  track,
  clip,
  label,
  placeholder,
}: {
  track: Track;
  clip: CaptionClip | TextClip;
  label: string;
  placeholder: string;
}) {
  const dispatch = useEditorStore((s) => s.dispatch);
  const [text, setText] = useState(clip.body.text);
  useEffect(() => setText(clip.body.text), [clip.id, clip.body.text]);

  function commit() {
    if (text === clip.body.text) return;
    void dispatch(setTextBody([{ trackId: track.id, clipId: clip.id }], text));
  }

  return (
    <div className="field">
      <label>{label}</label>
      <textarea
        value={text}
        disabled={track.locked}
        placeholder={placeholder}
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
            e.preventDefault();
            commit();
          }
        }}
      />
    </div>
  );
}

/** "Style not installed" style notice for a style ref held by `ownerId` (clip, or caption track). */
export function StyleStatusNote({
  ownerId,
  styleRef,
}: {
  ownerId: string | undefined;
  styleRef: string | undefined;
}) {
  const status = useStyleStatus(ownerId, styleRef);
  if (!status || !styleRef) return null;
  const text =
    status === "changed"
      ? "This style changed since it was applied."
      : `Style not available: ${styleLabel(styleRef)}. The default style is used instead.`;
  return (
    <p className={`effect-badge ${status}`} role="status">
      {text}
    </p>
  );
}

/** Stored animations, read-only: the engine keeps them but does not render them yet. */
export function AnimationsReadOnly({ animations }: { animations: TextAnimations | undefined }) {
  const entries = animationEntries(animations);
  if (entries.length === 0) return null;
  return (
    <div className="inspector-section">
      <h3>Animations</h3>
      <ul className="text-animation-list">
        {entries.map((a) => (
          <li key={a.phase}>
            <span className="text-animation-phase">{a.phase}</span>
            <span>{styleLabel(a.ref)}</span>
          </li>
        ))}
      </ul>
      <p className="empty-hint">Animations are saved with the clip and rendered in a later release.</p>
    </div>
  );
}

/** The recognised words of a caption, as chips; click one to mark or unmark it as a key word. */
export function WordList({ track, clip }: { track: Track; clip: CaptionClip }) {
  const dispatch = useEditorStore((s) => s.dispatch);
  const words = clip.body.words;
  if (words.length === 0) return null;
  return (
    <div className="inspector-section">
      <h3>Words · {words.length}</h3>
      <div className="word-chips" role="group" aria-label="Caption words">
        {words.map((w, i) => (
          <button
            key={`${i}:${w.word_id ?? w.text}`}
            type="button"
            className={`word-chip${w.emphasis ? " emphasis" : ""}`}
            aria-pressed={w.emphasis === true}
            disabled={track.locked}
            title={`${formatTimecode(clip.start_us + w.start_us)} - ${w.emphasis ? "key word (click to clear)" : "click to mark as a key word"}`}
            onClick={() => void dispatch(setWordEmphasis(track.id, clip.id, [i], !w.emphasis))}
          >
            {w.text}
          </button>
        ))}
      </div>
      <p className="empty-hint">Key words can be highlighted by the style. Editing the text keeps these timings.</p>
    </div>
  );
}
