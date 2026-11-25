// Probing a file for import: what it holds, whether this browser can decode it, a fingerprint for
// relinking, and a small thumbnail. Reads the container's headers and index, the first 64 KiB
// for the fingerprint, and one frame for the thumbnail. Never the whole file. Runs in a worker
// (probe-worker.ts).

import {
  ALL_FORMATS,
  BlobSource,
  CanvasSink,
  Input,
  UnsupportedInputFormatError,
  type InputAudioTrack,
  type InputVideoTrack,
} from "mediabunny";
import { secondsToFlicks } from "./time";

export type Rotation = 0 | 90 | 180 | 270;

export interface ProbedVideo {
  /** Picture size before rotation: what the encoder stored. */
  width: number;
  height: number;
  /** Measured frames per second (an average for variable-rate files). */
  fps: number;
  codec: string;
  rotation: Rotation;
  /** Whether this browser has a decoder for it. */
  decodable: boolean;
}

export interface ProbedAudio {
  sampleRate: number;
  channels: number;
  codec: string;
  decodable: boolean;
}

export type ProbeResult =
  | {
      ok: true;
      /** From the first frame or sample to the end, in flicks. */
      duration: number;
      video?: ProbedVideo;
      audio?: ProbedAudio;
      /** SHA-256 of the first 64 KiB, for recognising the file when relinking. */
      head: string;
      /** A JPEG of an early frame, at most 320 pixels wide, when the video can be decoded. */
      thumbnail?: Blob;
    }
  | {
      ok: false;
      /** "unreadable": not a container we know; "no-tracks": no video or audio in it. */
      reason: "unreadable" | "no-tracks";
      detail: string;
      head: string;
    };

const HEAD_BYTES = 64 * 1024;

/** `sha256-<base64>` of a file's first 64 KiB. Size and head together identify a file. */
export async function fingerprintHead(file: Blob): Promise<string> {
  const head = await file.slice(0, HEAD_BYTES).arrayBuffer(); // 64 KiB, never the whole file
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", head));
  let binary = "";
  for (const byte of digest) binary += String.fromCharCode(byte);
  return `sha256-${btoa(binary)}`;
}

export interface ProbeOptions {
  /** Make a thumbnail (default true). */
  thumbnail?: boolean;
}

export async function probe(file: Blob, options: ProbeOptions = {}): Promise<ProbeResult> {
  const head = await fingerprintHead(file);
  const input = new Input({ source: new BlobSource(file), formats: ALL_FORMATS });
  try {
    const videoTrack = await input.getPrimaryVideoTrack();
    const audioTrack = await input.getPrimaryAudioTrack();
    if (!videoTrack && !audioTrack)
      return { ok: false, reason: "no-tracks", detail: "The file has no video or audio.", head };
    const first = await input.getFirstTimestamp();
    const end = await input.computeDuration();
    const video = videoTrack ? await describeVideo(videoTrack) : undefined;
    const audio = audioTrack ? await describeAudio(audioTrack) : undefined;
    const thumbnail =
      videoTrack && video?.decodable && options.thumbnail !== false
        ? await makeThumbnail(videoTrack, first, end).catch(() => undefined)
        : undefined;
    return {
      ok: true,
      duration: Math.max(1, secondsToFlicks(end - first)),
      ...(video ? { video } : {}),
      ...(audio ? { audio } : {}),
      head,
      ...(thumbnail ? { thumbnail } : {}),
    };
  } catch (error) {
    if (error instanceof UnsupportedInputFormatError)
      return {
        ok: false,
        reason: "unreadable",
        detail: "This is not a video or audio file Renderly can read.",
        head,
      };
    return {
      ok: false,
      reason: "unreadable",
      detail: error instanceof Error ? error.message : String(error),
      head,
    };
  } finally {
    input.dispose();
  }
}

async function describeVideo(track: InputVideoTrack): Promise<ProbedVideo> {
  const rotation = (await track.getRotation()) as Rotation;
  const displayWidth = await track.getDisplayWidth();
  const displayHeight = await track.getDisplayHeight();
  const sideways = rotation === 90 || rotation === 270;
  const stats = await track.computePacketStats(120);
  return {
    width: sideways ? displayHeight : displayWidth,
    height: sideways ? displayWidth : displayHeight,
    fps: stats.averagePacketRate,
    codec: (await track.getCodec()) ?? "unknown",
    rotation,
    decodable: await track.canDecode(),
  };
}

async function describeAudio(track: InputAudioTrack): Promise<ProbedAudio> {
  return {
    sampleRate: track.sampleRate,
    channels: track.numberOfChannels,
    codec: (await track.getCodec()) ?? "unknown",
    decodable: await track.canDecode(),
  };
}

/** A frame a tenth of the way in (at most 2 s), so it is past any fade from black. */
async function makeThumbnail(
  track: InputVideoTrack,
  first: number,
  end: number,
): Promise<Blob | undefined> {
  const sink = new CanvasSink(track, { width: 320, poolSize: 1 });
  const at = first + Math.min(2, (end - first) / 10);
  const frame = (await sink.getCanvas(at)) ?? (await sink.getCanvas(first));
  if (!frame || !("convertToBlob" in frame.canvas)) return undefined;
  return frame.canvas.convertToBlob({ type: "image/jpeg", quality: 0.8 });
}
