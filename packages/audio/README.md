# @renderly/audio

Audio playback, and the clock the picture follows. Audio is decoded in a worker, written into a
`SharedArrayBuffer` ring, and played by an AudioWorklet that publishes its position.

Status: M0 spike (slice S0.4). One audio track, 48 kHz sources only (no resampler yet), mono or
stereo out. Mixing, effects and other rates come with the real audio engine (M2).

## Pieces

```
decode worker ──PCM──► ring (SharedArrayBuffer) ──► AudioWorklet ──► speakers
                                                        │
                                       clock (SharedArrayBuffer): frames consumed,
                                       context frame; output-time mapping from main
                                                        │
                                                        ▼
                                  render worker: picks the frame for the heard time
```

- `audio-decoder.ts` decodes from a source time with mediabunny (`AudioDecoder` underneath) and
  writes into the ring sample-exact: the first sample written is the one at the requested time,
  and gaps become silence. Source time 0 is the first video frame, as in `@renderly/media`.
- `ring.ts` is a single-producer, single-consumer ring of planar float audio.
- `playback-core.ts` is what the worklet does each render quantum: read the ring, apply 5 ms
  gain ramps on every start and stop, publish the clock. `processor.ts` adapts it to the worklet.
- `clock.ts` lays out the shared clock and computes the heard time.
- `output.ts` runs on the main thread: it owns the `AudioContext` (workers cannot), forwards play
  and stop, and every 100 ms writes `getOutputTimestamp()` into the clock.

## The heard time

The worklet publishes, after each 128-frame quantum, the context frame at the end of the quantum
and how many session frames it has consumed. `getOutputTimestamp()` says which context time is
reaching the speakers at which wall-clock time. Frames consumed but not yet at the speakers are
subtracted, which gives the session time being heard at any wall-clock time
(`heardSessionTime`). The render worker evaluates it for the moment its next refresh reaches the
screen, assumed to be one refresh period ahead.

## Starting together

Each play is a session with a fresh ring. The worklet holds a session until the ring has 100 ms
buffered and the render worker sets the session's go flag, which it does once its video pre-roll
is ready. Both then start from the same point. A session started while another is playing waits
for the old one to fade out.

## Rules

- Never allocate or post messages per quantum in the worklet; it runs on the audio thread.
- Pausing snaps to the frame on screen (the engine does this); resuming starts from there.
- Starts and stops always ramp. `ClockBuffer.diagnostics()` reports the largest sample-to-sample
  step in steady play and around starts and stops, to catch clicks.
