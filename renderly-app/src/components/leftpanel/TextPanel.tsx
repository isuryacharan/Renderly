import { useState } from "react";
import { useEditorStore } from "../../store/editorStore";
import { addCaption, generateCaptions } from "../../lib/commands";
import { fileName } from "../../lib/format";
import { normalizeFps, snapToFrameUs } from "../../lib/time";
import { DEFAULT_TEXT_STYLE_REF, styleFor } from "../../lib/text";
import { CaptionStyleGallery } from "../CaptionStyleGallery";
import { MenuSelect } from "../ui/MenuSelect";
import { TranscriptSection } from "./TranscriptSection";

/// Length of a caption added from the panel (µs).
const DEFAULT_CAPTION_US = 2_000_000;

export function TextPanel() {
  const project = useEditorStore((s) => s.project);
  const ensureTrack = useEditorStore((s) => s.ensureTrack);
  const dispatch = useEditorStore((s) => s.dispatch);
  const addTitleAtPlayhead = useEditorStore((s) => s.addTitleAtPlayhead);
  const rawPlayhead = useEditorStore((s) => s.playhead);
  // New captions start on a frame boundary of the project rate.
  const playhead = snapToFrameUs(normalizeFps(project?.settings.fps), rawPlayhead);
  const toast = useEditorStore((s) => s.toast);

  // Titles (a text layer on a video track).
  const [titleText, setTitleText] = useState("");
  const [titleStyle, setTitleStyle] = useState<string>(DEFAULT_TEXT_STYLE_REF);
  // Captions (on a caption track). "" = follow the track's caption style.
  const [text, setText] = useState("");
  const [captionStyle, setCaptionStyle] = useState<string>("");
  const [autoMediaId, setAutoMediaId] = useState("");
  const [autoBusy, setAutoBusy] = useState(false);

  const transcribable = (project?.media ?? []).filter((m) => m.kind === "video" || m.kind === "audio");

  async function addTitle() {
    if (!project) return;
    await addTitleAtPlayhead(titleText.trim() || "Title", { styleRef: titleStyle });
    setTitleText("");
  }

  async function addCaptionClip() {
    if (!text.trim() || !project) return;
    const track = await ensureTrack("caption", "Captions");
    const ok = await dispatch(
      addCaption(track.id, text.trim(), playhead, DEFAULT_CAPTION_US, captionStyle ? styleFor(captionStyle) : null),
    );
    if (ok) {
      toast("Caption added", "success");
      setText("");
    }
  }

  async function runAutoCaptions() {
    if (!autoMediaId || !project) return;
    setAutoBusy(true);
    toast("Transcribing… this may take a moment", "info");
    try {
      const track = await ensureTrack("caption", "Captions");
      const ok = await dispatch(
        generateCaptions(autoMediaId, track.id, captionStyle ? styleFor(captionStyle) : null, playhead),
      );
      if (ok) toast("Auto captions generated", "success");
    } finally {
      setAutoBusy(false);
    }
  }

  return (
    <div className="panel-body">
      <div className="inspector-section">
        <h3>Add title</h3>
        <div className="field">
          <label>Title text</label>
          <textarea value={titleText} onChange={(e) => setTitleText(e.target.value)} placeholder="Title" />
        </div>
        <div className="field">
          <label>Style</label>
          <CaptionStyleGallery label="Title styles" value={titleStyle} onChange={setTitleStyle} />
        </div>
        <div className="inspector-actions">
          <button type="button" className="btn-primary" disabled={!project} onClick={() => void addTitle()}>
            Add title at playhead
          </button>
        </div>
        <p className="empty-hint">A title is a layer on a video track: move, fade and add effects like any clip.</p>
      </div>

      <div className="inspector-section">
        <h3>Add caption</h3>
        <div className="field">
          <label>Caption text</label>
          <textarea value={text} onChange={(e) => setText(e.target.value)} placeholder="Enter caption text…" />
        </div>
        <div className="field">
          <label>Style</label>
          <CaptionStyleGallery
            label="Caption styles"
            value={captionStyle}
            onChange={setCaptionStyle}
            onInherit={() => setCaptionStyle("")}
          />
        </div>
        <div className="inspector-actions">
          <button
            type="button"
            className="btn"
            disabled={!project || !text.trim()}
            onClick={() => void addCaptionClip()}
          >
            Add caption at playhead
          </button>
        </div>
      </div>

      <div className="inspector-section">
        <h3>Auto captions</h3>
        <div className="field">
          <label>Source media</label>
          <MenuSelect
            value={autoMediaId}
            options={[
              { value: "", label: "Choose video or audio…" },
              ...transcribable.map((m) => ({
                value: m.id,
                label: fileName(m.path),
              })),
            ]}
            onChange={setAutoMediaId}
          />
        </div>
        <div className="inspector-actions">
          <button
            type="button"
            className="btn"
            disabled={!autoMediaId || autoBusy}
            onClick={() => void runAutoCaptions()}
          >
            {autoBusy ? "Transcribing…" : "Generate from speech"}
          </button>
        </div>
        <p className="empty-hint">Requires whisper-cli on PATH and RENDERLY_WHISPER_MODEL set.</p>
      </div>

      <TranscriptSection />
    </div>
  );
}
