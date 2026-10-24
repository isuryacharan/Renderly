//! `RationalTime` / `TimeRange` helpers.
//!
//! OTIO times are `value / rate` seconds. Renderly times are integer µs (`*_us`). Going out, a
//! time is snapped to the nearest frame at the project rate ([`frames`]: round half up, exact
//! integer arithmetic on the rational rate); coming in, each value is converted with **its
//! own** rate (`value / rate`, rounded to µs) — rates are never assumed to be the timeline's.

use serde_json::{json, Value};

use crate::time::{Fps, US_PER_SEC};

pub const RATIONAL_TIME: &str = "RationalTime.1";
pub const TIME_RANGE: &str = "TimeRange.1";

/// Nearest frame index to `us` at `fps` (round half up, exact).
pub fn frames(us: i64, fps: Fps) -> i64 {
    // round(us * num / (den * 1e6)) = floor((2 * us * num + unit) / (2 * unit))
    let num = i128::from(fps.num.max(1));
    let unit = i128::from(US_PER_SEC) * i128::from(fps.den.max(1));
    let n = (2 * i128::from(us) * num + unit).div_euclid(2 * unit);
    n as i64
}

/// Frame count rounded *up* (used for available ranges so a clip's snapped source range can
/// never end past the media it points into).
pub fn frames_ceil(us: i64, fps: Fps) -> i64 {
    let num = i128::from(fps.num.max(1));
    let unit = i128::from(US_PER_SEC) * i128::from(fps.den.max(1));
    let n = -((-i128::from(us) * num).div_euclid(unit));
    n as i64
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
    /// The time in µs, rounded to the nearest microsecond.
    pub fn us(self) -> i64 {
        (self.value / self.rate * US_PER_SEC as f64).round() as i64
    }
}

/// Read a `RationalTime`; `None` when it is not one, or its rate/value are unusable.
pub fn read_rt(v: &Value) -> Option<Rt> {
    let value = v.get("value")?.as_f64()?;
    let rate = v.get("rate")?.as_f64()?;
    (value.is_finite() && rate.is_finite() && rate > 0.0).then_some(Rt { value, rate })
}

/// A parsed `TimeRange`, in µs. `rate` is the start time's rate (used when tallying which
/// frame rate a document is mostly authored in).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    pub start: i64,
    pub dur: i64,
    pub rate: f64,
}

/// Read a `TimeRange`; `None` for `null`, a malformed range, or a negative duration.
pub fn read_range(v: &Value) -> Option<Range> {
    let start = read_rt(v.get("start_time")?)?;
    let dur = read_rt(v.get("duration")?)?;
    (dur.value >= 0.0).then_some(Range {
        start: start.us(),
        dur: dur.us(),
        rate: start.rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_half_up_and_are_exact() {
        let f30 = Fps::new(30, 1);
        assert_eq!(frames(0, f30), 0);
        assert_eq!(frames(1_000_000, f30), 30);
        assert_eq!(frames(33_333, f30), 1);
        // 0.4 frames down, 0.6 frames up, exactly 0.5 up.
        assert_eq!(frames(13_333, f30), 0);
        assert_eq!(frames(20_000, f30), 1);
        assert_eq!(frames(16_667, f30), 1);
        assert_eq!(frames(50_000, f30), 2);
        // Negative times round the same way (half up).
        assert_eq!(frames(-33_333, f30), -1);
        // NTSC rate: frame 100 starts at its own boundary and maps back to 100.
        let ntsc = Fps::new(30000, 1001);
        assert_eq!(frames(ntsc.frame_start(100), ntsc), 100);
    }

    #[test]
    fn ceil_never_undershoots() {
        let f30 = Fps::new(30, 1);
        assert_eq!(frames_ceil(10_000_000, f30), 300);
        assert_eq!(frames_ceil(10_010_000, f30), 301);
        assert_eq!(frames_ceil(0, f30), 0);
    }

    #[test]
    fn rational_time_and_range_roundtrip_through_json() {
        let v = rational_time(48, 24.0);
        assert_eq!(v["OTIO_SCHEMA"], "RationalTime.1");
        assert_eq!(v["value"], 48);
        assert_eq!(v["rate"], 24);
        let rt = read_rt(&v).unwrap();
        assert_eq!(rt.us(), 2_000_000);
        let r = read_range(&time_range(10, 5, 25.0)).unwrap();
        assert_eq!((r.start, r.dur), (400_000, 200_000));
        assert_eq!(r.rate, 25.0);
    }

    #[test]
    fn each_value_converts_with_its_own_rate() {
        let v = json!({"OTIO_SCHEMA":"TimeRange.1",
            "start_time": {"OTIO_SCHEMA":"RationalTime.1","rate":24.0,"value":48.0},
            "duration": {"OTIO_SCHEMA":"RationalTime.1","rate":30.0,"value":45.0}});
        let r = read_range(&v).unwrap();
        assert_eq!(r.start, 2_000_000);
        assert_eq!(r.dur, 1_500_000);
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
}
