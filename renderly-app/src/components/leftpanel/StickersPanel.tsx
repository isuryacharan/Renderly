import { useEffect, useState } from "react";
import { Music, Sparkles, Sticker } from "lucide-react";
import { addSfxFromPack, addStickerFromPack, addTrack, generateSticker } from "../../lib/commands";
import * as ipc from "../../lib/ipc";
import type { ExtensionCatalog } from "../../lib/ipc";
import { clipDurationSecs, type Track, type TrackKind } from "../../lib/types";
import { activeTracks } from "../../lib/container";
import { useEditorStore } from "../../store/editorStore";

const DEFAULT_STICKER_SECS = 3;

function rangeOverlaps(track: Track, position: number, duration: number): boolean {
  return track.clips.some(
    (c) => position < c.position_secs + clipDurationSecs(c) && c.position_secs < position + duration,
  );
}

export function StickersPanel() {
  const project = useEditorStore((s) => s.project);
  const playhead = useEditorStore((s) => s.playhead);
  const dispatch = useEditorStore((s) => s.dispatch);
  const toast = useEditorStore((s) => s.toast);
  const [catalog, setCatalog] = useState<ExtensionCatalog | null>(null);
  const [stickerPrompt, setStickerPrompt] = useState("");
  const [genBusy, setGenBusy] = useState(false);

  useEffect(() => {
    if (!project) {
      setCatalog(null);
      return;
    }
    void ipc.listExtensions().then(setCatalog).catch(() => setCatalog(null));
  }, [project, project?.asset_pack_paths?.join("|")]);

  const stickers =
    catalog?.packs.flatMap((p) => p.stickers.map((s) => ({ ...s, pack_id: p.id }))) ?? [];
  const sfx = catalog?.packs.flatMap((p) => p.sfx.map((s) => ({ ...s, pack_id: p.id }))) ?? [];

  /**
   * Stickers/SFX are overlays — they must share the playhead with main A/V, so they
   * cannot land on Video 1 / Audio 1 (overlap error). Find an unlocked track of `kind`
   * that is free at [position, position+duration), preferring `baseName`, else create one.
   */
  async function resolveOverlayTrack(
    kind: TrackKind,
    baseName: string,
    position: number,
    duration: number,
  ): Promise<Track | null> {
    const state = useEditorStore.getState();
    const proj = state.project;
    if (!proj) return null;
    const activeId = state.activeSequenceId;

    const unlocked = activeTracks(proj, activeId).filter((t) => t.kind === kind && !t.locked);
    // Only reuse dedicated overlay tracks — never drop onto Video 1 / Audio 1.
    const overlayTracks = unlocked.filter(
      (t) => t.name === baseName || t.name.startsWith(`${baseName} `),
    );
    const free = overlayTracks.find((t) => !rangeOverlaps(t, position, duration));
    if (free) return free;

    const n = overlayTracks.length;
    const name = n === 0 ? baseName : `${baseName} ${n + 1}`;
    const id = crypto.randomUUID();
    const ok = await dispatch(addTrack(kind, name, id, activeId), true);
    if (!ok) {
      toast(`Could not create ${name} track`, "error");
      return null;
    }
    const fresh = useEditorStore.getState();
    const created = activeTracks(fresh.project, fresh.activeSequenceId).find((t) => t.id === id);
    if (!created) {
      toast(`Track ${name} missing after creation`, "error");
      return null;
    }
    return created;
  }

  async function placeSticker(packId: string, stickerId: string, durationSecs: number) {
    const dur = Math.max(0.1, durationSecs || DEFAULT_STICKER_SECS);
    const track = await resolveOverlayTrack("video", "Stickers", playhead, dur);
    if (!track) return;
    await dispatch(addStickerFromPack(packId, stickerId, track.id, playhead));
  }

  async function placeSfx(packId: string, sfxId: string) {
    // Real duration is probed in-core; use a short probe window so we still prefer a
    // free overlay track at the playhead. Worst case the command rejects and the user
    // retries — but with a dedicated SFX track this almost never happens.
    const track = await resolveOverlayTrack("audio", "SFX", playhead, 0.25);
    if (!track) return;
    await dispatch(addSfxFromPack(packId, sfxId, track.id, playhead));
  }

  async function generateAiSticker() {
    if (!stickerPrompt.trim() || !project) return;
    setGenBusy(true);
    try {
      const track = await resolveOverlayTrack("video", "Stickers", playhead, DEFAULT_STICKER_SECS);
      if (!track) return;
      const outputPath = `${project.name.replace(/[^\w-]+/g, "_")}-sticker-${Date.now()}.png`;
      toast("Generating sticker…", "info");
      const ok = await dispatch(
        generateSticker(stickerPrompt.trim(), track.id, playhead, outputPath),
      );
      if (ok) {
        toast("Sticker generated", "success");
        setStickerPrompt("");
      }
    } finally {
      setGenBusy(false);
    }
  }

  if (!project) {
    return (
      <div className="panel-body">
        <h3>Stickers</h3>
        <p className="empty-hint">Open a project to use stickers and SFX packs.</p>
      </div>
    );
  }

  return (
    <div className="panel-body">
      <div className="inspector-section">
        <h3>Generate sticker</h3>
        <p className="empty-hint">Local placeholder art from a text prompt (Phase 4).</p>
        <div className="field">
          <label>Prompt</label>
          <input
            type="text"
            value={stickerPrompt}
            placeholder="e.g. red heart emoji"
            onChange={(e) => setStickerPrompt(e.target.value)}
          />
        </div>
        <button
          type="button"
          className="btn-primary"
          disabled={genBusy || !stickerPrompt.trim()}
          onClick={() => void generateAiSticker()}
        >
          <Sparkles size={14} strokeWidth={1.75} />
          {genBusy ? "Generating…" : "Generate at playhead"}
        </button>
      </div>

      <h3>Pack stickers</h3>
      <p className="empty-hint">Placed at the playhead on a Stickers overlay track.</p>
      {stickers.length === 0 ? (
        <p className="empty-hint">Load a pack with stickers (Extensions tab).</p>
      ) : (
        <div className="effects-catalog">
          {stickers.map((s) => (
            <button
              key={`${s.pack_id}:${s.id}`}
              type="button"
              onClick={() =>
                void placeSticker(s.pack_id, s.id, s.default_duration_secs ?? DEFAULT_STICKER_SECS)
              }
            >
              <Sticker size={14} strokeWidth={1.75} />
              {s.label}
            </button>
          ))}
        </div>
      )}
      <h3 style={{ marginTop: "1.25rem" }}>SFX</h3>
      {sfx.length === 0 ? (
        <p className="empty-hint">Load a pack with SFX.</p>
      ) : (
        <div className="effects-catalog">
          {sfx.map((s) => (
            <button
              key={`${s.pack_id}:${s.id}`}
              type="button"
              onClick={() => void placeSfx(s.pack_id, s.id)}
            >
              <Music size={14} strokeWidth={1.75} />
              {s.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
