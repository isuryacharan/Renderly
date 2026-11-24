// Chooses which decoded frame to show at a given playhead. Pure: works on anything with a time
// and a duration, so it is tested without WebCodecs.

export interface Timed {
  /** Start of the frame's presentation interval. */
  time: number;
  duration: number;
}

export interface Pick<T> {
  /** The frame showing at the playhead, if it has arrived. */
  frame: T | null;
  /** Older frames that were never shown because the playhead passed them. */
  dropped: T[];
}

/** Frames waiting to be shown, in presentation order. */
export class FrameQueue<T extends Timed> {
  private frames: T[] = [];

  get length(): number {
    return this.frames.length;
  }

  push(frame: T): void {
    this.frames.push(frame);
  }

  /**
   * Removes and returns the latest frame that starts at or before `time`, with the frames before
   * it as dropped. Frames still in the future stay queued.
   */
  take(time: number): Pick<T> {
    let index = -1;
    for (let i = 0; i < this.frames.length; i++) {
      if ((this.frames[i] as T).time <= time) index = i;
      else break;
    }
    if (index < 0) return { frame: null, dropped: [] };
    const taken = this.frames.splice(0, index + 1);
    const frame = taken.pop() ?? null;
    return { frame, dropped: taken };
  }

  /** Removes everything, e.g. on seek or stop. */
  clear(): T[] {
    return this.frames.splice(0);
  }
}
