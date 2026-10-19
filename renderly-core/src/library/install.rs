//! Installing a `.rpack` file or a pack folder into the library, and linking dev folders.
//!
//! The checks are the ones `renderly pack validate` runs (the same loader), plus the library's
//! own: archive limits, conflicts between releases, and the rule that published item versions
//! never change.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::{safe_segment, storage, Install, ItemRecord, Library, LibraryError, BUILTIN_PACK_ID};
use crate::packs::rpack;
use crate::packs::{load_pack, ItemState, LoadedPack};

/// What an install or link did, for the CLI and the MCP tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallReport {
    pub pack_id: String,
    pub name: String,
    pub release: String,
    pub format: u32,
    pub source: String,
    /// The same release with the same content was already installed. Nothing changed.
    pub already_installed: bool,
    /// Other releases of this pack that stay installed alongside. Newest wins on resolve.
    pub other_releases: Vec<String>,
    /// Items that loaded (state `ok`), counted by kind. Empty for an already-installed release.
    pub items_added: BTreeMap<String, usize>,
    /// Every item with its state, so skipped, disabled and failed ones show their reasons.
    pub items: Vec<ItemRecord>,
}

impl Library {
    /// Installs a `.rpack` file or a pack folder.
    ///
    /// Steps: check the archive limits, extract or copy into `cache/`, load and validate, check
    /// conflicts, then move the folder into `packs/<id>/<release>/` and record it. A pack-level
    /// error refuses the install. An item-level error installs the pack and reports the item.
    /// Installing a release that is already installed with the same content is a no-op.
    pub fn install(&mut self, source: &Path) -> Result<InstallReport, LibraryError> {
        let is_file = if source.is_file() {
            true
        } else if source.is_dir() {
            false
        } else {
            return Err(LibraryError::NotFound(format!(
                "{}: no such .rpack file or pack folder",
                source.display()
            )));
        };
        let staging = self
            .cache_dir()
            .join(format!("install-{}", uuid::Uuid::new_v4()));
        let result = self.stage_and_commit(source, is_file, &staging);
        // After a successful commit the staging folder has been renamed away, so this is a no-op.
        let _ = fs::remove_dir_all(&staging);
        result
    }

    fn stage_and_commit(
        &mut self,
        source: &Path,
        is_file: bool,
        staging: &Path,
    ) -> Result<InstallReport, LibraryError> {
        fs::create_dir_all(staging).map_err(storage)?;
        let (archive_sha256, label) = if is_file {
            rpack::extract(source, staging).map_err(LibraryError::Refused)?;
            let digest = rpack::file_sha256(source).map_err(LibraryError::Refused)?;
            (Some(digest), "file")
        } else {
            rpack::copy_folder(source, staging).map_err(LibraryError::Refused)?;
            (None, "folder")
        };
        let pack = load_pack(staging)
            .map_err(|e| LibraryError::Refused(format!("{}: {e}", source.display())))?;
        let (pack_id, release) = identify(&pack)?;
        let content = rpack::tree_digest(staging).map_err(LibraryError::Refused)?;

        if let Some(existing) = self
            .installs
            .iter()
            .find(|i| i.pack_id == pack_id && i.release == release)
        {
            if existing.is_dev() {
                return Err(LibraryError::Refused(format!(
                    "{} {release} is linked from a folder ({}); unlink it before installing",
                    pack_id, existing.source
                )));
            }
            if existing.content_sha256.as_deref() == Some(content.as_str()) {
                let others = self.other_releases(&pack_id, &release);
                return Ok(report(existing, true, others));
            }
            return Err(LibraryError::Refused(format!(
                "a different file claims to be {} {release}",
                pack.manifest.name
            )));
        }

        self.check_item_versions(&pack, &pack_id, &release)?;

        let target = self.packs_dir().join(&pack_id).join(&release);
        if target.exists() {
            // Left behind by an interrupted install: it is not in library.json.
            fs::remove_dir_all(&target).map_err(storage)?;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(storage)?;
        }
        fs::rename(staging, &target).map_err(storage)?;

        let install = Install {
            pack_id: pack_id.clone(),
            name: pack.manifest.name.clone(),
            release: release.clone(),
            format: pack.format,
            license: pack.meta.license.clone(),
            archive_sha256,
            content_sha256: Some(content),
            source: label.to_string(),
            installed_at: now(),
            enabled: true,
            items: pack.items.iter().map(ItemRecord::from_item).collect(),
            error: None,
        };
        self.installs.push(install.clone());
        if let Err(e) = self.save() {
            self.installs.pop();
            let _ = fs::remove_dir_all(&target);
            return Err(e);
        }
        let others = self.other_releases(&pack_id, &release);
        Ok(report(&install, false, others))
    }

    /// Refuses when an item version already installed from another release of this pack has
    /// different content. Published versions never change; a change is a new version.
    fn check_item_versions(
        &self,
        pack: &LoadedPack,
        pack_id: &str,
        release: &str,
    ) -> Result<(), LibraryError> {
        for item in pack
            .items
            .iter()
            .filter(|i| matches!(i.state, ItemState::Ok))
        {
            let Some(hash) = item.hash.as_deref() else {
                continue;
            };
            for other in self
                .installs
                .iter()
                .filter(|o| o.pack_id == pack_id && o.release != release && !o.is_dev())
            {
                let Some(record) = other
                    .items
                    .iter()
                    .find(|r| r.kind == item.kind && r.id == item.id && r.version == item.version)
                else {
                    continue;
                };
                if record.hash.as_deref().is_some_and(|old| old != hash) {
                    return Err(LibraryError::Refused(format!(
                        "{}@{} is already installed from {} {} with different content; a published item version never changes (give the change a new version)",
                        item.id, item.version, pack_id, other.release
                    )));
                }
            }
        }
        Ok(())
    }

    /// Links a pack folder for development. Nothing is copied: every load reads the folder, so
    /// edits show on the next load. The folder must load at pack level, and its id and release
    /// must not be installed from a file or linked from another folder.
    pub fn link_dev(&mut self, path: &Path) -> Result<InstallReport, LibraryError> {
        let pack = load_pack(path)
            .map_err(|e| LibraryError::Refused(format!("{}: {e}", path.display())))?;
        let (pack_id, release) = identify(&pack)?;
        let source = format!("dev:{}", canonical_path(path)?.display());
        let items: Vec<ItemRecord> = pack.items.iter().map(ItemRecord::from_item).collect();
        let link = Install {
            pack_id: pack_id.clone(),
            name: pack.manifest.name.clone(),
            release: release.clone(),
            format: pack.format,
            license: pack.meta.license.clone(),
            archive_sha256: None,
            content_sha256: None,
            source,
            installed_at: now(),
            enabled: true,
            items,
            error: None,
        };
        let existing = self
            .installs
            .iter()
            .position(|i| i.pack_id == pack_id && i.release == release);
        let already = match existing {
            Some(index) if !self.installs[index].is_dev() => {
                return Err(LibraryError::Refused(format!(
                    "{pack_id} {release} is installed as a copy; uninstall it before linking"
                )));
            }
            Some(index) if self.installs[index].source != link.source => {
                return Err(LibraryError::Refused(format!(
                    "{pack_id} {release} is already linked from {}",
                    self.installs[index].source
                )));
            }
            Some(index) => {
                self.installs[index] = Install {
                    installed_at: self.installs[index].installed_at,
                    ..link.clone()
                };
                true
            }
            None => {
                self.installs.push(link.clone());
                false
            }
        };
        self.save()?;
        let others = self.other_releases(&pack_id, &release);
        Ok(report(&link, already, others))
    }

    fn other_releases(&self, pack_id: &str, release: &str) -> Vec<String> {
        self.installs
            .iter()
            .filter(|i| i.pack_id == pack_id && i.release != release)
            .map(|i| i.release.clone())
            .collect()
    }
}

/// The pack id and release a pack installs under. Format-1 ids are `local.<id>` (see
/// [`LoadedPack::canonical_id`]). The builtin id is reserved.
fn identify(pack: &LoadedPack) -> Result<(String, String), LibraryError> {
    let pack_id = pack.canonical_id.clone();
    if pack_id == BUILTIN_PACK_ID || !safe_segment(&pack_id) {
        return Err(LibraryError::Refused(format!(
            "'{pack_id}' is not a usable pack id"
        )));
    }
    let release = pack.release();
    if !safe_segment(&release) {
        return Err(LibraryError::Refused(format!(
            "release '{release}' of {pack_id} is not usable as a folder name"
        )));
    }
    Ok((pack_id, release))
}

fn report(
    install: &Install,
    already_installed: bool,
    other_releases: Vec<String>,
) -> InstallReport {
    let mut items_added = BTreeMap::new();
    if !already_installed {
        for item in install.items.iter().filter(|i| i.state == "ok") {
            *items_added.entry(item.kind.clone()).or_insert(0) += 1;
        }
    }
    InstallReport {
        pack_id: install.pack_id.clone(),
        name: install.name.clone(),
        release: install.release.clone(),
        format: install.format,
        source: install.source.clone(),
        already_installed,
        other_releases,
        items_added,
        items: install.items.clone(),
    }
}

/// Canonical absolute path, without the `\\?\` prefix Windows adds.
fn canonical_path(path: &Path) -> Result<PathBuf, LibraryError> {
    let full = fs::canonicalize(path).map_err(storage)?;
    let text = full.to_string_lossy();
    Ok(match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => full,
    })
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
