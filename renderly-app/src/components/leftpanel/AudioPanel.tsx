import { useState } from "react";
import { Music, Plus } from "lucide-react";
import { useEditorStore } from "../../store/editorStore";
import { fileName } from "../../lib/format";
import { generateVoiceover } from "../../lib/commands";
import { pickAndImportMedia } from "../../lib/projectFlows";
import { startMediaDrag } from "../../lib/dragMedia";
import { normalizeFps, snapToFrameUs, usToSecs } from "../../lib/time";
import { MenuSelect } from "../ui/MenuSelect";
import { Tooltip } from "../ui/Tooltip";

export function AudioPanel() {
  const project = useEditorStore((s) => s.project);
  const mediaAssets = useEditorStore((s) => s.mediaAssets);
  const placeMediaOnTimeline = useEditorStore((s) => s.placeMediaOnTimeline);
  const ensureTrack = useEditorStore((s) => s.ensureTrack);
  const dispatch = useEditorStore((s) => s.dispatch);
  const rawPlayhead = useEditorStore((s) => s.playhead);
  const playhead = snapToFrameUs(normalizeFps(project?.settings.fps), rawPlayhead);
  const toast = useEditorStore((s) => s.toast);

  const [ttsText, setTtsText] = useState("");
  const [provider, setProvider] = useState<"piper_local" | "open_ai">("piper_local");
  const [busy, setBusy] = useState(false);

  const items = (project?.media ?? []).filter((m) => m.kind === "audio");

  async function generate() {
    if (!ttsText.trim() || !project) return;
    setBusy(true);
    try {
      const track = await ensureTrack("audio", "Voiceover");
      const outputPath = `${project.name.replace(/[^\w-]+/g, "_")}-voiceover-${Date.now()}.wav`;
      const ok = await dispatch(
        generateVoiceover(
          ttsText.trim(),
          track.id,
          playhead,
          outputPath,
          provider === "open_ai" ? { provider: "open_ai", voice: "alloy" } : { provider: "piper_local" },
        ),
      );
      if (ok) {
        toast("Voiceover generated", "success");
        setTtsText("");
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="panel-body">
      <div className="inspector-section">
        <h3>Text to speech</h3>
        <div className="field">
          <label>Script</label>
          <textarea
            value={ttsText}
            onChange={(e) => setTtsText(e.target.value)}
            placeholder="Type narration text…"
          />
        </div>
        <div className="field">
          <label>Voice provider</label>
          <MenuSelect
            value={provider}
            options={[
              { value: "piper_local", label: "Piper (local)" },
              { value: "open_ai", label: "OpenAI (BYO key)" },
            ]}
            onChange={(v) => setProvider(v as typeof provider)}
          />
        </div>
        <div className="inspector-actions">
          <button
            type="button"
            className="btn-primary"
            disabled={!project || busy || !ttsText.trim()}
            onClick={() => void generate()}
          >
            {busy ? "Generating…" : "Generate voiceover"}
          </button>
        </div>
      </div>

      <div className="inspector-section">
        <h3>Audio files</h3>
        <button type="button" className="btn" onClick={() => void pickAndImportMedia()}>
          Import audio
        </button>
      {items.length === 0 ? (
        <div className="empty-state">
          <div className="empty-state-icon">
            <Music size={28} strokeWidth={1.5} />
          </div>
          <p>
            <strong>No audio yet</strong>
          </p>
          <p className="empty-hint">Import audio or generate a voiceover above.</p>
        </div>
      ) : (
          items.map((item) => {
            const peaks = mediaAssets[item.id]?.waveform?.peaks;
            return (
            <div
              key={item.id}
              className="media-item"
              draggable
              onDragStart={(e) => startMediaDrag(e, item.id, item.kind, item.duration_us ?? 5_000_000)}
              onClick={() => void placeMediaOnTimeline(item.id, item.kind)}
            >
              {peaks?.length ? (
                <div className="media-thumb audio waveform-thumb" aria-hidden>
                  {(() => {
                    const bars = 24;
                    const step = Math.max(1, Math.floor(peaks.length / bars));
                    const sampled: number[] = [];
                    for (let i = 0; i < bars; i++) {
                      let max = 0;
                      for (let j = 0; j < step && i * step + j < peaks.length; j++) {
                        max = Math.max(max, Math.abs(peaks[i * step + j] ?? 0));
                      }
                      sampled.push(max);
                    }
                    const peak = Math.max(0.01, ...sampled);
                    return sampled.map((v, i) => (
                      <span key={i} style={{ height: `${Math.max(8, (v / peak) * 100)}%` }} />
                    ));
                  })()}
                </div>
              ) : (
                <div className="media-thumb audio">
                  <Music size={18} strokeWidth={1.5} />
                </div>
              )}
              <div className="media-meta">
                <div className="name">{fileName(item.path)}</div>
                <div className="sub">{item.duration_us != null ? usToSecs(item.duration_us).toFixed(1) : "?"}s</div>
              </div>
              <Tooltip content="Add to timeline">
                <span className="media-add-hint">
                  <Plus size={14} strokeWidth={2} />
                </span>
              </Tooltip>
            </div>
            );
          })
        )}
      </div>
    </div>
  );
}
