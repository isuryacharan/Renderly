//! `.rpack` archives (docs/community-store.md, "The `.rpack` file"): a reproducible build of a
//! pack folder, and the checks that run before an archive is extracted. Native only.
//!
//! The same limits apply to a pack folder that is copied into the library ([`copy_folder`]), so
//! a folder cannot install what an archive could not.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::hash;
use super::{load_pack, ItemState, LoadedPack};

/// Largest total uncompressed size of an archive or pack folder.
pub const MAX_UNCOMPRESSED_BYTES: u64 = 1 << 30;
/// Most entries (files and directories) of an archive or pack folder.
pub const MAX_ENTRIES: usize = 10_000;
/// Largest uncompressed / compressed size ratio of one entry.
pub const MAX_RATIO: u64 = 100;

/// What an archive or folder holds, once the checks passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveSummary {
    pub entries: usize,
    pub uncompressed_bytes: u64,
}

/// A written `.rpack`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BuildReport {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub files: usize,
}

/// `<pack-id>-<release>.rpack`, the default file name (`local.starter-1.0.0.rpack`).
pub fn default_file_name(pack: &LoadedPack) -> String {
    format!("{}-{}.rpack", pack.canonical_id, pack.release())
}

/// Builds the pack folder `dir` into a `.rpack`. `out` is the file to write, or a folder that
/// receives [`default_file_name`]. The pack must load with no pack-level error and no item in
/// the `error` state. The archive is byte-identical for the same source: entries are sorted,
/// timestamps and permissions are fixed, and compression is fixed.
pub fn build(dir: &Path, out: &Path) -> Result<BuildReport, String> {
    let pack = load_pack(dir)?;
    if let Some(item) = pack
        .items
        .iter()
        .find(|i| matches!(i.state, ItemState::Error { .. }))
    {
        return Err(format!(
            "{} {}@{} failed to load: {}",
            item.kind,
            item.id,
            item.version,
            item.state.reason().unwrap_or("unknown error")
        ));
    }
    let files = collect_files(dir)?;
    let total: u64 = files
        .iter()
        .map(|(_, path)| fs::metadata(path).map_or(0, |m| m.len()))
        .sum();
    if total > MAX_UNCOMPRESSED_BYTES {
        return Err(format!(
            "the pack holds {total} bytes; the limit is {MAX_UNCOMPRESSED_BYTES}"
        ));
    }
    let dest = if out.is_dir() {
        out.join(default_file_name(&pack))
    } else {
        out.to_path_buf()
    };
    let partial = dest.with_extension("partial");
    write_archive(&files, &partial)?;
    fs::rename(&partial, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let bytes = fs::metadata(&dest).map_or(0, |m| m.len());
    Ok(BuildReport {
        sha256: file_sha256(&dest)?,
        path: dest,
        bytes,
        files: files.len(),
    })
}

fn write_archive(files: &[(String, PathBuf)], partial: &Path) -> Result<(), String> {
    let file = File::create(partial).map_err(|e| format!("{}: {e}", partial.display()))?;
    let mut zip = zip::ZipWriter::new(BufWriter::new(file));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(6))
        .unix_permissions(0o644)
        .last_modified_time(zip::DateTime::default());
    for (relative, path) in files {
        zip.start_file(relative.as_str(), options)
            .map_err(|e| format!("{relative}: {e}"))?;
        let mut source = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        io::copy(&mut source, &mut zip).map_err(|e| format!("{relative}: {e}"))?;
    }
    let mut inner = zip.finish().map_err(|e| e.to_string())?;
    io::Write::flush(&mut inner).map_err(|e| e.to_string())
}

/// Checks the name, type, size and ratio of every entry of the `.rpack` at `path`, and that it
/// has a `pack.json` at its top level. Nothing is extracted.
pub fn check_archive(path: &Path) -> Result<ArchiveSummary, String> {
    let mut archive = open_archive(path)?;
    let count = archive.len();
    if count > MAX_ENTRIES {
        return Err(format!(
            "the archive has {count} entries; the limit is {MAX_ENTRIES}"
        ));
    }
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    let mut has_manifest = false;
    for index in 0..count {
        let entry = archive.by_index_raw(index).map_err(|e| e.to_string())?;
        let name = entry.name().map_err(|e| e.to_string())?.into_owned();
        let relative = entry_path(&name)?;
        if entry.is_symlink() {
            return Err(format!("archive entry '{name}' is a symbolic link"));
        }
        if !seen.insert(relative.clone()) {
            return Err(format!("archive entry '{name}' appears more than once"));
        }
        if entry.is_dir() {
            continue;
        }
        let size = entry.size();
        let packed = entry.compressed_size();
        let ratio_too_high = size > 0
            && (packed == 0 || u128::from(size) > u128::from(MAX_RATIO) * u128::from(packed));
        if ratio_too_high {
            return Err(format!(
                "archive entry '{name}' expands more than {MAX_RATIO}:1"
            ));
        }
        total = total.saturating_add(size);
        if total > MAX_UNCOMPRESSED_BYTES {
            return Err(format!(
                "the archive expands to more than {MAX_UNCOMPRESSED_BYTES} bytes"
            ));
        }
        has_manifest |= relative == "pack.json";
    }
    if !has_manifest {
        return Err("the archive has no pack.json at its top level".into());
    }
    Ok(ArchiveSummary {
        entries: count,
        uncompressed_bytes: total,
    })
}

/// Checks the archive with [`check_archive`], then extracts it into the empty folder `dest`.
/// Every written path is built from validated components, and each entry's bytes are capped at
/// its declared size, so a lying header cannot write more than the checks allowed.
pub(crate) fn extract(path: &Path, dest: &Path) -> Result<ArchiveSummary, String> {
    let summary = check_archive(path)?;
    let mut archive = open_archive(path)?;
    let mut written = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|e| e.to_string())?;
        let name = entry.name().map_err(|e| e.to_string())?.into_owned();
        let relative = entry_path(&name)?;
        let target = dest.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|e| format!("{relative}: {e}"))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{relative}: {e}"))?;
        }
        let declared = entry.size();
        let mut out = File::create(&target).map_err(|e| format!("{relative}: {e}"))?;
        let copied = io::copy(&mut (&mut entry).take(declared.saturating_add(1)), &mut out)
            .map_err(|e| format!("{relative}: {e}"))?;
        if copied != declared {
            return Err(format!(
                "archive entry '{relative}' does not match its declared size"
            ));
        }
        written += copied;
        if written > MAX_UNCOMPRESSED_BYTES {
            return Err("the archive expands to more than the size limit".into());
        }
    }
    Ok(summary)
}

fn open_archive(path: &Path) -> Result<zip::ZipArchive<BufReader<File>>, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    zip::ZipArchive::new(BufReader::new(file))
        .map_err(|e| format!("{} is not a .rpack (zip) archive: {e}", path.display()))
}

/// The relative path of an archive entry, without its trailing `/` for directories. Refuses
/// absolute and non-portable names, drive prefixes, backslashes, and `.` or `..` parts.
fn entry_path(name: &str) -> Result<String, String> {
    if name.is_empty() || name.starts_with('/') || name.contains(['\\', ':', '\0']) {
        return Err(format!(
            "archive entry '{name}' is an absolute or non-portable path"
        ));
    }
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    if trimmed.is_empty()
        || trimmed
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!(
            "archive entry '{name}' is not a plain relative path"
        ));
    }
    Ok(trimmed.to_string())
}

/// Every regular file under `dir` as `(relative path with '/', path)`, sorted by the relative
/// path. Symbolic links refuse the folder. `.rpack` files are left out, so a build can sit in
/// its own pack folder.
pub(crate) fn collect_files(dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut out = Vec::new();
    walk(dir, "", &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
    let read = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in read {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|n| format!("{}: file name is not UTF-8: {n:?}", dir.display()))?;
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(|e| format!("{relative}: {e}"))?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "'{relative}' is a symbolic link; packs may not contain links"
            ));
        }
        if meta.is_dir() {
            walk(&path, &relative, out)?;
        } else if meta.is_file() && !name.ends_with(".rpack") {
            out.push((relative, path));
        }
        if out.len() > MAX_ENTRIES {
            return Err(format!("the pack has more than {MAX_ENTRIES} files"));
        }
    }
    Ok(())
}

/// Copies the pack folder `src` into the empty folder `dest`, under the same limits as an
/// archive. Returns what was copied.
pub(crate) fn copy_folder(src: &Path, dest: &Path) -> Result<ArchiveSummary, String> {
    let files = collect_files(src)?;
    let mut total = 0u64;
    for (relative, path) in &files {
        let size = fs::metadata(path)
            .map_err(|e| format!("{relative}: {e}"))?
            .len();
        total = total.saturating_add(size);
        if total > MAX_UNCOMPRESSED_BYTES {
            return Err(format!(
                "the pack holds more than {MAX_UNCOMPRESSED_BYTES} bytes"
            ));
        }
    }
    for (relative, path) in &files {
        let target = dest.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{relative}: {e}"))?;
        }
        fs::copy(path, &target).map_err(|e| format!("{relative}: {e}"))?;
    }
    Ok(ArchiveSummary {
        entries: files.len(),
        uncompressed_bytes: total,
    })
}

/// SHA-256 of every file of the pack folder `dir`, as one hex digest over the sorted relative
/// paths. Two copies of the same pack have the same digest, whatever archive or folder they came
/// from.
pub(crate) fn tree_digest(dir: &Path) -> Result<String, String> {
    let mut entries = Vec::new();
    for (relative, path) in collect_files(dir)? {
        let bytes = fs::read(&path).map_err(|e| format!("{relative}: {e}"))?;
        entries.push((relative, hash::sha256(&bytes)));
    }
    Ok(hash::item_hash(&entries))
}

/// Lowercase hex SHA-256 of the file at `path`, streamed.
pub(crate) fn file_sha256(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(hash::to_hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::entry_path;

    #[test]
    fn entry_names_must_be_plain_relative_paths() {
        assert_eq!(entry_path("pack.json").unwrap(), "pack.json");
        assert_eq!(entry_path("luts/").unwrap(), "luts");
        assert_eq!(entry_path("luts/a.cube").unwrap(), "luts/a.cube");
        for bad in [
            "",
            "/etc/passwd",
            "../evil",
            "a/../../evil",
            "a//b",
            "C:/x",
            "a\\b",
            "./x",
            "a/./b",
        ] {
            assert!(entry_path(bad).is_err(), "should refuse {bad:?}");
        }
    }
}
