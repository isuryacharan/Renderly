// View state: where the user is looking, not what the project is. Playhead, zoom, scroll and
// selection change constantly and are never part of undo or the saved project.

export interface ViewState {
  /** Sequence on screen. */
  sequence: string | null;
  /** Playhead, in flicks. */
  playhead: number;
  /** Timeline zoom: flicks per pixel. */
  zoom: number;
  /** Timeline scroll: the time at the left edge, in flicks, and the track rows' offset, in pixels. */
  scroll: { t: number; y: number };
  /** Selected ids (clips, markers, tracks). */
  selection: readonly string[];
}

export const INITIAL_VIEW: ViewState = {
  sequence: null,
  playhead: 0,
  zoom: 705_600_000 / 100, // 100 pixels a second
  scroll: { t: 0, y: 0 },
  selection: [],
};

export class ViewStore {
  private state: ViewState;
  private readonly listeners = new Set<() => void>();

  constructor(initial: Partial<ViewState> = {}) {
    this.state = { ...INITIAL_VIEW, ...initial };
  }

  get(): ViewState {
    return this.state;
  }

  /** Merges `change` into the view state and tells subscribers when anything differs. */
  set(change: Partial<ViewState>): void {
    const next = { ...this.state, ...change };
    const keys = Object.keys(change) as (keyof ViewState)[];
    if (keys.every((key) => Object.is(next[key], this.state[key]))) return;
    this.state = next;
    for (const listener of this.listeners) listener();
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** Calls `listener` when the selected value changes. */
  select<T>(
    selector: (state: ViewState) => T,
    listener: (value: T) => void,
    equals = Object.is,
  ): () => void {
    let last = selector(this.state);
    return this.subscribe(() => {
      const next = selector(this.state);
      if (!equals(next, last)) {
        last = next;
        listener(next);
      }
    });
  }
}
