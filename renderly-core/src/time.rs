//! Integer time for the project model (schema v8, docs/project-schema.md "Time").
//!
//! Every stored time is an `i64` count of microseconds (`*_us`). Frame boundaries are not
//! whole microseconds at most frame rates, so they are *computed*: frame `n` of a rational
//! rate `num/den` starts at `floor(n * 1_000_000 * den / num)` µs. Every frame-positioning
//! command snaps to that function, so one frame always maps to one µs value.
//!
//! This module is pure (no cfg, no I/O) so the wasm compositor and the web UI can share
//! the exact same maths through `renderly-wasm`.

use serde::{Deserialize, Serialize};

/// Microseconds per second.
pub const US_PER_SEC: i64 = 1_000_000;

/// A rational frame rate `num / den` (e.g. `30000/1001` for NTSC 29.97).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Fps {
    pub num: u32,
    pub den: u32,
}

/// Standard rates the migration and `Fps::from_f64` snap to (within 0.01).
pub const STANDARD_FPS: [(u32, u32); 10] = [
    (24000, 1001),
    (24, 1),
    (25, 1),
    (30000, 1001),
    (30, 1),
    (48, 1),
    (50, 1),
    (60000, 1001),
    (60, 1),
    (120, 1),
];

/// Accepts the rational form `{"num": 30000, "den": 1001}` and, for hand-written or legacy files, a
/// plain float (snapped with [`Fps::from_f64`]).
impl<'de> Deserialize<'de> for Fps {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Rational {
            num: u32,
            den: u32,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Rational(Rational),
            Float(f64),
        }
        match Either::deserialize(deserializer)? {
            Either::Rational(r) if r.num > 0 && r.den > 0 => Ok(Fps::new(r.num, r.den)),
            Either::Rational(_) => Err(serde::de::Error::custom("fps num and den must be > 0")),
            Either::Float(f) if f.is_finite() && f > 0.0 => Ok(Fps::from_f64(f)),
            Either::Float(_) => Err(serde::de::Error::custom("fps must be a positive number")),
        }
    }
}

impl Fps {
    pub const fn new(num: u32, den: u32) -> Self {
        Self { num, den }
    }

    /// Rate as a float (only for display and decoder/encoder boundaries).
    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// The nearest standard rational within 0.01 fps, else `{round(fps*1000), 1000}`.
    /// Used by the v7 -> v8 migration and by hosts converting a float rate.
    pub fn from_f64(fps: f64) -> Self {
        if !fps.is_finite() || fps <= 0.0 {
            return Self::new(30, 1);
        }
        for (num, den) in STANDARD_FPS {
            let r = num as f64 / den as f64;
            if (fps - r).abs() <= 0.01 {
                return Self::new(num, den);
            }
        }
        let num = (fps * 1000.0).round().max(1.0) as u32;
        Self::new(num, 1000)
    }

    /// Start of frame `n` in µs: `floor(n * 1e6 * den / num)`, exact in `i128`.
    pub fn frame_start(self, n: i64) -> i64 {
        let num = self.num.max(1) as i128;
        let den = self.den.max(1) as i128;
        (n as i128 * US_PER_SEC as i128 * den).div_euclid(num) as i64
    }

    /// Index of the frame containing `t_us` (the last frame whose start is `<= t_us`).
    pub fn frame_at(self, t_us: i64) -> i64 {
        let num = self.num.max(1) as i128;
        let den = self.den.max(1) as i128;
        // Estimate, then correct so that frame_start(n) <= t < frame_start(n + 1).
        let mut n = (t_us as i128 * num).div_euclid(US_PER_SEC as i128 * den) as i64;
        while self.frame_start(n + 1) <= t_us {
            n += 1;
        }
        while self.frame_start(n) > t_us {
            n -= 1;
        }
        n
    }

    /// True when `t_us` is exactly a frame boundary.
    pub fn is_on_frame(self, t_us: i64) -> bool {
        self.frame_start(self.frame_at(t_us)) == t_us
    }

    /// The nearest frame boundary to `t_us` (ties go to the earlier frame).
    pub fn snap_to_frame(self, t_us: i64) -> i64 {
        let n = self.frame_at(t_us);
        let lo = self.frame_start(n);
        let hi = self.frame_start(n + 1);
        if t_us - lo <= hi - t_us {
            lo
        } else {
            hi
        }
    }

    /// Nominal integer frame count per second (for timecode), e.g. 30 for 29.97.
    pub fn nominal(self) -> i64 {
        (self.as_f64()).round().max(1.0) as i64
    }
}

impl Default for Fps {
    fn default() -> Self {
        Self::new(30, 1)
    }
}

/// Seconds (float, from a host or a legacy alias) to µs, rounded to the nearest µs.
pub fn seconds_to_us(secs: f64) -> i64 {
    (secs * US_PER_SEC as f64).round() as i64
}

/// µs to seconds, for decoder/encoder/ffmpeg/whisper boundaries and float sampling.
pub fn us_to_seconds(us: i64) -> f64 {
    us as f64 / US_PER_SEC as f64
}

/// A time as written by a command author. Serialises as integer µs when it is already
/// resolved, otherwise as the string it was written as.
///
/// Grammar (see docs/command-api.md "Time"):
/// - integer: microseconds (`1500000`);
/// - `"1.5s"`, `"1500ms"`: seconds / milliseconds (decimal allowed);
/// - `"90f"`: frames at the project's rate (resolved with [`Time::resolve`]);
/// - `"00:00:01:15"`: non-drop timecode `HH:MM:SS:FF` at the project's rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Time {
    /// Absolute microseconds.
    Us(i64),
    /// A frame index at the project rate.
    Frames(i64),
    /// Non-drop timecode at the project rate.
    Timecode {
        hours: i64,
        minutes: i64,
        seconds: i64,
        frames: i64,
    },
}

/// Why a time string did not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    #[error("invalid time \"{0}\": expected integer µs, \"<n>s\", \"<n>ms\", \"<n>f\" or \"HH:MM:SS:FF\"")]
    Syntax(String),
    #[error("time \"{0}\" must not be negative")]
    Negative(String),
    #[error("timecode \"{0}\" has a field out of range")]
    TimecodeRange(String),
}

impl Default for Time {
    fn default() -> Self {
        Time::Us(0)
    }
}

impl Time {
    /// Absolute µs, or `None` when the time needs a project rate to resolve.
    pub fn as_us(self) -> Option<i64> {
        match self {
            Time::Us(us) => Some(us),
            _ => None,
        }
    }

    /// Resolve to absolute µs at `fps`. Frame and timecode forms land on the frame boundary.
    pub fn resolve(self, fps: Fps) -> i64 {
        match self {
            Time::Us(us) => us,
            Time::Frames(n) => fps.frame_start(n),
            Time::Timecode {
                hours,
                minutes,
                seconds,
                frames,
            } => {
                let nominal = fps.nominal();
                let total = ((hours * 60 + minutes) * 60 + seconds) * nominal + frames;
                fps.frame_start(total)
            }
        }
    }

    /// Parse the string grammar. Integers are handled by the caller (they are µs).
    pub fn parse_str(s: &str) -> Result<Self, TimeError> {
        let t = s.trim();
        let bad = || TimeError::Syntax(s.to_string());
        if t.is_empty() {
            return Err(bad());
        }
        if let Some(rest) = t.strip_suffix("ms") {
            let v: f64 = rest.trim().parse().map_err(|_| bad())?;
            return finite_nonneg(s, v / 1000.0).map(|secs| Time::Us(seconds_to_us(secs)));
        }
        if let Some(rest) = t.strip_suffix('s') {
            let v: f64 = rest.trim().parse().map_err(|_| bad())?;
            return finite_nonneg(s, v).map(|secs| Time::Us(seconds_to_us(secs)));
        }
        if let Some(rest) = t.strip_suffix('f') {
            let n: i64 = rest.trim().parse().map_err(|_| bad())?;
            if n < 0 {
                return Err(TimeError::Negative(s.to_string()));
            }
            return Ok(Time::Frames(n));
        }
        if t.contains(':') {
            let parts: Vec<&str> = t.split(':').collect();
            if parts.len() != 4 {
                return Err(bad());
            }
            let mut f = [0i64; 4];
            for (slot, part) in f.iter_mut().zip(parts) {
                *slot = part.trim().parse().map_err(|_| bad())?;
            }
            let [hours, minutes, seconds, frames] = f;
            if hours < 0 || !(0..60).contains(&minutes) || !(0..60).contains(&seconds) || frames < 0
            {
                return Err(TimeError::TimecodeRange(s.to_string()));
            }
            return Ok(Time::Timecode {
                hours,
                minutes,
                seconds,
                frames,
            });
        }
        // Bare integer string is not in the grammar; only JSON integers are µs.
        Err(bad())
    }
}

fn finite_nonneg(original: &str, v: f64) -> Result<f64, TimeError> {
    if !v.is_finite() {
        return Err(TimeError::Syntax(original.to_string()));
    }
    if v < 0.0 {
        return Err(TimeError::Negative(original.to_string()));
    }
    Ok(v)
}

impl Serialize for Time {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match *self {
            Time::Us(us) => serializer.serialize_i64(us),
            Time::Frames(n) => serializer.serialize_str(&format!("{n}f")),
            Time::Timecode {
                hours,
                minutes,
                seconds,
                frames,
            } => serializer
                .serialize_str(&format!("{hours:02}:{minutes:02}:{seconds:02}:{frames:02}")),
        }
    }
}

impl<'de> Deserialize<'de> for Time {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = Time;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an integer (microseconds) or a time string such as \"1.5s\", \"1500ms\", \"90f\" or \"00:00:01:15\"")
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Time, E> {
                if v < 0 {
                    return Err(E::custom(format!("time {v} must not be negative")));
                }
                Ok(Time::Us(v))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Time, E> {
                i64::try_from(v)
                    .map(Time::Us)
                    .map_err(|_| E::custom(format!("time {v} is out of range")))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Time, E> {
                Time::parse_str(v).map_err(E::custom)
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// Snap a µs time that a legacy float-seconds producer wrote to the nearest frame when it is
/// within `tolerance_us` of one, else keep it. Used by the v7 -> v8 migration.
pub fn snap_within(t_us: i64, fps: Fps, tolerance_us: i64) -> i64 {
    let snapped = fps.snap_to_frame(t_us);
    if (snapped - t_us).abs() <= tolerance_us {
        snapped
    } else {
        t_us
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for Time {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Time".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A time: an integer in microseconds, or a string: \"1.5s\", \"1500ms\", \"90f\" (frames at the project rate) or \"00:00:01:15\" (timecode at the project rate). Serialised as integer microseconds.",
            "anyOf": [
                { "type": "integer", "minimum": 0 },
                { "type": "string" }
            ]
        })
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for Fps {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Fps".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "Rational frame rate, for example {\"num\": 30000, \"den\": 1001}.",
            "type": "object",
            "required": ["num", "den"],
            "properties": {
                "num": { "type": "integer", "minimum": 1 },
                "den": { "type": "integer", "minimum": 1 }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_boundaries_are_exact_for_integer_rates() {
        let f30 = Fps::new(30, 1);
        assert_eq!(f30.frame_start(0), 0);
        assert_eq!(f30.frame_start(30), 1_000_000);
        assert_eq!(f30.frame_start(1), 33_333);
        let f60 = Fps::new(60, 1);
        assert_eq!(f60.frame_start(1), 16_666);
    }

    #[test]
    fn ntsc_frame_starts_use_the_rational_rate() {
        let ntsc = Fps::new(30000, 1001);
        // 1001 frames of 30000 fps take exactly 1001000/30000*... = 33.4 s
        assert_eq!(ntsc.frame_start(30000), 1_001_000_000);
        assert_eq!(ntsc.frame_start(1), 33_366);
    }

    #[test]
    fn frame_at_and_on_frame_round_trip() {
        for fps in [Fps::new(24, 1), Fps::new(30000, 1001), Fps::new(60, 1)] {
            for n in [0_i64, 1, 2, 29, 30, 1000, 123_457] {
                let t = fps.frame_start(n);
                assert!(fps.is_on_frame(t), "frame {n} at {fps:?}");
                assert_eq!(fps.frame_at(t), n);
                assert_eq!(fps.frame_at(t + 1), n.max(fps.frame_at(t + 1)));
                assert_eq!(fps.frame_at(fps.frame_start(n + 1) - 1), n);
            }
        }
    }

    #[test]
    fn snap_goes_to_the_nearest_boundary() {
        let f24 = Fps::new(24, 1);
        assert_eq!(f24.snap_to_frame(1_000_010), 1_000_000);
        assert_eq!(f24.snap_to_frame(1_020_000), 1_000_000);
        assert_eq!(f24.snap_to_frame(1_030_000), 1_041_666);
        assert_eq!(f24.snap_to_frame(0), 0);
    }

    #[test]
    fn from_f64_snaps_to_standard_rates() {
        assert_eq!(Fps::from_f64(29.97), Fps::new(30000, 1001));
        assert_eq!(Fps::from_f64(23.976), Fps::new(24000, 1001));
        assert_eq!(Fps::from_f64(30.0), Fps::new(30, 1));
        assert_eq!(Fps::from_f64(12.5), Fps::new(12500, 1000));
    }

    #[test]
    fn parses_every_grammar_form() {
        assert_eq!(Time::parse_str("1.5s").unwrap(), Time::Us(1_500_000));
        assert_eq!(Time::parse_str("1500ms").unwrap(), Time::Us(1_500_000));
        assert_eq!(Time::parse_str("90f").unwrap(), Time::Frames(90));
        assert_eq!(
            Time::parse_str("00:00:01:15").unwrap(),
            Time::Timecode {
                hours: 0,
                minutes: 0,
                seconds: 1,
                frames: 15
            }
        );
        assert!(Time::parse_str("1.5").is_err());
        assert!(Time::parse_str("-1s").is_err());
        assert!(Time::parse_str("00:61:00:00").is_err());
    }

    #[test]
    fn resolves_frames_and_timecode_at_project_rate() {
        let f30 = Fps::new(30, 1);
        assert_eq!(Time::Frames(30).resolve(f30), 1_000_000);
        assert_eq!(
            Time::Timecode {
                hours: 0,
                minutes: 0,
                seconds: 1,
                frames: 15
            }
            .resolve(f30),
            1_500_000
        );
    }

    #[test]
    fn deserializes_ints_and_strings_and_serializes_ints() {
        let t: Time = serde_json::from_str("1500000").unwrap();
        assert_eq!(t, Time::Us(1_500_000));
        let t: Time = serde_json::from_str("\"1.5s\"").unwrap();
        assert_eq!(t, Time::Us(1_500_000));
        assert!(serde_json::from_str::<Time>("-5").is_err());
        assert_eq!(serde_json::to_string(&Time::Us(42)).unwrap(), "42");
    }
}
