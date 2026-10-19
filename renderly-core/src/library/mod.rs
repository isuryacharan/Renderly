//! The per-user library of installed packs (docs/community-store.md, "The user library").
//!
//! ```text
//! <root>/library.json                      what is installed, from where, enabled or not
//! <root>/packs/<pack-id>/<release>/        extracted copies (installs; dev links have none)
//! <root>/cache/                            staging folders while an install runs
//! ```
//!
//! Dev links (`source: dev:<path>`) are not copied: every load reads their folder again, so an
//! edit shows on the next load. Builtins are listed as a virtual, non-removable `builtin` pack.
//!
//! Native only. A process is assumed to be the only writer of a library (there is no file
//! locking). Installs are staged under `cache/` and renamed into `packs/` only after every check
//! passed, so an interrupted install leaves no half-installed release behind.

mod install;

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::packs::format2::parse_version;
use crate::packs::refs::{self, ItemRef, Resolution};
use crate::packs::{builtin_items, load_pack, LoadedItem, LoadedPack};

pub use install::InstallReport;

/// The virtual pack that lists the builtin effects, filters and transitions.
pub const BUILTIN_PACK_ID: &str = "builtin";

const LIBRARY_FILE: &str = "library.json";

/// Why a library operation failed.
#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    /// The change is refused: an invalid pack, a conflict, or a reserved id.
    #[error("{0}")]
    Refused(String),
    /// Nothing installed matches.
    #[error("{0}")]
    NotFound(String),
    /// The library folder or its library.json could not be read or written.
    #[error("{0}")]
    Storage(String),
}

/// One installed release, one dev link, or the builtin pack, as `library.json` records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Install {
    pub pack_id: String,
    pub name: String,
    pub release: String,
    /// Pack format (1 or 2). 0 for the virtual builtin pack.
    pub format: u32,
    pub license: Option<String>,
    /// SHA-256 of the `.rpack` file. `None` for folder, dev and builtin entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_sha256: Option<String>,
    /// Digest over every file of the installed copy. `None` for dev links and builtins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
    /// `file`, `folder`, `dev:<absolute path>`, or `builtin`. `import` is reserved.
    pub source: String,
    /// Seconds since the Unix epoch.
    pub installed_at: u64,
    pub enabled: bool,
    /// The per-item load result when it was installed (re-read for dev links on [`Library::list`]).
    pub items: Vec<ItemRecord>,
    /// Why the folder does not load now (a dev link that was edited into an error, say).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Install {
    /// Dev links are read from their folder on every load and never treated as immutable.
    pub fn is_dev(&self) -> bool {
        self.source.starts_with("dev:")
    }

    /// The folder a dev link reads from.
    pub fn dev_path(&self) -> Option<PathBuf> {
        self.source.strip_prefix("dev:").map(PathBuf::from)
    }

    /// Everything except the builtin pack can be uninstalled.
    pub fn removable(&self) -> bool {
        self.pack_id != BUILTIN_PACK_ID
    }
}

/// One item of an install: its state as the loader reported it, with the reason if it is not ok.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemRecord {
    pub kind: String,
    pub id: String,
    pub version: u32,
    /// `ok`, `disabled`, `error` or `skipped`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

impl ItemRecord {
    pub(crate) fn from_item(item: &LoadedItem) -> Self {
        ItemRecord {
            kind: item.kind.clone(),
            id: item.id.clone(),
            version: item.version,
            state: item.state.name().to_string(),
            reason: item.state.reason().map(str::to_string),
            hash: item.hash.clone(),
        }
    }
}

/// The result of resolving a ref against the library.
#[derive(Debug, Clone)]
pub enum Resolved {
    /// Installed and usable. `release` is the release it came from (newest wins).
    Ok { item: LoadedItem, release: String },
    /// Installed with a different hash than the one expected. The installed copy renders.
    Changed {
        item: LoadedItem,
        release: String,
        installed_hash: String,
    },
    /// No installed release has the ref.
    Missing,
    /// Installed, but this build cannot run it.
    Disabled { reason: String },
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LibraryFile {
    #[serde(default)]
    format: u32,
    #[serde(default)]
    installs: Vec<Install>,
}

/// An open library: its root folder and the contents of its `library.json`.
#[derive(Debug)]
pub struct Library {
    root: PathBuf,
    installs: Vec<Install>,
}

/// The default library folder: `RENDERLY_LIBRARY_DIR` if set, else `<user data dir>/Renderly/library`.
pub fn default_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("RENDERLY_LIBRARY_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(|h| PathBuf::from(h).join("AppData").join("Roaming"))
                    .unwrap_or_else(|| PathBuf::from("."))
            })
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join(".local").join("share"))
                    .unwrap_or_else(|| PathBuf::from("."))
            })
    };
    base.join("Renderly").join("library")
}

impl Library {
    /// Opens the library at `root`. A missing folder or library.json is an empty library; the
    /// folders are created by the first change. A library.json that does not parse is an error,
    /// and is never overwritten.
    pub fn open(root: &Path) -> Result<Library, LibraryError> {
        let path = root.join(LIBRARY_FILE);
        let installs = match fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str::<LibraryFile>(&text)
                    .map_err(|e| LibraryError::Storage(format!("{}: {e}", path.display())))?
                    .installs
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(LibraryError::Storage(format!("{}: {e}", path.display()))),
        };
        Ok(Library {
            root: root.to_path_buf(),
            installs,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every installed release and dev link, then the builtin pack. Dev links are re-read, so
    /// their items and error reflect the folder now.
    pub fn list(&self) -> Vec<Install> {
        let mut out: Vec<Install> = self.installs.iter().map(|i| self.refreshed(i)).collect();
        out.push(builtin_install());
        out
    }

    /// Enables or disables every release of `pack_id` (or only `release`). Returns how many
    /// changed. Disabled releases stay installed but do not load.
    pub fn set_enabled(
        &mut self,
        pack_id: &str,
        release: Option<&str>,
        enabled: bool,
    ) -> Result<usize, LibraryError> {
        if pack_id == BUILTIN_PACK_ID {
            return Err(LibraryError::Refused(
                "the builtin pack is always enabled".into(),
            ));
        }
        let mut changed = 0;
        for install in self
            .installs
            .iter_mut()
            .filter(|i| i.pack_id == pack_id && release.is_none_or(|r| i.release == r))
        {
            install.enabled = enabled;
            changed += 1;
        }
        if changed == 0 {
            return Err(not_installed(pack_id, release));
        }
        self.save()?;
        Ok(changed)
    }

    /// Removes every release of `pack_id`, or only `release`. Copies are deleted. A dev link is
    /// only forgotten: its folder is the author's and is never touched. The builtin pack cannot
    /// be removed. Returns the removed `pack_id release` pairs.
    pub fn uninstall(
        &mut self,
        pack_id: &str,
        release: Option<&str>,
    ) -> Result<Vec<String>, LibraryError> {
        if pack_id == BUILTIN_PACK_ID {
            return Err(LibraryError::Refused(
                "the builtin pack cannot be removed".into(),
            ));
        }
        let matches = |i: &Install| i.pack_id == pack_id && release.is_none_or(|r| i.release == r);
        if !self.installs.iter().any(matches) {
            return Err(not_installed(pack_id, release));
        }
        let (gone, kept): (Vec<Install>, Vec<Install>) =
            self.installs.iter().cloned().partition(|i| matches(i));
        self.installs = kept;
        self.save()?;
        for install in &gone {
            if install.is_dev() {
                continue;
            }
            let dir = self
                .packs_dir()
                .join(&install.pack_id)
                .join(&install.release);
            match fs::remove_dir_all(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(LibraryError::Storage(format!("{}: {e}", dir.display()))),
            }
        }
        // Only succeeds when the pack folder is now empty.
        let _ = fs::remove_dir(self.packs_dir().join(pack_id));
        Ok(gone
            .iter()
            .map(|i| format!("{} {}", i.pack_id, i.release))
            .collect())
    }

    /// Every enabled release and dev link that loads, as packs. Dev links are read now, so
    /// edits show without reinstalling. A release whose folder does not load is left out (see
    /// [`Library::list`] for why).
    pub fn load_all(&self) -> Vec<LoadedPack> {
        self.installs
            .iter()
            .filter(|i| i.enabled)
            .filter_map(|i| load_pack(&self.location(i)).ok())
            .collect()
    }

    /// Resolves `item_ref` across every enabled release. When several releases hold the ref,
    /// one with the expected hash wins; otherwise the newest release does (semver compare).
    pub fn resolve(&self, item_ref: &ItemRef, expected_hash: Option<&str>) -> Resolved {
        let mut packs = self.load_all();
        packs.sort_by_key(|p| std::cmp::Reverse(release_key(p)));
        match refs::resolve(&packs, item_ref, expected_hash) {
            Resolution::Ok(item) => Resolved::Ok {
                release: owning_release(&packs, item),
                item: item.clone(),
            },
            Resolution::Changed {
                item,
                installed_hash,
            } => Resolved::Changed {
                release: owning_release(&packs, item),
                item: item.clone(),
                installed_hash,
            },
            Resolution::Missing => Resolved::Missing,
            Resolution::Disabled { reason } => Resolved::Disabled { reason },
        }
    }

    fn refreshed(&self, install: &Install) -> Install {
        let mut out = install.clone();
        if install.is_dev() {
            match load_pack(&self.location(install)) {
                Ok(pack) => {
                    out.name = pack.manifest.name.clone();
                    out.items = pack.items.iter().map(ItemRecord::from_item).collect();
                    out.error = None;
                }
                Err(e) => {
                    out.items = Vec::new();
                    out.error = Some(e);
                }
            }
        }
        out
    }

    /// Where the pack of `install` is read from.
    fn location(&self, install: &Install) -> PathBuf {
        match install.dev_path() {
            Some(path) => path,
            None => self
                .packs_dir()
                .join(&install.pack_id)
                .join(&install.release),
        }
    }

    pub(crate) fn packs_dir(&self) -> PathBuf {
        self.root.join("packs")
    }

    pub(crate) fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// Writes library.json atomically: a temp file, then a rename over the old one.
    pub(crate) fn save(&self) -> Result<(), LibraryError> {
        fs::create_dir_all(&self.root).map_err(storage)?;
        let file = LibraryFile {
            format: 1,
            installs: self.installs.clone(),
        };
        let mut text = serde_json::to_string_pretty(&file).map_err(storage)?;
        text.push('\n');
        let path = self.root.join(LIBRARY_FILE);
        let temp = self.root.join("library.json.tmp");
        fs::write(&temp, text).map_err(storage)?;
        fs::rename(&temp, &path).map_err(storage)
    }
}

fn storage(e: impl std::fmt::Display) -> LibraryError {
    LibraryError::Storage(e.to_string())
}

fn not_installed(pack_id: &str, release: Option<&str>) -> LibraryError {
    LibraryError::NotFound(match release {
        Some(r) => format!("{pack_id} {r} is not installed"),
        None => format!("{pack_id} is not installed"),
    })
}

/// Newest first, by the semver of the release (missing parts count as 0).
fn release_key(pack: &LoadedPack) -> (u64, u64, u64) {
    parse_version(&pack.release()).unwrap_or((0, 0, 0))
}

fn owning_release(packs: &[LoadedPack], item: &LoadedItem) -> String {
    packs
        .iter()
        .find(|p| p.items.iter().any(|i| std::ptr::eq(i, item)))
        .map_or_else(|| BUILTIN_PACK_ID.to_string(), LoadedPack::release)
}

fn builtin_install() -> Install {
    Install {
        pack_id: BUILTIN_PACK_ID.into(),
        name: "Builtin effects".into(),
        release: env!("CARGO_PKG_VERSION").into(),
        format: 0,
        license: Some("AGPL-3.0-or-later".into()),
        archive_sha256: None,
        content_sha256: None,
        source: "builtin".into(),
        installed_at: 0,
        enabled: true,
        items: builtin_items().iter().map(ItemRecord::from_item).collect(),
        error: None,
    }
}

/// A pack id or release that can be a folder name: no separators, no leading dot.
pub(crate) fn safe_segment(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('.')
        && !s.contains("..")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
}

#[cfg(test)]
mod tests {
    use super::safe_segment;

    #[test]
    fn folder_names_are_restricted() {
        assert!(safe_segment("mira.film-looks"));
        assert!(safe_segment("1.3.0-rc.1+build5"));
        assert!(!safe_segment(""));
        assert!(!safe_segment(".."));
        assert!(!safe_segment("a/b"));
        assert!(!safe_segment("a\\b"));
        assert!(!safe_segment(".hidden"));
    }
}
