//! Project save backups (docs/schema-v8-design.md section 6, "Backups").
//!
//! Every save goes through [`save_project_file`], which
//! 1. keeps the file it is about to replace: on the first save of a migrated file the original
//!    is copied to `<name>.v<from>.renderly.json` next to it (once, never overwritten);
//! 2. keeps a rolling history in `<project dir>/.renderly-backups/<name>/<yyyymmdd-HHMMSS>.json`
//!    with the last 20 versions plus the newest one per UTC day for the last 7 days;
//! 3. writes the new contents atomically (temp file + rename).
//!
//! Timestamps are UTC: the engine carries no time-zone database.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Newest rolling backups always kept.
pub const KEEP_RECENT: usize = 20;
/// Days of daily backups kept beyond the recent ones (one per UTC day, today included).
pub const KEEP_DAILY_DAYS: i64 = 7;
/// Name of the directory, inside the project directory, that holds rolling backups.
pub const BACKUP_DIR: &str = ".renderly-backups";

/// Save `contents` to `path` with backups. `previous_schema_version` is the schema version of
/// the file currently on disk, if the caller parsed it; the one-time migration backup is taken
/// when it is older than `current_version`.
pub fn save_project_file(path: &Path, contents: &str, current_version: u32) -> io::Result<()> {
    save_project_file_at(path, contents, current_version, SystemTime::now())
}

/// [`save_project_file`] with an explicit clock, for tests.
pub fn save_project_file_at(
    path: &Path,
    contents: &str,
    current_version: u32,
    now: SystemTime,
) -> io::Result<()> {
    if path.exists() {
        let previous = fs::read(path)?;
        if let Some(from) = schema_version_of(&previous) {
            if from < current_version {
                backup_before_migration(path, &previous, from)?;
            }
        }
        write_rolling_backup(path, &previous, now)?;
    }
    write_atomic(path, contents.as_bytes())
}

/// Copy `original` to `<name>.v<from>.renderly.json` next to the project file, unless a copy
/// already exists. Returns the path written, or `None` when it already existed.
pub fn backup_before_migration(
    path: &Path,
    original: &[u8],
    from: u32,
) -> io::Result<Option<PathBuf>> {
    let target = migration_backup_path(path, from);
    if target.exists() {
        return Ok(None);
    }
    write_atomic(&target, original)?;
    Ok(Some(target))
}

/// `<dir>/<name>.v<from>.renderly.json` for `<dir>/<name>.renderly.json`.
pub fn migration_backup_path(path: &Path, from: u32) -> PathBuf {
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = project_name(path);
    dir.join(format!("{name}.v{from}.renderly.json"))
}

/// Write `previous` into the rolling history of `path` at time `now`, then prune. Returns the
/// backup path written.
pub fn write_rolling_backup(path: &Path, previous: &[u8], now: SystemTime) -> io::Result<PathBuf> {
    let dir = rolling_dir(path);
    fs::create_dir_all(&dir)?;
    let at_s = unix_seconds(now);
    let stamp = format_stamp(at_s);
    let mut target = dir.join(format!("{stamp}.json"));
    let mut n = 2;
    while target.exists() {
        target = dir.join(format!("{stamp}_{n}.json"));
        n += 1;
    }
    write_atomic(&target, previous)?;
    prune_rolling(&dir, at_s)?;
    Ok(target)
}

/// Rolling backups of `path`, newest first.
pub fn list_rolling_backups(path: &Path) -> io::Result<Vec<PathBuf>> {
    let dir = rolling_dir(path);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<(String, PathBuf)> = fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| {
            let stem = p.file_stem()?.to_string_lossy().into_owned();
            Some((stem, p))
        })
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(entries.into_iter().map(|(_, p)| p).collect())
}

/// Apply the retention rule to a rolling directory as of `now_s`.
fn prune_rolling(dir: &Path, now_s: i64) -> io::Result<()> {
    let mut entries: Vec<(i64, String, PathBuf)> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| {
            let stem = p.file_stem()?.to_string_lossy().into_owned();
            let secs = parse_stamp(&stem)?;
            Some((secs, stem, p))
        })
        .collect();
    // Newest first; ties broken by name so `_2` variants sort after their base.
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));

    let today = now_s.div_euclid(86_400);
    let mut kept_days = std::collections::HashSet::new();
    for (index, (secs, _, path)) in entries.iter().enumerate() {
        if index < KEEP_RECENT {
            continue;
        }
        let day = secs.div_euclid(86_400);
        let within_week = today - day < KEEP_DAILY_DAYS && day <= today;
        if within_week && kept_days.insert(day) {
            continue;
        }
        fs::remove_file(path)?;
    }
    Ok(())
}

fn rolling_dir(path: &Path) -> PathBuf {
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    dir.join(BACKUP_DIR).join(project_name(path))
}

/// File name without the `.renderly.json` (or `.json`) suffix.
fn project_name(path: &Path) -> String {
    let file = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    file.strip_suffix(".renderly.json")
        .or_else(|| file.strip_suffix(".json"))
        .unwrap_or(&file)
        .to_string()
}

/// Reads `schema_version` from raw project JSON without a full parse.
fn schema_version_of(bytes: &[u8]) -> Option<u32> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    u32::try_from(value.get("schema_version")?.as_u64()?).ok()
}

/// Temp file + rename, so a crash never leaves a half-written project.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

fn unix_seconds(now: SystemTime) -> i64 {
    now.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `yyyymmdd-HHMMSS` (UTC) for `secs` since the Unix epoch.
pub fn format_stamp(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Inverse of [`format_stamp`] for the base name (a `_n` suffix is ignored).
fn parse_stamp(stem: &str) -> Option<i64> {
    let base = stem.split('_').next()?;
    if base.len() != 15 || base.as_bytes()[8] != b'-' {
        return None;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    let y = num(&base[0..4])?;
    let m = num(&base[4..6])?;
    let d = num(&base[6..8])?;
    let hh = num(&base[9..11])?;
    let mm = num(&base[11..13])?;
    let ss = num(&base[13..15])?;
    Some(days_from_civil(y, m, d) * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// Days since 1970-01-01 to (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// (year, month, day) to days since 1970-01-01.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: i64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs as u64)
    }

    #[test]
    fn stamps_round_trip() {
        let secs = 1_781_000_000_i64; // 2026-06-09
        let s = format_stamp(secs);
        assert_eq!(s.len(), 15);
        assert_eq!(parse_stamp(&s), Some(secs));
        assert_eq!(format_stamp(0), "19700101-000000");
    }

    #[test]
    fn retention_keeps_recent_and_one_per_day() {
        let dir = std::env::temp_dir().join(format!("renderly-backup-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let project = dir.join("p.renderly.json");
        let now = 1_781_000_000_i64;
        // 30 saves, one per hour, over 30 hours: all are within a week, so the 20 newest plus
        // the newest of each day that is older than them are kept.
        for i in 0..30 {
            write_rolling_backup(&project, b"{}", at(now - i * 3600)).unwrap();
        }
        let kept = list_rolling_backups(&project).unwrap();
        assert!(kept.len() >= 20 && kept.len() <= 22, "kept {}", kept.len());
        // Very old saves (80 days back) are dropped except the one-per-day policy: they are
        // outside the seven-day window, so after pruning only the 20 newest remain.
        for i in 0..30 {
            write_rolling_backup(&project, b"{}", at(now - 80 * 86_400 - i * 3600)).unwrap();
        }
        let kept = list_rolling_backups(&project).unwrap();
        assert!(kept.len() <= 22, "kept {}", kept.len());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrated_file_gets_one_original_copy() {
        let dir =
            std::env::temp_dir().join(format!("renderly-migrate-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let project = dir.join("demo.renderly.json");
        fs::write(&project, r#"{"schema_version": 7}"#).unwrap();
        save_project_file_at(&project, r#"{"schema_version": 8}"#, 8, at(1_781_000_000)).unwrap();
        let copy = dir.join("demo.v7.renderly.json");
        assert_eq!(
            fs::read_to_string(&copy).unwrap(),
            r#"{"schema_version": 7}"#
        );
        assert_eq!(
            fs::read_to_string(&project).unwrap(),
            r#"{"schema_version": 8}"#
        );
        // A second save of the migrated file does not replace the original copy.
        fs::write(
            dir.join("demo.renderly.json"),
            r#"{"schema_version": 7, "x": 1}"#,
        )
        .unwrap();
        save_project_file_at(&project, r#"{"schema_version": 8}"#, 8, at(1_781_000_100)).unwrap();
        assert_eq!(
            fs::read_to_string(&copy).unwrap(),
            r#"{"schema_version": 7}"#
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
