//! Persistent playback engine — audio-only backend playback + tick emission + scrub-audio
//! service.
//!
//! Video preview is entirely owned by the webview (`renderly-app/src/preview/
//! webviewPreviewEngine.ts` driving `renderly-wasm`'s compositor) since the P4 deletion of
//! the native child-window preview (docs/preview-webview.md) — this module no longer opens
//! a `FrameRenderer`, decodes video, or presents anywhere. It owns the audio/wall clock: a
//! dedicated worker thread pre-mixes and plays the timeline's audio through rodio and emits
//! `playback:tick` (~30 Hz) so the webview's clock can slew to it, plus a paused-state
//! `ScrubWorker` that plays a short audio blip on scrub for feedback (coalesced — only the
//! latest request survives).
//!
//! Frame rendering for non-playback purposes (export, project thumbnails, the MCP live
//! bridge's `render_frame`) is unaffected: those call `renderly_core::FrameRenderer`
//! directly and never went through this module's now-removed render/present path.

use parking_lot::Mutex;
use renderly_core::{
    mix_timeline_audio_range_to_file, mix_timeline_audio_segment, timeline_duration, Project,
};
use rodio::{Decoder, OutputStreamBuilder, Sink};
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// Emitted ~30 Hz while playing, and once on seek/pause/EOF.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlaybackTick {
    pub time_us: i64,
    pub playing: bool,
}

/// Emitted on play start/stop and end-of-timeline.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlaybackStateEvent {
    pub playing: bool,
    pub time_us: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PlaybackErrorEvent {
    pub message: String,
}

const TICK_INTERVAL: Duration = Duration::from_millis(33);

struct PlaySession {
    stop: Arc<AtomicUsize>, // 0 = running, 1 = stop requested
    seek: Arc<Mutex<Option<i64>>>,
    current_time: Arc<Mutex<i64>>,
    /// Shared with the playback thread; `update_live_project` swaps its contents so an
    /// edit made while playing shows up in the audio premix without stopping the sink or
    /// reopening decoders — see `update_live_project`'s doc comment.
    /// Outer `Arc`/`Mutex` for sharing; inner `Arc<Project>` so live swaps and
    /// `project.lock().clone()` snapshots are cheap (B3 — no full Project clone).
    project: Arc<Mutex<Arc<Project>>>,
    thread: JoinHandle<()>,
}

/// A pending scrub-audio request; coalesced so only the latest position survives between
/// worker wakeups.
struct ScrubRequest {
    project: Arc<Project>,
    time_us: i64,
    /// `play_epoch` at submission time — lets the scrub worker detect a `play()` call that
    /// started after this request was queued and skip playing a now-stale blip.
    epoch: u64,
}

pub struct PlaybackEngine {
    session: Mutex<Option<PlaySession>>,
    scrub_pending: Arc<Mutex<Option<ScrubRequest>>>,
    _scrub_thread: JoinHandle<()>,
    play_epoch: Arc<AtomicU64>,
}

impl PlaybackEngine {
    pub fn new() -> Self {
        let scrub_pending: Arc<Mutex<Option<ScrubRequest>>> = Arc::new(Mutex::new(None));
        let worker_pending = Arc::clone(&scrub_pending);
        let play_epoch = Arc::new(AtomicU64::new(0));
        let worker_epoch = Arc::clone(&play_epoch);
        let scrub_thread = thread::spawn(move || run_scrub_worker(worker_pending, worker_epoch));

        Self {
            session: Mutex::new(None),
            scrub_pending,
            _scrub_thread: scrub_thread,
            play_epoch,
        }
    }

    /// Stop playback without reporting a resume time (used on session teardown).
    pub fn stop(&self) {
        // `let session = ...;` as its own statement, NOT `if let Some(session) =
        // self.session.lock().take() { ... }` — Rust extends a temporary created in a
        // match/if-let scrutinee to live for the whole block (see the Reference's
        // "temporary scopes" rules), so the `MutexGuard` from `self.session.lock()` would
        // otherwise stay held for the entire body below, including the blocking
        // `thread.join()` — self-deadlocking any other call (e.g. `play()`'s own
        // `self.stop()`) that needs this same lock while a session is still joining.
        let session = self.session.lock().take();
        if let Some(session) = session {
            session.stop.store(1, Ordering::SeqCst);
            let _ = session.thread.join();
        }
    }

    /// Stop playback and return the time to resume from.
    pub fn pause(&self) -> i64 {
        // See the identical note in `stop()` above — this early `let` is load-bearing,
        // not stylistic.
        let session = self.session.lock().take();
        if let Some(session) = session {
            session.stop.store(1, Ordering::SeqCst);
            let t = *session.current_time.lock();
            let _ = session.thread.join();
            t
        } else {
            0
        }
    }

    /// Whether a play session is currently active (bridge status / HUD).
    pub fn is_playing(&self) -> bool {
        self.session.lock().is_some()
    }

    /// If a play session is active, coalesce a seek into it (the loop picks up the
    /// latest value on its next iteration and restarts audio from there — a newer seek
    /// arriving before the loop observes an older one simply replaces it). Returns `false`
    /// if nothing is playing (paused-state seeks are handled entirely by the webview
    /// canvas's own scrub redraw).
    pub fn seek_while_playing(&self, time_us: i64) -> bool {
        let guard = self.session.lock();
        if let Some(session) = guard.as_ref() {
            *session.seek.lock() = Some(time_us);
            true
        } else {
            false
        }
    }

    /// Live-swap the project for the active play session so the *next* audio chunk
    /// reflects an edit — no pause, no audio-sink teardown. Safe for both property edits
    /// (opacity/gain/effects) and structural ones (split/move/delete). Returns `false` if
    /// nothing is playing (paused-state repaint is entirely the webview canvas's own
    /// store-subscription redraw).
    ///
    /// One caveat: the audio track was pre-mixed once for the whole remaining play range
    /// at the last restart (see `run_playback_loop`), so gain/mute/audio-clip edits won't
    /// be audible until the next natural restart (seek, loop end). Segmented premix
    /// (planned separately) removes this caveat.
    pub fn update_live_project(&self, project: Arc<Project>) -> bool {
        let guard = self.session.lock();
        if let Some(session) = guard.as_ref() {
            *session.project.lock() = project;
            true
        } else {
            false
        }
    }

    pub fn play(&self, app: AppHandle, project: Arc<Project>, start_us: i64) {
        self.stop();
        // Claims this playback generation — see `ScrubRequest`'s `epoch` doc comment.
        self.play_epoch.fetch_add(1, Ordering::SeqCst);

        let duration = timeline_duration(&project);
        let fps = project.settings.fps.as_f64().max(1.0);

        let stop = Arc::new(AtomicUsize::new(0));
        let seek = Arc::new(Mutex::new(None::<i64>));
        let current_time = Arc::new(Mutex::new(start_us.min(duration.max(0))));
        let project = Arc::new(Mutex::new(project));

        let stop_clone = Arc::clone(&stop);
        let seek_clone = Arc::clone(&seek);
        let current_time_clone = Arc::clone(&current_time);
        let project_clone = Arc::clone(&project);
        let start_us = start_us.max(0);

        let thread = thread::spawn(move || {
            run_playback_loop(
                app,
                project_clone,
                start_us,
                duration,
                fps,
                stop_clone,
                seek_clone,
                current_time_clone,
            );
        });

        *self.session.lock() = Some(PlaySession {
            stop,
            seek,
            current_time,
            project,
            thread,
        });
    }

    /// Play a short audio blip at `time_us` (paused-state scrub feedback); coalesced
    /// with any other pending request.
    pub fn request_scrub_audio(&self, project: Arc<Project>, time_us: i64) {
        *self.scrub_pending.lock() = Some(ScrubRequest {
            project,
            time_us,
            epoch: self.play_epoch.load(Ordering::SeqCst),
        });
    }
}

/// Removes its directory on drop — covers every exit path from the scope it's declared
/// in (early `return`, `continue 'restart`, *and* a panic unwinding through), unlike a
/// manual `remove_dir_all` call repeated at each individual exit point.
struct TempDirGuard(std::path::PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[allow(clippy::too_many_arguments)]
fn run_playback_loop(
    app: AppHandle,
    project: Arc<Mutex<Arc<Project>>>,
    mut start_us: i64,
    duration: i64,
    fps: f64,
    stop: Arc<AtomicUsize>,
    seek: Arc<Mutex<Option<i64>>>,
    current_time: Arc<Mutex<i64>>,
) {
    // The webview decodes/presents video itself (see this module's doc comment) — this
    // loop's only job is audio pacing (premix + rodio sink) and `playback:tick` emission.
    let finish = |t: i64| {
        *current_time.lock() = t;
        let _ = app.emit(
            "playback:tick",
            PlaybackTick {
                time_us: t,
                playing: false,
            },
        );
        let _ = app.emit(
            "playback:state",
            PlaybackStateEvent {
                playing: false,
                time_us: t,
            },
        );
    };

    'restart: loop {
        if stop.load(Ordering::SeqCst) != 0 {
            finish(start_us);
            return;
        }
        if start_us >= duration {
            finish(duration.max(0));
            return;
        }

        // A6: premix audio in ~5s chunks so playback can start after the first chunk
        // (~100–300 ms) instead of waiting on the entire remaining timeline. Chunk 0 is
        // mixed on this thread; later chunks are mixed on a background worker and
        // appended to the same rodio Sink (still the master clock via get_pos()).
        const AUDIO_CHUNK_US: i64 = 5_000_000;
        let audio_dir =
            std::env::temp_dir().join(format!("renderly-playback-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&audio_dir);
        // RAII cleanup instead of a manual `remove_dir_all` at every exit point below: it
        // also covers a panic unwinding through this scope, which none of the manual call
        // sites could.
        let _audio_dir_guard = TempDirGuard(audio_dir.clone());

        let remaining = (duration - start_us).max(0);
        let first_chunk = remaining.min(AUDIO_CHUNK_US);
        let first_wav = audio_dir.join("audio_0.wav");
        let project_snap = project.lock().clone();
        let has_audio = match mix_timeline_audio_range_to_file(
            &project_snap,
            start_us,
            first_chunk,
            &first_wav,
        ) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("playback: audio pre-mix failed: {e}");
                let _ = app.emit(
                    "playback:error",
                    PlaybackErrorEvent {
                        message: format!("Audio mix failed: {e}"),
                    },
                );
                false
            }
        };

        let stream = OutputStreamBuilder::open_default_stream().ok();
        let sink = stream.as_ref().map(|s| Sink::connect_new(s.mixer()));
        if has_audio {
            if let Some(sink) = &sink {
                if let Ok(file) = File::open(&first_wav) {
                    if let Ok(decoder) = Decoder::new_wav(BufReader::new(file)) {
                        sink.append(decoder);
                    }
                }
            }
        }

        // Background premix for chunks after the first. Cancelled when this restart
        // scope ends (seek / stop / end) by dropping `audio_rx` so `send` fails.
        let (audio_tx, audio_rx) = std::sync::mpsc::channel::<std::path::PathBuf>();
        let audio_worker = if has_audio && remaining > first_chunk {
            let dir = audio_dir.clone();
            let stop_flag = Arc::clone(&stop);
            Some(thread::spawn(move || {
                let mut chunk_start = start_us + first_chunk;
                let mut idx = 1u32;
                while chunk_start < duration - 1 {
                    if stop_flag.load(Ordering::SeqCst) != 0 {
                        break;
                    }
                    let chunk_len = (duration - chunk_start).min(AUDIO_CHUNK_US);
                    let path = dir.join(format!("audio_{idx}.wav"));
                    match mix_timeline_audio_range_to_file(
                        &project_snap,
                        chunk_start,
                        chunk_len,
                        &path,
                    ) {
                        Ok(true) => {
                            if audio_tx.send(path).is_err() {
                                break; // playback restarted or stopped
                            }
                        }
                        Ok(false) => break,
                        Err(e) => {
                            eprintln!("playback: audio chunk {idx} premix failed: {e}");
                            break;
                        }
                    }
                    chunk_start += chunk_len;
                    idx += 1;
                }
            }))
        } else {
            None
        };

        let wall_clock_start = Instant::now();
        let mut last_tick_emit = Instant::now() - TICK_INTERVAL;
        let frame_period = Duration::from_secs_f64(1.0 / fps);

        loop {
            if stop.load(Ordering::SeqCst) != 0 {
                if let Some(sink) = &sink {
                    sink.stop();
                }
                drop(audio_rx);
                if let Some(h) = audio_worker {
                    let _ = h.join();
                }
                finish(*current_time.lock());
                return;
            }

            // Drain any finished background audio chunks onto the sink.
            if let Some(sink) = &sink {
                while let Ok(path) = audio_rx.try_recv() {
                    if let Ok(file) = File::open(&path) {
                        if let Ok(decoder) = Decoder::new_wav(BufReader::new(file)) {
                            sink.append(decoder);
                        }
                    }
                }
            }

            // `seek.lock()` as its own statement, NOT the `if let` scrutinee directly —
            // same reasoning as `stop()`/`pause()`'s identically-shaped fix above: an
            // if-let scrutinee's temporary is extended to live for the whole block, which
            // would hold this `MutexGuard` across `sink.stop()`/`app.emit` (blocking I/O)
            // for no reason. Milder than the original bug (no reverse-lock-order deadlock
            // here — confirmed no other holder of `seek` ever tries to reacquire anything
            // this loop holds) but the same footgun shape.
            let pending_seek = seek.lock().take();
            if let Some(new_time) = pending_seek {
                if let Some(sink) = &sink {
                    sink.stop();
                }
                drop(audio_rx);
                if let Some(h) = audio_worker {
                    let _ = h.join();
                }
                start_us = new_time.clamp(0, duration.max(0));
                let _ = app.emit(
                    "playback:tick",
                    PlaybackTick {
                        time_us: start_us,
                        playing: true,
                    },
                );
                continue 'restart;
            }

            // rodio's `Sink::get_pos()` quantizes to buffer boundaries; the wall-clock
            // fallback (no audio) is a plain monotonic clock instead.
            let elapsed = if let (Some(sink), true) = (&sink, has_audio) {
                sink.get_pos().as_secs_f64()
            } else {
                wall_clock_start.elapsed().as_secs_f64()
            };
            let t = start_us + (elapsed * 1_000_000.0).round() as i64;

            if t >= duration {
                if let Some(sink) = &sink {
                    sink.stop();
                }
                drop(audio_rx);
                if let Some(h) = audio_worker {
                    let _ = h.join();
                }
                finish(duration.max(0));
                return;
            }

            *current_time.lock() = t;

            if last_tick_emit.elapsed() >= TICK_INTERVAL {
                let _ = app.emit(
                    "playback:tick",
                    PlaybackTick {
                        time_us: t,
                        playing: true,
                    },
                );
                last_tick_emit = Instant::now();
            }

            thread::sleep(frame_period);
        }
    }
}

fn run_scrub_worker(pending: Arc<Mutex<Option<ScrubRequest>>>, play_epoch: Arc<AtomicU64>) {
    loop {
        let request = pending.lock().take();
        let Some(req) = request else {
            thread::sleep(Duration::from_millis(8));
            continue;
        };

        // A `play()` call landed after this request was submitted — skip it entirely
        // rather than playing a blip nobody wants anymore.
        if req.epoch != play_epoch.load(Ordering::SeqCst) {
            continue;
        }

        if let Ok(wav) = mix_timeline_audio_segment(&req.project, req.time_us, 80_000) {
            if !wav.is_empty() {
                // Detached: plays out on its own thread so the scrub worker is free to
                // pick up the next coalesced request immediately.
                thread::spawn(move || {
                    if let Ok(stream) = OutputStreamBuilder::open_default_stream() {
                        let sink = Sink::connect_new(stream.mixer());
                        if let Ok(decoder) = Decoder::new_wav(Cursor::new(wav)) {
                            sink.append(decoder);
                            sink.sleep_until_end();
                        }
                    }
                });
            }
        }
    }
}
