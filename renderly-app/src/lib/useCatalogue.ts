import { useEffect, useMemo } from "react";
import { effectStack, type Clip } from "./types";
import type { CatalogueEntry, EffectStatusEntry } from "./ipc";
import { useCatalogueStore } from "../store/catalogueStore";
import { useEditorStore } from "../store/editorStore";

/** The engine's effect catalogue; fetched when a project is open and refetched when it changes. */
export function useCatalogue(): CatalogueEntry[] {
  const hasProject = useEditorStore((s) => s.project != null);
  const projectId = useEditorStore((s) => s.project?.id);
  const entries = useCatalogueStore((s) => s.entries);
  useEffect(() => {
    if (hasProject) void useCatalogueStore.getState().refresh();
  }, [hasProject, projectId]);
  return entries;
}

export function usePlugins() {
  useCatalogue();
  return useCatalogueStore((s) => s.plugins);
}

/** The transition a clip carries, whichever kind of clip it is. */
function transitionOf(clip: Clip | null | undefined) {
  return clip && (clip.type === "video" || clip.type === "audio" || clip.type === "text")
    ? (clip.outgoing_transition ?? null)
    : null;
}

/** Status of a clip's effect instances (by instance id) and its transition (key `transition`),
 *  for the non-`ok` ones. A text clip's style status (an entry with neither an instance id nor
 *  the transition's ref) is under key `style`. Refetched when the clip's refs or hashes change
 *  or the library does. */
export function useEffectStatus(clip: Clip | null | undefined): Map<string, EffectStatusEntry["status"]> {
  const status = useCatalogueStore((s) => s.status);
  const epoch = useCatalogueStore((s) => s.epoch);
  const transitionRef = transitionOf(clip)?.ref ?? "";
  const sig = clip
    ? [
        ...effectStack(clip).map((e) => `${e.id}:${e.ref}:${e.hash ?? ""}`),
        `t:${transitionRef}:${transitionOf(clip)?.hash ?? ""}`,
        clip.type === "text" ? `s:${clip.style.ref}` : "",
      ].join("|")
    : "";
  useEffect(() => {
    if (clip) void useCatalogueStore.getState().refreshStatus();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sig, epoch]);
  const clipId = clip?.id;
  return useMemo(() => {
    const m = new Map<string, EffectStatusEntry["status"]>();
    for (const e of status) {
      if (e.clip_id !== clipId) continue;
      const key = e.instance_id ?? (transitionRef && e.ref === transitionRef ? "transition" : "style");
      m.set(key, e.status);
    }
    return m;
  }, [status, clipId, transitionRef]);
}

/**
 * Status of a style ref (`builtin:...`/`pack:...`) held by `ownerId`: a caption or text clip, or
 * a caption track for its `caption_style` (core reports those with the track id as `clip_id`
 * and no instance id). `undefined` when the style is fine (or `styleRef` is absent).
 */
export function useStyleStatus(
  ownerId: string | undefined,
  styleRef: string | undefined,
): EffectStatusEntry["status"] | undefined {
  const status = useCatalogueStore((s) => s.status);
  const epoch = useCatalogueStore((s) => s.epoch);
  useEffect(() => {
    if (ownerId && styleRef) void useCatalogueStore.getState().refreshStatus();
  }, [ownerId, styleRef, epoch]);
  if (!ownerId || !styleRef) return undefined;
  return status.find((e) => e.clip_id === ownerId && !e.instance_id && e.ref === styleRef)?.status;
}
