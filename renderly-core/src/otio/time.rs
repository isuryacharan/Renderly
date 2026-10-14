//! `RationalTime` / `TimeRange` helpers.
//!
//! OTIO times are `value / rate` seconds. Renderly times are `f64` seconds. Going out, a
//! time is snapped to the nearest frame at the project rate ([`frames`]: round half up, with
//! a tiny epsilon so `x.5` ties and float noise round the same way every time); coming in,
//! each value is converted with **its own** rate (`value / rate`) — rates are never assumed
//! to be the timeline's.

use serde_json::{json, Value};

pub const RATIONAL_TIME: &str = "RationalTime.1";
pub const TIME_RANGE: &str = "TimeRange.1";

/// Nearest frame at `fps` (round half up; stable under float noise).
pub fn frames(secs: f64, fps: f64) -> i64 {
    (secs * fps + 0.5 + 1e-9).floor() as i64
}

/// Frame count rounded *up* (used for available ranges so a clip's snapped source range can
/// never end past the media it points into).
pub fn frames_ceil(secs: f64, fps: f64) -> i64 {
    (secs * fps - 1e-9).ceil() as i64
}

/// A JSON number, written as an integer when it is whole (what OTIO tools emit for frames).
pub fn num(x: f64) -> Value {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 9.0e15 {
        json!(x as i64)
    } else {
        json!(x)
    }
}

/// `RationalTime.1` for `frames` frames at `rate`.
pub fn rational_time(frames: i64, rate: f64) -> Value {
    json!({ "OTIO_SCHEMA": RATIONAL_TIME, "rate": num(rate), "value": frames })
}

/// `TimeRange.1` of `duration` frames starting at `start` frames, at `rate`.
pub fn time_range(start: i64, duration: i64, rate: f64) -> Value {
    json!({
        "OTIO_SCHEMA": TIME_RANGE,
        "start_time": rational_time(start, rate),
        "duration": rational_time(duration, rate),
    })
}

/// A parsed `RationalTime` (value and rate kept apart; `rate` is `> 0` and finite).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rt {
    pub value: f64,
    pub rate: f64,
}

impl Rt {
    pub fn secs(self) -> f64 {
        self.value / self.rate
    }
}

/// Read a `RationalTime`; `None` when it is not one, or its rate/value are unusable.
pub fn read_rt(v: &Value) -> Option<Rt> {
    let value = v.get("value")?.as_f64()?;
    let rate = v.get("rate")?.as_f64()?;
    (value.is_finite() && rate.is_finite() && rate > 0.0).then_some(Rt { value, rate })
}

/// A parsed `TimeRange`, in seconds. `rate` is the start time's rate (used when tallying
/// which frame rate a document is mostly authored in).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    pub start: f64,
    pub dur: f64,
    pub rate: f64,
}

/// Read a `TimeRange`; `None` for `null`, a malformed range, or a negative duration.
pub fn read_range(v: &Value) -> Option<Range> {
    let start = read_rt(v.get("start_time")?)?;
    let dur = read_rt(v.get("duration")?)?;
    (dur.value >= 0.0).then_some(Range {
        start: start.secs(),
        dur: dur.secs(),
        rate: start.rate,
    })
}

/// Snap a value to picoseconds so sums of many `value / rate` terms compare equal to the
/// value they should be (`0.1 + 0.2` style noise never leaks into a project).
pub fn clean(secs: f64) -> f64 {
    (secs * 1e12).round() / 1e12
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_half_up_and_are_stable_under_noise() {
        assert_eq!(frames(0.0, 30.0), 0);
        assert_eq!(frames(1.0, 30.0), 30);
        assert_eq!(frames(1.0 / 30.0, 30.0), 1);
        // 0.4 frames down, 0.6 frames up, exactly 0.5 up.
        assert_eq!(frames(0.4 / 30.0, 30.0), 0);
        assert_eq!(frames(0.6 / 30.0, 30.0), 1);
        assert_eq!(frames(0.5 / 30.0, 30.0), 1);
        assert_eq!(frames(1.5 / 30.0, 30.0), 2);
        // Float noise right below a frame boundary still lands on the frame.
        assert_eq!(frames(3.0 / 30.0 - 1e-12, 30.0), 3);
        // Negative times round the same way (half up).
        assert_eq!(frames(-1.0 / 30.0, 30.0), -1);
        // NTSC rate.
        let ntsc = 30000.0 / 1001.0;
        assert_eq!(frames(100.0 / ntsc, ntsc), 100);
    }

    #[test]
    fn ceil_never_undershoots() {
        assert_eq!(frames_ceil(10.0, 30.0), 300);
        assert_eq!(frames_ceil(10.01, 30.0), 301);
        assert_eq!(frames_ceil(0.0, 30.0), 0);
    }

    #[test]
    fn rational_time_and_range_roundtrip_through_json() {
        let v = rational_time(48, 24.0);
        assert_eq!(v["OTIO_SCHEMA"], "RationalTime.1");
        assert_eq!(v["value"], 48);
        assert_eq!(v["rate"], 24);
        let rt = read_rt(&v).unwrap();
        assert_eq!(rt.secs(), 2.0);
        let r = read_range(&time_range(10, 5, 25.0)).unwrap();
        assert!((r.start - 0.4).abs() < 1e-12 && (r.dur - 0.2).abs() < 1e-12);
        assert_eq!(r.rate, 25.0);
    }

    #[test]
    fn each_value_converts_with_its_own_rate() {
        let v = json!({"OTIO_SCHEMA":"TimeRange.1",
            "start_time": {"OTIO_SCHEMA":"RationalTime.1","rate":24.0,"value":48.0},
            "duration": {"OTIO_SCHEMA":"RationalTime.1","rate":30.0,"value":45.0}});
        let r = read_range(&v).unwrap();
        assert_eq!(r.start, 2.0);
        assert_eq!(r.dur, 1.5);
    }

    #[test]
    fn unusable_times_are_rejected() {
        assert!(read_rt(&json!({"value": 1, "rate": 0})).is_none());
        assert!(read_rt(&json!({"value": 1})).is_none());
        assert!(read_rt(&json!(null)).is_none());
        assert!(read_range(&json!(null)).is_none());
        let neg = json!({"start_time":{"value":0,"rate":24},"duration":{"value":-1,"rate":24}});
        assert!(read_range(&neg).is_none());
    }

    #[test]
    fn num_writes_whole_values_as_integers() {
        assert_eq!(num(24.0).to_string(), "24");
        assert_eq!(num(23.976).to_string(), "23.976");
    }

    #[test]
    fn clean_removes_float_noise() {
        assert_eq!(clean(0.1 + 0.2), 0.3);
        assert_eq!(clean(1.0 / 3.0), 0.333333333333);
    }
}
