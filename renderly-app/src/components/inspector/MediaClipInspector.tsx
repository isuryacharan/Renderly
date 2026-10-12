import { useEffect, useState } from "react";
import { Trash2, Link2 } from "lucide-react";
import { useEditorStore } from "../../store/editorStore";
import {
  deleteClip,
  generateBackgroundMatte,
  setAudioFade,
  setAudioGain,
  setClipAudioDenoise,
  setClipBackgroundRemoval,
  setClipEnabled,
  setClipSpeed,
  setClipTransform,
  setTrackAudioRole,
  stabilizeClip,
  trackMotion,
  autoReframeClip,
  trimClip,
} from "../../lib/commands";
import type { ClipTransform, MediaClip, Track, TrackAudioRole } from "../../lib/types";
import { IDENTITY_TRANSFORM } from "../../lib/types";
import { MenuSelect } from "../ui/MenuSelect";
import { Tooltip } from "../ui/Tooltip";
import { KeyframeEditor } from "./KeyframeEditor";
import { EffectStackSection } from "./EffectStackSection";
import { MaskSection } from "./MaskSection";
import { MulticamSection } from "./MulticamSection";

const AUDIO_ROLES: { value: TrackAudioRole | ""; label: string }[] = [
  { value: "", label: "None" },
  { value: "voiceover", label: "Voiceover" },
  { value: "dialog", label: "Dialog" },
  { value: "music", label: "Music" },
  { value: "ambience", label: "Ambience" },
];

function clipTransform(clip: MediaClip): ClipTransform {
  return clip.transform ?? IDENTITY_TRANSFORM;
}

function NumCell({
  label,
  value,
  step,
  min,
  disabled,
  onChange,
  onCommit,
}: {
  label: string;
  value: number;
  step?: number;
  min?: number;
  disabled?: boolean;
  onChange: (v: number) => void;
  onCommit: (v: number) => void;
}) {
  return (
    <label className="insp-cell">
      <span>{label}</span>
      <input
        type="number"
        step={step ?? 0.05}
        min={min}
        value={Number.isFinite(value) ? value : 0}
        disabled={disabled}
        onChange={(e) => onChange(parseFloat(e.target.value) || 0)}
        onBlur={(e) => onCommit(parseFloat(e.target.value) || 0)}
      />
    </label>
  );
}

/** New denoise configs from the GUI use RNNoise; saved `afftdn` projects keep their backend. */
const DEFAULT_DENOISE_BACKEND = "rnnoise";
const DENOISE_BACKENDS = [
  { value: "rnnoise", label: "RNNoise (recommended)" },
  { value: "afftdn", label: "Spectral (afftdn)" },
];

export function MediaClipInspector({ track, clip }: { track: Track; clip: MediaClip }) {
  const dispatch = useEditorStore((s) => s.dispatch);
  const select = useEditorStore((s) => s.select);
  const project = useEditorStore((s) => s.project);
  const media = useEditorStore((s) => s.project?.media.find((m) => m.id === clip.media_id));

  const [gain, setGain] = useState(clip.gain_db);
  const [sourceIn, setSourceIn] = useState(clip.source_in_secs);
  const [sourceOut, setSourceOut] = useState(clip.source_out_secs);
  const [fadeIn, setFadeIn] = useState(clip.fade_in_secs);
  const [fadeOut, setFadeOut] = useState(clip.fade_out_secs);
  const [transform, setTransform] = useState<ClipTransform>(() => clipTransform(clip));
  const [speed, setSpeed] = useState(clip.speed ?? 1);
  const [linkScale, setLinkScale] = useState(true);
  const [denoiseStrength, setDenoiseStrength] = useState(clip.audio_denoise?.strength ?? 0.5);

  function commitDenoiseStrength() {
    if (!clip.audio_denoise) return;
    void dispatch(
      setClipAudioDenoise(track.id, clip.id, {
        ...clip.audio_denoise,
        strength: denoiseStrength,
      }),
    );
  }

  const showAudio = clip.type === "audio" || track.kind === "audio" || clip.type === "video";
  const showTransform = clip.type === "video" || track.kind === "video";
  type InspTab = "video" | "audio" | "speed" | "effects";
  const [tab, setTab] = useState<InspTab>(showTransform ? "video" : "audio");

  useEffect(() => {
    setGain(clip.gain_db);
    setSourceIn(clip.source_in_secs);
    setSourceOut(clip.source_out_secs);
    setFadeIn(clip.fade_in_secs);
    setFadeOut(clip.fade_out_secs);
    setTransform(clipTransform(clip));
    setSpeed(clip.speed ?? 1);
    setDenoiseStrength(clip.audio_denoise?.strength ?? 0.5);
  }, [
    clip.id,
    clip.gain_db,
    clip.source_in_secs,
    clip.source_out_secs,
    clip.fade_in_secs,
    clip.fade_out_secs,
    clip.transform,
    clip.speed,
    clip.audio_denoise?.strength,
  ]);

  async function commitTransform(next: ClipTransform) {
    setTransform(next);
    await dispatch(setClipTransform(track.id, clip.id, next));
  }

  const fileName = media ? media.path.split(/[/\\]/).pop() : null;

  const tabs: { id: InspTab; label: string; show: boolean }[] = [
    { id: "video", label: "Video", show: showTransform },
    { id: "audio", label: "Audio", show: showAudio },
    { id: "speed", label: "Speed", show: true },
    { id: "effects", label: "Effects", show: true },
  ];
  const activeTab = tabs.find((t) => t.id === tab && t.show) ? tab : tabs.find((t) => t.show)!.id;

  return (
    <div className="inspector">
      <div className="inspector-section">
        <div className="insp-clip-head">
          <div>
            <h3>{clip.type === "audio" ? "Audio clip" : "Video clip"}</h3>
            <p className="insp-meta">
              {track.name}
              {fileName ? ` · ${fileName}` : ""}
            </p>
            {media?.width && media?.height ? (
              <p className="insp-meta muted">
                {media.width}×{media.height}
                {media.fps ? ` · ${media.fps.toFixed(0)} fps` : ""}
                {media.duration_secs != null ? ` · ${media.duration_secs.toFixed(1)}s` : ""}
              </p>
            ) : null}
          </div>
          <label className="insp-enable">
            <input
              type="checkbox"
              checked={clip.enabled}
              disabled={track.locked}
              onChange={(e) => void dispatch(setClipEnabled(track.id, clip.id, e.target.checked))}
            />
            <span>On</span>
          </label>
        </div>
      </div>

      <div className="insp-tabs">
        {tabs
          .filter((t) => t.show)
          .map((t) => (
            <button
              key={t.id}
              type="button"
              className={`insp-tab${activeTab === t.id ? " on" : ""}`}
              onClick={() => setTab(t.id)}
            >
              {t.label}
            </button>
          ))}
      </div>

      {activeTab === "video" && (
      <>
      <div className="inspector-section">
        <h3>Trim</h3>
        <div className="insp-grid-2">
          <NumCell
            label="In"
            value={sourceIn}
            step={0.1}
            min={0}
            disabled={track.locked}
            onChange={setSourceIn}
            onCommit={(v) => {
              setSourceIn(v);
              void dispatch(trimClip(track.id, clip.id, v, sourceOut));
            }}
          />
          <NumCell
            label="Out"
            value={sourceOut}
            step={0.1}
            min={0}
            disabled={track.locked}
            onChange={setSourceOut}
            onCommit={(v) => {
              setSourceOut(v);
              void dispatch(trimClip(track.id, clip.id, sourceIn, v));
            }}
          />
        </div>
      </div>

      {showTransform && (
        <div className="inspector-section">
          <h3>Transform</h3>
          <div className="insp-grid-2">
            <NumCell
              label="X"
              value={transform.x}
              disabled={track.locked}
              onChange={(x) => setTransform({ ...transform, x })}
              onCommit={(x) => void commitTransform({ ...transform, x })}
            />
            <NumCell
              label="Y"
              value={transform.y}
              disabled={track.locked}
              onChange={(y) => setTransform({ ...transform, y })}
              onCommit={(y) => void commitTransform({ ...transform, y })}
            />
          </div>
          <div className="insp-scale-row">
            <NumCell
              label="Scale X"
              value={transform.scale_x}
              min={0.01}
              disabled={track.locked}
              onChange={(scale_x) => {
                if (linkScale) {
                  setTransform({ ...transform, scale_x, scale_y: scale_x });
                } else {
                  setTransform({ ...transform, scale_x });
                }
              }}
              onCommit={(scale_x) => {
                const next = linkScale
                  ? { ...transform, scale_x, scale_y: scale_x }
                  : { ...transform, scale_x };
                void commitTransform(next);
              }}
            />
            <Tooltip content={linkScale ? "Unlink scales" : "Link scales"}>
              <button
                type="button"
                className={`insp-link-btn${linkScale ? " active" : ""}`}
                disabled={track.locked}
                onClick={() => setLinkScale((v) => !v)}
              >
                <Link2 size={14} strokeWidth={1.75} />
              </button>
            </Tooltip>
            <NumCell
              label="Scale Y"
              value={transform.scale_y}
              min={0.01}
              disabled={track.locked}
              onChange={(scale_y) => {
                if (linkScale) {
                  setTransform({ ...transform, scale_x: scale_y, scale_y });
                } else {
                  setTransform({ ...transform, scale_y });
                }
              }}
              onCommit={(scale_y) => {
                const next = linkScale
                  ? { ...transform, scale_x: scale_y, scale_y }
                  : { ...transform, scale_y };
                void commitTransform(next);
              }}
            />
          </div>
          <div className="insp-grid-2">
            <NumCell
              label="Rotate °"
              value={transform.rotation_deg}
              step={1}
              disabled={track.locked}
              onChange={(rotation_deg) => setTransform({ ...transform, rotation_deg })}
              onCommit={(rotation_deg) => void commitTransform({ ...transform, rotation_deg })}
            />
            <label className="insp-cell">
              <span>Opacity · {Math.round(transform.opacity * 100)}%</span>
              <input
                type="range"
                min={0}
                max={1}
                step={0.01}
                value={transform.opacity}
                disabled={track.locked}
                onChange={(e) =>
                  setTransform({ ...transform, opacity: parseFloat(e.target.value) })
                }
                onMouseUp={(e) => {
                  const opacity = parseFloat((e.target as HTMLInputElement).value);
                  void commitTransform({ ...transform, opacity });
                }}
                onTouchEnd={(e) => {
                  const opacity = parseFloat((e.target as HTMLInputElement).value);
                  void commitTransform({ ...transform, opacity });
                }}
              />
            </label>
          </div>
        </div>
      )}

      </>
      )}

      {activeTab === "speed" && (
        <div className="inspector-section">
          <h3>Speed</h3>
          <label className="insp-cell">
            <span>Speed · {speed.toFixed(2)}×</span>
            <input
              type="range"
              min={0.25}
              max={4}
              step={0.05}
              value={speed}
              disabled={track.locked}
              onChange={(e) => setSpeed(parseFloat(e.target.value))}
              onMouseUp={() => void dispatch(setClipSpeed(track.id, clip.id, speed))}
              onTouchEnd={() => void dispatch(setClipSpeed(track.id, clip.id, speed))}
            />
          </label>
          <p className="empty-hint">
            Clip duration scales inversely with speed; audio is pitch-corrected on export.
          </p>
        </div>
      )}

      {activeTab === "effects" && (
      <>
      {showTransform && project && (
        <MulticamSection project={project} clip={clip.type === "video" ? clip : null} />
      )}

      <details className="insp-disclosure" open>
        <summary>Keyframes</summary>
        <KeyframeEditor track={track} clip={clip} nested />
      </details>

      <EffectStackSection track={track} clip={clip} />

      {showTransform && <MaskSection track={track} clip={clip} />}

      {showTransform && (
        <details className="insp-disclosure">
          <summary>Background &amp; motion</summary>
          <div className="inspector-section insp-disclosure-body">
            <button
              type="button"
              disabled={track.locked}
              onClick={() =>
                void dispatch(
                  setClipBackgroundRemoval(track.id, clip.id, {
                    enabled: true,
                    model_id: "heuristic",
                    threshold: 0.15,
                    feather: 0.1,
                    temporal: false,
                  }),
                )
              }
            >
              Enable BG removal
            </button>
            <button
              type="button"
              disabled={track.locked}
              onClick={() => void dispatch(generateBackgroundMatte(track.id, clip.id))}
            >
              Bake matte
            </button>
            <button
              type="button"
              disabled={track.locked}
              onClick={() => void dispatch(setClipBackgroundRemoval(track.id, clip.id, null))}
            >
              Clear BG removal
            </button>
            <button
              type="button"
              disabled={track.locked}
              onClick={() => void dispatch(stabilizeClip(track.id, clip.id))}
            >
              Stabilize
            </button>
            <button
              type="button"
              disabled={track.locked}
              onClick={() => void dispatch(trackMotion(track.id, clip.id, 12))}
            >
              Track motion
            </button>
            <button
              type="button"
              disabled={track.locked}
              onClick={() =>
                void dispatch(
                  autoReframeClip(
                    track.id,
                    clip.id,
                    (project?.settings.width ?? 1080) / (project?.settings.height ?? 1920),
                  ),
                )
              }
            >
              Auto-reframe
            </button>
          </div>
        </details>
      )}
      </>
      )}

      {activeTab === "audio" && (
      <>
      {showAudio && (
        <details className="insp-disclosure" open>
          <summary>Denoise</summary>
          <div className="inspector-section insp-disclosure-body">
            {!clip.audio_denoise && (
              <button
                type="button"
                disabled={track.locked || track.kind !== "audio"}
                onClick={() =>
                  void dispatch(
                    setClipAudioDenoise(track.id, clip.id, {
                      enabled: true,
                      backend: DEFAULT_DENOISE_BACKEND,
                      strength: 0.8,
                    }),
                  )
                }
              >
                Enable denoise
              </button>
            )}
            {clip.audio_denoise && (
              <>
                <label className="insp-cell">
                  <span>Method</span>
                  <MenuSelect
                    value={clip.audio_denoise.backend ?? "afftdn"}
                    disabled={track.locked}
                    options={DENOISE_BACKENDS}
                    onChange={(backend) =>
                      void dispatch(
                        setClipAudioDenoise(track.id, clip.id, {
                          ...clip.audio_denoise,
                          backend,
                          strength: denoiseStrength,
                        }),
                      )
                    }
                  />
                </label>
                <label className="insp-cell">
                  <span>Strength · {Math.round(denoiseStrength * 100)}%</span>
                  <input
                    type="range"
                    min={0}
                    max={1}
                    step={0.05}
                    value={denoiseStrength}
                    disabled={track.locked}
                    onChange={(e) => setDenoiseStrength(parseFloat(e.target.value))}
                    onMouseUp={commitDenoiseStrength}
                    onTouchEnd={commitDenoiseStrength}
                    onKeyUp={commitDenoiseStrength}
                  />
                </label>
              </>
            )}
            <button
              type="button"
              disabled={track.locked}
              onClick={() => void dispatch(setClipAudioDenoise(track.id, clip.id, null))}
            >
              Clear denoise
            </button>
            <p className="empty-hint">Audio-track clips only (v1).</p>
          </div>
        </details>
      )}

      {showAudio && (
        <div className="inspector-section">
          <h3>Levels</h3>
          <label className="insp-cell">
            <span>Volume · {gain} dB</span>
            <input
              type="range"
              min={-24}
              max={12}
              step={0.5}
              value={gain}
              disabled={track.locked}
              onChange={(e) => setGain(parseFloat(e.target.value))}
              onMouseUp={() => void dispatch(setAudioGain(track.id, clip.id, gain))}
              onTouchEnd={() => void dispatch(setAudioGain(track.id, clip.id, gain))}
            />
          </label>
          <div className="insp-grid-2">
            <NumCell
              label="Fade in"
              value={fadeIn}
              step={0.1}
              min={0}
              disabled={track.locked}
              onChange={setFadeIn}
              onCommit={(v) => {
                setFadeIn(v);
                void dispatch(setAudioFade(track.id, clip.id, v, fadeOut));
              }}
            />
            <NumCell
              label="Fade out"
              value={fadeOut}
              step={0.1}
              min={0}
              disabled={track.locked}
              onChange={setFadeOut}
              onCommit={(v) => {
                setFadeOut(v);
                void dispatch(setAudioFade(track.id, clip.id, fadeIn, v));
              }}
            />
          </div>
          {track.kind === "audio" && (
            <label className="insp-cell">
              <span>Track role</span>
              <MenuSelect
                value={track.audio_role ?? ""}
                disabled={track.locked}
                options={AUDIO_ROLES.map((r) => ({ value: r.value, label: r.label }))}
                onChange={(v) => {
                  void dispatch(
                    setTrackAudioRole(track.id, v === "" ? null : (v as TrackAudioRole)),
                  );
                }}
              />
            </label>
          )}
        </div>
      )}
      </>
      )}

      <div className="inspector-actions">
        <Tooltip content="Delete clip">
          <button
            type="button"
            className="btn-danger"
            disabled={track.locked}
            onClick={async () => {
              await dispatch(deleteClip(track.id, clip.id, false));
              select(null);
            }}
          >
            <Trash2 size={14} strokeWidth={1.75} className="btn-lucide" />
          </button>
        </Tooltip>
      </div>
    </div>
  );
}
