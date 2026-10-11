//! RNNoise-class audio denoise (`AudioDenoise.backend == "rnnoise"`), backed by the
//! pure-Rust `nnnoiseless` port of Xiph's RNNoise (BSD-3-Clause, weights built in).
//!
//! Two layers:
//! - [`StreamDenoiser`] / [`denoise_interleaved`]: the DSP. RNNoise runs on 48 kHz mono
//!   frames of 480 samples scaled to i16 range, with one frame (10 ms) of algorithmic
//!   latency. Every channel gets its own `DenoiseState`; the latency is compensated (the
//!   first, fade-in-contaminated output frame is the latency frame and is dropped, and the
//!   tail is flushed with silence) so output is sample-aligned with, and exactly as long as,
//!   the input. `strength` is a wet/dry mix of the denoised and the original signal
//!   (1.0 = fully denoised, 0.0 = bit-exact passthrough) — deterministic, no VAD gating.
//! - [`denoised_wav_cached`]: the mix integration. The export/playback mix is an FFmpeg
//!   filter graph over source files, so a denoised clip is materialised as a 48 kHz float
//!   WAV in a cache directory and substituted for the clip's source in the mix. The
//!   **whole source** is processed (not just the used range): the result then stays valid
//!   across trims and across the 5 s playback pre-mix chunks (which each ask for a different
//!   sub-range), RNNoise never restarts mid-clip (no per-chunk warm-up artefacts), and the
//!   cost is one streaming pass that is far faster than realtime.

use std::collections::hash_map::DefaultHasher;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use nnnoiseless::DenoiseState;
use thiserror::Error;

use crate::media::FfmpegCliError;

/// Sample rate RNNoise operates at.
pub const RNNOISE_SAMPLE_RATE: u32 = 48_000;
/// Channels the cache WAV is decoded to (the mix is stereo anyway).
const CACHE_CHANNELS: usize = 2;
/// Bump to invalidate every cache entry when the DSP output changes.
const CACHE_VERSION: u32 = 1;
/// Cache entries untouched for this long are pruned when a new entry is written.
const CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

const FRAME: usize = DenoiseState::FRAME_SIZE;
const I16_SCALE: f32 = 32768.0;

#[derive(Debug, Error)]
pub enum DenoiseError {
    #[error("invalid denoise input: {0}")]
    InvalidInput(String),
    #[error(transparent)]
    Ffmpeg(#[from] FfmpegCliError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

struct Channel {
    state: Box<DenoiseState<'static>>,
    /// Input samples (i16 scale) not yet forming a full frame.
    pending: Vec<f32>,
    /// The previous input frame: RNNoise output lags input by one frame, so this is the
    /// dry signal that the current wet frame lines up with.
    prev_dry: Option<[f32; FRAME]>,
}

/// Streaming multi-channel RNNoise processor with latency compensation.
///
/// Feed interleaved 48 kHz `f32` samples (nominal range -1..1) with [`push`](Self::push);
/// call [`finish`](Self::finish) once at the end. Total output length equals total input.
pub struct StreamDenoiser {
    channels: Vec<Channel>,
    strength: f32,
    frames_in: u64,
    frames_out: u64,
}

impl StreamDenoiser {
    pub fn new(channels: usize, strength: f64) -> Result<Self, DenoiseError> {
        if channels == 0 {
            return Err(DenoiseError::InvalidInput("zero channels".into()));
        }
        let strength = if strength.is_finite() {
            strength.clamp(0.0, 1.0) as f32
        } else {
            1.0
        };
        Ok(Self {
            channels: (0..channels)
                .map(|_| Channel {
                    state: DenoiseState::new(),
                    pending: Vec::new(),
                    prev_dry: None,
                })
                .collect(),
            strength,
            frames_in: 0,
            frames_out: 0,
        })
    }

    /// Process `interleaved` (length must be a multiple of the channel count), appending
    /// whatever output is ready (interleaved, delayed by one frame) to `out`.
    pub fn push(&mut self, interleaved: &[f32], out: &mut Vec<f32>) -> Result<(), DenoiseError> {
        let n = self.channels.len();
        if !interleaved.len().is_multiple_of(n) {
            return Err(DenoiseError::InvalidInput(
                "sample count is not a multiple of the channel count".into(),
            ));
        }
        for (c, ch) in self.channels.iter_mut().enumerate() {
            ch.pending
                .extend(interleaved.iter().skip(c).step_by(n).map(|s| s * I16_SCALE));
        }
        self.frames_in += (interleaved.len() / n) as u64;
        self.drain(out);
        Ok(())
    }

    /// Flush the tail (zero-padded) and trim the output to exactly the input length.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        let n = self.channels.len();
        let partial = self.channels[0].pending.len();
        if partial > 0 {
            for ch in &mut self.channels {
                ch.pending.resize(FRAME, 0.0);
            }
            self.drain(out);
        }
        // One more silent frame pushes the last real frame out of the one-frame delay.
        for ch in &mut self.channels {
            ch.pending.resize(FRAME, 0.0);
        }
        self.drain(out);
        let excess = self.frames_out.saturating_sub(self.frames_in) as usize;
        out.truncate(out.len().saturating_sub(excess * n));
        self.frames_out = self.frames_in;
    }

    fn drain(&mut self, out: &mut Vec<f32>) {
        let n = self.channels.len();
        let s = self.strength;
        while self.channels[0].pending.len() >= FRAME {
            let mut emitted: Vec<[f32; FRAME]> = Vec::with_capacity(n);
            for ch in &mut self.channels {
                let mut frame = [0.0_f32; FRAME];
                frame.copy_from_slice(&ch.pending[..FRAME]);
                ch.pending.drain(..FRAME);
                let mut wet = [0.0_f32; FRAME];
                ch.state.process_frame(&mut wet, &frame);
                // The first call's output is the latency/fade-in frame: discard it.
                if let Some(dry) = ch.prev_dry {
                    let mut mixed = [0.0_f32; FRAME];
                    for i in 0..FRAME {
                        let w = wet[i].clamp(-I16_SCALE, I16_SCALE - 1.0);
                        mixed[i] = (dry[i] + s * (w - dry[i])) / I16_SCALE;
                    }
                    emitted.push(mixed);
                }
                ch.prev_dry = Some(frame);
            }
            if emitted.len() == n {
                for i in 0..FRAME {
                    for e in &emitted {
                        out.push(e[i]);
                    }
                }
                self.frames_out += FRAME as u64;
            }
        }
    }
}

/// Denoise a whole interleaved 48 kHz buffer. Output has the same length and alignment as
/// the input. `strength` 0..=1 is the wet/dry mix (1.0 = fully denoised).
pub fn denoise_interleaved(
    samples: &[f32],
    channels: usize,
    strength: f64,
) -> Result<Vec<f32>, DenoiseError> {
    let mut d = StreamDenoiser::new(channels, strength)?;
    let mut out = Vec::with_capacity(samples.len() + FRAME * channels);
    d.push(samples, &mut out)?;
    d.finish(&mut out);
    Ok(out)
}

// --- cache -------------------------------------------------------------------------------

/// Default cache location: next to the other transient caches (matte caches, playback
/// pre-mix), under the OS temp directory — never beside the user's media.
pub fn cache_dir() -> PathBuf {
    std::env::temp_dir().join("renderly-denoise-cache")
}

/// Stable-per-toolchain key over source path + mtime + size + backend + strength.
pub fn cache_key(source: &Path, strength: f64) -> Result<String, DenoiseError> {
    let meta = std::fs::metadata(source)?;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let strength_milli = (strength.clamp(0.0, 1.0) * 1000.0).round() as u32;
    let mut h = DefaultHasher::new();
    CACHE_VERSION.hash(&mut h);
    source.to_string_lossy().hash(&mut h);
    mtime_ns.hash(&mut h);
    meta.len().hash(&mut h);
    "rnnoise".hash(&mut h);
    strength_milli.hash(&mut h);
    Ok(format!("rnnoise-{:016x}", h.finish()))
}

static COMPUTE_LOCK: Mutex<()> = Mutex::new(());

/// Path of the denoised, 48 kHz stereo float WAV of `source`, creating it if needed.
pub fn denoised_wav_cached(source: &Path, strength: f64) -> Result<PathBuf, DenoiseError> {
    denoised_wav_in(&cache_dir(), source, strength).map(|(p, _)| p)
}

/// As [`denoised_wav_cached`] with an explicit cache directory; also reports whether the
/// file was freshly computed (`true`) or reused (`false`).
pub fn denoised_wav_in(
    dir: &Path,
    source: &Path,
    strength: f64,
) -> Result<(PathBuf, bool), DenoiseError> {
    let key = cache_key(source, strength)?;
    let target = dir.join(format!("{key}.wav"));
    if target.is_file() {
        return Ok((target, false));
    }
    // Serialise: the playback pre-mix worker and the main thread may ask for the same clip.
    let _guard = COMPUTE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if target.is_file() {
        return Ok((target, false));
    }
    std::fs::create_dir_all(dir)?;
    prune_old(dir);
    let tmp = dir.join(format!("{key}.{}.tmp", uuid::Uuid::new_v4()));
    if let Err(e) = render_to_wav(source, strength, &tmp) {
        std::fs::remove_file(&tmp).ok();
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, &target) {
        std::fs::remove_file(&tmp).ok();
        // Another process won the race: its file is just as good.
        if !target.is_file() {
            return Err(e.into());
        }
        return Ok((target, false));
    }
    Ok((target, true))
}

fn prune_old(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > CACHE_MAX_AGE);
        if old {
            std::fs::remove_file(entry.path()).ok();
        }
    }
}

/// Decode `source` with FFmpeg (resampled to 48 kHz stereo f32), run RNNoise streaming, and
/// write the result as an IEEE-float WAV.
fn render_to_wav(source: &Path, strength: f64, out_path: &Path) -> Result<(), DenoiseError> {
    let mut child = crate::proc::command(crate::media::ffmpeg_path()?)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(source)
        .args([
            "-vn",
            "-map",
            "0:a:0",
            "-f",
            "f32le",
            "-ac",
            &CACHE_CHANNELS.to_string(),
            "-ar",
            &RNNOISE_SAMPLE_RATE.to_string(),
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| FfmpegCliError::SpawnFailed {
            tool: "ffmpeg",
            message: e.to_string(),
        })?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| FfmpegCliError::BadOutput("ffmpeg stdout unavailable".into()))?;

    let mut writer = BufWriter::new(File::create(out_path)?);
    write_wav_header(&mut writer, CACHE_CHANNELS as u16, 0)?;
    let mut denoiser = StreamDenoiser::new(CACHE_CHANNELS, strength)?;
    let mut data_bytes: u64 = 0;

    let stride = CACHE_CHANNELS * 4;
    let mut buf = vec![0u8; stride * 16_384];
    let mut filled = 0usize;
    let mut samples = Vec::new();
    let mut out = Vec::new();
    loop {
        let n = stdout.read(&mut buf[filled..])?;
        filled += n;
        let eof = n == 0;
        let usable = filled - filled % stride;
        if usable > 0 {
            samples.clear();
            samples.extend(
                buf[..usable]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b)),
            );
            out.clear();
            denoiser.push(&samples, &mut out)?;
            data_bytes += write_f32(&mut writer, &out)?;
            buf.copy_within(usable..filled, 0);
            filled -= usable;
        }
        if eof {
            break;
        }
    }
    out.clear();
    denoiser.finish(&mut out);
    data_bytes += write_f32(&mut writer, &out)?;

    let status = child.wait()?;
    if !status.success() {
        return Err(FfmpegCliError::NonZeroExit(status.code().unwrap_or(-1)).into());
    }
    writer.seek(SeekFrom::Start(0))?;
    write_wav_header(&mut writer, CACHE_CHANNELS as u16, data_bytes)?;
    writer.flush()?;
    Ok(())
}

fn write_f32<W: Write>(w: &mut W, samples: &[f32]) -> std::io::Result<u64> {
    for s in samples {
        w.write_all(&s.to_le_bytes())?;
    }
    Ok(samples.len() as u64 * 4)
}

/// 44-byte RIFF/WAVE header for 32-bit IEEE-float PCM at 48 kHz.
fn write_wav_header<W: Write>(w: &mut W, channels: u16, data_bytes: u64) -> std::io::Result<()> {
    let data = data_bytes.min(u32::MAX as u64 - 36) as u32;
    let block_align = channels * 4;
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&3u16.to_le_bytes())?; // WAVE_FORMAT_IEEE_FLOAT
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&RNNOISE_SAMPLE_RATE.to_le_bytes())?;
    w.write_all(&(RNNOISE_SAMPLE_RATE * u32::from(block_align)).to_le_bytes())?;
    w.write_all(&block_align.to_le_bytes())?;
    w.write_all(&32u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data.to_le_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = RNNOISE_SAMPLE_RATE as f32;

    struct Rng(u64);
    impl Rng {
        fn next_f32(&mut self) -> f32 {
            // xorshift64*, mapped to -1..1
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            let v = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40;
            v as f32 / (1u64 << 23) as f32 - 1.0
        }
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
    }
    fn db(x: f64) -> f64 {
        20.0 * x.max(1e-12).log10()
    }

    fn white(n: usize, amp: f32, seed: u64) -> Vec<f32> {
        let mut r = Rng(seed);
        (0..n).map(|_| r.next_f32() * amp).collect()
    }

    /// Paul Kellet's economy pink-noise filter over white noise, normalised to ~`amp` RMS.
    fn pink(n: usize, amp: f32, seed: u64) -> Vec<f32> {
        let mut r = Rng(seed);
        let (mut b0, mut b1, mut b2) = (0.0_f32, 0.0_f32, 0.0_f32);
        let raw: Vec<f32> = (0..n)
            .map(|_| {
                let w = r.next_f32();
                b0 = 0.99765 * b0 + w * 0.0990460;
                b1 = 0.96300 * b1 + w * 0.2965164;
                b2 = 0.57000 * b2 + w * 1.0526913;
                b0 + b1 + b2 + w * 0.1848
            })
            .collect();
        let g = amp / rms(&raw).max(1e-9) as f32;
        raw.iter().map(|v| v * g).collect()
    }

    /// One-pole low-passed white noise (rumble-like), normalised to `amp` RMS.
    fn brown(n: usize, amp: f32, seed: u64) -> Vec<f32> {
        let mut z = 0.0_f32;
        let raw: Vec<f32> = white(n, 1.0, seed)
            .iter()
            .map(|v| {
                z = 0.99 * z + 0.01 * v;
                z
            })
            .collect();
        let g = amp / rms(&raw).max(1e-9) as f32;
        raw.iter().map(|v| v * g).collect()
    }

    /// Crude voiced-speech stand-in: 140 Hz fundamental with formant-ish harmonics and a
    /// syllabic (4 Hz) amplitude envelope.
    fn voiced(n: usize, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let t = i as f32 / SR;
                let env = 0.5 + 0.5 * (std::f32::consts::TAU * 4.0 * t).sin().abs();
                let mut s = 0.0;
                for h in 1..=20 {
                    let f = 140.0 * h as f32;
                    let formant = (-((f - 700.0) / 500.0).powi(2)).exp()
                        + 0.6 * (-((f - 1800.0) / 700.0).powi(2)).exp();
                    s += formant * (std::f32::consts::TAU * f * t).sin() / h as f32;
                }
                s * env * amp
            })
            .collect()
    }

    fn interleave(chs: &[&[f32]]) -> Vec<f32> {
        let n = chs[0].len();
        (0..n).flat_map(|i| chs.iter().map(move |c| c[i])).collect()
    }

    #[test]
    fn output_length_and_alignment_match_input() {
        // Mono and stereo, lengths that are not multiples of 480 (and shorter than a frame).
        for &len in &[100usize, 479, 480, 481, 4800 + 123, 48_000 + 7] {
            let x = voiced(len, 0.3);
            let y = denoise_interleaved(&x, 1, 1.0).unwrap();
            assert_eq!(y.len(), len, "mono len {len}");
            let st = interleave(&[&x, &x]);
            let ys = denoise_interleaved(&st, 2, 1.0).unwrap();
            assert_eq!(ys.len(), st.len(), "stereo len {len}");
        }
        // Alignment: a loud harmonic tone passes through; lag 0 must be the best match.
        let len = 48_000;
        let x = voiced(len, 0.5);
        let y = denoise_interleaved(&x, 1, 1.0).unwrap();
        let corr = |lag: i64| -> f64 {
            let mut acc = 0.0;
            for i in 12_000..36_000_i64 {
                acc += f64::from(x[i as usize]) * f64::from(y[(i + lag) as usize]);
            }
            acc
        };
        let best = (-600..=600_i64).step_by(1).max_by(|a, b| {
            corr(*a)
                .partial_cmp(&corr(*b))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        // RNNoise's own filtering (pre-emphasis/pitch filter) leaves at most a sample or
        // two of group delay; the 480-sample frame latency must be fully compensated.
        let best = best.unwrap_or(i64::MAX);
        eprintln!("best alignment lag: {best} samples");
        assert!(best.abs() <= 2, "output misaligned by {best} samples");
    }

    #[test]
    fn channels_are_independent() {
        let a = voiced(24_000, 0.4);
        let b = white(24_000, 0.2, 7);
        let both = denoise_interleaved(&interleave(&[&a, &b]), 2, 1.0).unwrap();
        let only_a = denoise_interleaved(&a, 1, 1.0).unwrap();
        let left: Vec<f32> = both.iter().step_by(2).copied().collect();
        assert_eq!(left, only_a);
    }

    #[test]
    fn noise_alone_is_strongly_attenuated() {
        // RNNoise is trained on real-world noise; perfectly flat, loud, stationary white
        // noise looks too much like fricative speech to it (measured: only a few dB), so
        // the strong-attenuation claim is asserted on pink and low-frequency (brown-ish)
        // noise, and white is only required not to be amplified.
        let brown = brown(48_000 * 3, 0.06, 4);
        for (name, noise, max_db) in [
            ("white", white(48_000 * 3, 0.1, 1), 0.0),
            ("pink", pink(48_000 * 3, 0.06, 2), -3.0),
            ("brown", brown, -20.0),
        ] {
            let y = denoise_interleaved(&noise, 1, 1.0).unwrap();
            // Skip the first 0.5 s while the network settles.
            let drop = db(rms(&y[24_000..])) - db(rms(&noise[24_000..]));
            eprintln!("{name} noise reduction: {drop:.1} dB");
            assert!(drop < max_db, "{name}: only {drop:.1} dB");
        }
    }

    #[test]
    fn noise_only_segments_attenuated_far_more_than_voiced() {
        // 1 s noise | 2 s voiced + noise | 1 s noise.
        let n = 48_000 * 4;
        let noise = white(n, 0.03, 3);
        let mut v = vec![0.0_f32; n];
        let seg = voiced(48_000 * 2, 0.35);
        v[48_000..48_000 * 3].copy_from_slice(&seg);
        let x: Vec<f32> = noise.iter().zip(&v).map(|(a, b)| a + b).collect();
        let y = denoise_interleaved(&x, 1, 1.0).unwrap();
        let tail = 48_000 * 3 + 9_600..n; // after the voiced burst, past RNNoise's hangover
        let noise_drop = db(rms(&y[tail.clone()])) - db(rms(&x[tail]));
        let voiced_drop = db(rms(&y[60_000..132_000])) - db(rms(&x[60_000..132_000]));
        eprintln!("noise-only {noise_drop:.1} dB, voiced segment {voiced_drop:.1} dB");
        assert!(noise_drop < -15.0, "noise-only dropped {noise_drop:.1} dB");
        assert!(
            noise_drop < voiced_drop - 10.0,
            "noise {noise_drop:.1} dB vs voiced {voiced_drop:.1} dB"
        );
    }

    #[test]
    fn strength_zero_is_passthrough_and_mix_is_monotonic() {
        let x: Vec<f32> = brown(48_000 * 2, 0.06, 5);
        let zero = denoise_interleaved(&x, 1, 0.0).unwrap();
        assert_eq!(zero, x, "strength 0 must return the input unchanged");
        let mut last = f64::INFINITY;
        for s in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let y = denoise_interleaved(&x, 1, s).unwrap();
            let level = rms(&y[24_000..]);
            assert!(level <= last + 1e-9, "strength {s}: {level} > {last}");
            last = level;
        }
        assert!(last < rms(&x[24_000..]) * 0.2);
    }

    #[test]
    fn streaming_chunks_match_one_shot() {
        let x = voiced(30_000 + 11, 0.3);
        let whole = denoise_interleaved(&x, 1, 0.8).unwrap();
        let mut d = StreamDenoiser::new(1, 0.8).unwrap();
        let mut out = Vec::new();
        for c in x.chunks(777) {
            d.push(c, &mut out).unwrap();
        }
        d.finish(&mut out);
        assert_eq!(whole, out);
    }

    #[test]
    fn invalid_input_is_rejected() {
        assert!(StreamDenoiser::new(0, 1.0).is_err());
        assert!(denoise_interleaved(&[0.0; 3], 2, 1.0).is_err());
    }

    fn write_test_wav(path: &Path, samples: &[f32]) {
        let mut f = BufWriter::new(File::create(path).unwrap());
        // Reuse our own writer (float, 48k, 2ch) as a quick fixture.
        write_wav_header(&mut f, 2, samples.len() as u64 * 4).unwrap();
        write_f32(&mut f, samples).unwrap();
        f.flush().unwrap();
    }

    #[test]
    fn cache_key_tracks_source_and_strength_and_second_run_hits() {
        if !crate::media::ffmpeg_available() {
            eprintln!("skipping: ffmpeg not available");
            return;
        }
        let dir = std::env::temp_dir().join(format!("renderly-dn-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("cache");
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        let x = white(48_000 * 2, 0.1, 9);
        write_test_wav(&a, &x);
        write_test_wav(&b, &x);

        let ka = cache_key(&a, 1.0).unwrap();
        assert_eq!(ka, cache_key(&a, 1.0).unwrap());
        assert_ne!(ka, cache_key(&a, 0.5).unwrap());
        assert_ne!(ka, cache_key(&b, 1.0).unwrap());

        let (p1, fresh1) = denoised_wav_in(&cache, &a, 1.0).unwrap();
        assert!(fresh1);
        let (p2, fresh2) = denoised_wav_in(&cache, &a, 1.0).unwrap();
        assert!(!fresh2, "second run must hit the cache");
        assert_eq!(p1, p2);
        // 2 s of stereo float + header.
        assert_eq!(std::fs::metadata(&p1).unwrap().len(), 44 + 96_000 * 4);

        // Editing the source (size changes) invalidates.
        write_test_wav(&a, &x[..x.len() - 4000]);
        let (_, fresh3) = denoised_wav_in(&cache, &a, 1.0).unwrap();
        assert!(fresh3);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Decode a WAV to mono-averaged f32 via ffmpeg.
    fn decode_f32(path: &Path) -> Vec<f32> {
        let out = crate::proc::command(crate::media::ffmpeg_path().unwrap())
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(path)
            .args(["-f", "f32le", "-ac", "1", "-"])
            .output()
            .unwrap();
        assert!(out.status.success());
        out.stdout
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect()
    }

    #[test]
    fn mix_with_rnnoise_lowers_noise_floor() {
        use crate::media::{mix_timeline_audio, AudioMixClip};
        use crate::project::AudioDenoise;

        if !crate::media::ffmpeg_available() {
            eprintln!("skipping: ffmpeg not available");
            return;
        }
        let dir = std::env::temp_dir().join(format!("renderly-dn-mix-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // 44.1 kHz brown noise with a tone burst at 1..2 s; 3..4 s is noise only.
        let src = dir.join("noisy.wav");
        let status = crate::proc::command(crate::media::ffmpeg_path().unwrap())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "anoisesrc=color=brown:amplitude=0.3:d=4:r=44100"])
            .args(["-f", "lavfi", "-i", "sine=frequency=300:duration=4:sample_rate=44100"])
            .args([
                "-filter_complex",
                "[1:a]volume=enable='between(t,1,2)':volume=0.5[s];[0:a][s]amix=inputs=2:duration=first[o]",
                "-map",
                "[o]",
            ])
            .arg(&src)
            .status()
            .unwrap();
        assert!(status.success());

        let clip = |denoise: Option<AudioDenoise>| AudioMixClip {
            path: src.clone(),
            position_secs: 0.0,
            source_in_secs: 0.0,
            source_out_secs: 4.0,
            gain_db: 0.0,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            role: None,
            speed: 1.0,
            effects: Vec::new(),
            denoise,
        };
        let floor = |denoise: Option<AudioDenoise>, name: &str| -> f64 {
            let out = dir.join(name);
            mix_timeline_audio(&[clip(denoise)], 44_100, 4.0, &out, None).unwrap();
            let pcm = decode_f32(&out);
            assert!(pcm.len() >= 44_100 * 4 - 100);
            rms(&pcm[44_100 * 3..44_100 * 4 - 2_000])
        };
        let off = floor(None, "off.wav");
        let on = floor(
            Some(AudioDenoise {
                enabled: true,
                backend: "rnnoise".into(),
                strength: 1.0,
            }),
            "on.wav",
        );
        let drop = db(on) - db(off);
        eprintln!(
            "mix noise floor: off {:.1} dBFS, rnnoise {:.1} dBFS ({drop:.1} dB)",
            db(off),
            db(on)
        );
        assert!(drop < -15.0, "noise floor only dropped {drop:.1} dB");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn realtime_factor_report() {
        let x = white(48_000 * 20, 0.1, 11);
        let t = std::time::Instant::now();
        let y = denoise_interleaved(&x, 1, 1.0).unwrap();
        let el = t.elapsed().as_secs_f64();
        eprintln!(
            "20 s mono processed in {el:.2} s = {:.0}x realtime",
            20.0 / el
        );
        assert_eq!(y.len(), x.len());
    }
}
