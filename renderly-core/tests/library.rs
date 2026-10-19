//! The user library and the `.rpack` install path (docs/community-store.md, rollout step 1).
//! Every test works in its own temp folder; none touches the real user library.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use renderly_core::library::{Library, LibraryError, Resolved};
use renderly_core::packs::refs::ItemRef;
use renderly_core::packs::{self, load_pack};
use uuid::Uuid;

/// A temp folder that is removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("renderly-library-{tag}-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One LUT item of a test pack. `scale` changes the file, so the item hash changes with it.
struct Item {
    id: &'static str,
    version: u32,
    scale: f32,
    label: &'static str,
}

fn lut(id: &'static str, version: u32, scale: f32) -> Item {
    Item {
        id,
        version,
        scale,
        label: id,
    }
}

fn cube(scale: f32) -> String {
    let mut text = String::from("LUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                text.push_str(&format!(
                    "{} {} {}\n",
                    r as f32 * scale,
                    g as f32 * scale,
                    b as f32 * scale
                ));
            }
        }
    }
    text
}

/// Writes a format-2 pack `test.looks` at `dir` with the given release and LUT items.
fn write_pack(dir: &Path, release: &str, items: &[Item]) {
    fs::create_dir_all(dir.join("luts")).unwrap();
    let mut decls = Vec::new();
    for item in items {
        let rel = format!("luts/{}-{}.cube", item.id, item.version);
        fs::write(dir.join(&rel), cube(item.scale)).unwrap();
        decls.push(format!(
            r#"{{"kind":"lut","id":"{}","label":"{}","path":"{}","version":{}}}"#,
            item.id, item.label, rel, item.version
        ));
    }
    let pack = format!(
        r#"{{"format":2,"id":"test.looks","name":"Test Looks","release":"{release}","license":"CC0-1.0","items":[{}]}}"#,
        decls.join(",")
    );
    fs::write(dir.join("pack.json"), pack).unwrap();
}

fn lib_at(scratch: &Scratch) -> Library {
    Library::open(&scratch.path("library")).unwrap()
}

fn refused<T: std::fmt::Debug>(result: Result<T, LibraryError>, needle: &str) {
    match result {
        Err(LibraryError::Refused(message)) => {
            assert!(message.contains(needle), "refusal was: {message}")
        }
        other => panic!("expected a refusal containing {needle:?}, got {other:?}"),
    }
}

fn item_ref(text: &str) -> ItemRef {
    ItemRef::parse(text).unwrap()
}

fn installed(lib: &Library, pack_id: &str) -> Vec<String> {
    lib.list()
        .into_iter()
        .filter(|i| i.pack_id == pack_id)
        .map(|i| i.release)
        .collect()
}

#[test]
fn install_from_folder_is_idempotent_and_survives_reopen() {
    let scratch = Scratch::new("folder");
    let pack = scratch.path("pack");
    write_pack(&pack, "1.0.0", &[lut("teal", 1, 1.0)]);

    let mut lib = lib_at(&scratch);
    let first = lib.install(&pack).unwrap();
    assert!(!first.already_installed);
    assert_eq!(first.source, "folder");
    assert_eq!(first.items_added.get("lut"), Some(&1));
    assert!(scratch
        .path("library/packs/test.looks/1.0.0/pack.json")
        .is_file());

    // Same release, same content: a no-op success.
    let again = lib.install(&pack).unwrap();
    assert!(again.already_installed);
    assert!(again.items_added.is_empty());
    assert_eq!(installed(&lib, "test.looks"), vec!["1.0.0"]);

    // library.json is what a fresh open reads back.
    drop(lib);
    let reopened = lib_at(&scratch);
    let record = reopened
        .list()
        .into_iter()
        .find(|i| i.pack_id == "test.looks")
        .unwrap();
    assert_eq!(record.release, "1.0.0");
    assert!(record.enabled);
    assert!(record.content_sha256.is_some());
    assert!(record.archive_sha256.is_none());
}

#[test]
fn rpack_and_folder_installs_have_the_same_content() {
    let scratch = Scratch::new("rpack");
    let pack = scratch.path("pack");
    write_pack(&pack, "1.2.0", &[lut("teal", 1, 1.0), lut("print", 1, 0.5)]);
    let archive = packs::build(&pack, &scratch.0).unwrap();
    assert_eq!(archive.path.file_name().unwrap(), "test.looks-1.2.0.rpack");

    let mut from_file = Library::open(&scratch.path("file-lib")).unwrap();
    let report = from_file.install(&archive.path).unwrap();
    assert_eq!(report.source, "file");
    assert_eq!(report.items_added.get("lut"), Some(&2));

    let mut from_folder = Library::open(&scratch.path("folder-lib")).unwrap();
    from_folder.install(&pack).unwrap();

    let digest = |lib: &Library| {
        lib.list()
            .into_iter()
            .find(|i| i.pack_id == "test.looks")
            .unwrap()
            .content_sha256
    };
    assert_eq!(digest(&from_file), digest(&from_folder));
    let record = from_file
        .list()
        .into_iter()
        .find(|i| i.pack_id == "test.looks")
        .unwrap();
    assert_eq!(
        record.archive_sha256.as_deref(),
        Some(archive.sha256.as_str())
    );

    // The folder of the same release is already installed from the archive.
    let again = from_file.install(&pack).unwrap();
    assert!(again.already_installed);
}

#[test]
fn reproducible_build_is_byte_identical() {
    let scratch = Scratch::new("repro");
    let pack = scratch.path("pack");
    write_pack(&pack, "1.0.0", &[lut("teal", 1, 1.0), lut("print", 1, 0.5)]);
    let first = packs::build(&pack, &scratch.path("a.rpack")).unwrap();
    let second = packs::build(&pack, &scratch.path("b.rpack")).unwrap();
    assert_eq!(
        fs::read(&first.path).unwrap(),
        fs::read(&second.path).unwrap()
    );

    // Timestamps are not part of the input: touching every file changes nothing.
    let later = SystemTime::now() + Duration::from_secs(86_400);
    for entry in fs::read_dir(pack.join("luts")).unwrap() {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(entry.unwrap().path())
            .unwrap();
        file.set_modified(later).unwrap();
    }
    let third = packs::build(&pack, &scratch.path("c.rpack")).unwrap();
    assert_eq!(first.sha256, third.sha256);
    assert_eq!(
        fs::read(&first.path).unwrap(),
        fs::read(&third.path).unwrap()
    );
}

#[test]
fn a_different_file_under_the_same_release_is_refused() {
    let scratch = Scratch::new("conflict");
    let original = scratch.path("original");
    write_pack(&original, "1.0.0", &[lut("teal", 1, 1.0)]);
    let changed = scratch.path("changed");
    write_pack(&changed, "1.0.0", &[lut("teal", 1, 2.0)]);

    let mut lib = lib_at(&scratch);
    lib.install(&original).unwrap();
    refused(
        lib.install(&changed),
        "a different file claims to be Test Looks 1.0.0",
    );
    assert_eq!(installed(&lib, "test.looks"), vec!["1.0.0"]);
}

#[test]
fn a_changed_item_version_across_releases_is_refused() {
    let scratch = Scratch::new("versions");
    let v1 = scratch.path("v1");
    write_pack(&v1, "1.0.0", &[lut("teal", 1, 1.0)]);
    // Same item version, different content, in a later release: refused.
    let v1_changed = scratch.path("v1-changed");
    write_pack(&v1_changed, "1.1.0", &[lut("teal", 1, 2.0)]);
    // A change is a new item version: accepted, side by side with @1.
    let v2 = scratch.path("v2");
    write_pack(&v2, "1.1.0", &[lut("teal", 1, 1.0), lut("teal", 2, 2.0)]);

    let mut lib = lib_at(&scratch);
    lib.install(&v1).unwrap();
    refused(
        lib.install(&v1_changed),
        "published item version never changes",
    );
    assert_eq!(installed(&lib, "test.looks"), vec!["1.0.0"]);

    let report = lib.install(&v2).unwrap();
    assert_eq!(report.other_releases, vec!["1.0.0".to_string()]);
    assert_eq!(installed(&lib, "test.looks"), vec!["1.0.0", "1.1.0"]);
}

#[test]
fn newest_release_wins_and_both_resolve_side_by_side() {
    let scratch = Scratch::new("newest");
    let old = scratch.path("old");
    write_pack(&old, "1.0.0", &[lut("teal", 1, 1.0)]);
    let new = scratch.path("new");
    write_pack(&new, "1.10.0", &[lut("teal", 1, 1.0), lut("grain", 1, 0.5)]);

    let mut lib = lib_at(&scratch);
    lib.install(&old).unwrap();
    lib.install(&new).unwrap();

    let teal_hash = lib
        .load_all()
        .iter()
        .flat_map(|p| p.items.iter())
        .find(|i| i.id == "teal")
        .and_then(|i| i.hash.clone())
        .unwrap();

    // Both releases hold teal@1 with the same hash: semver compare picks 1.10.0 over 1.0.0.
    match lib.resolve(&item_ref("pack:test.looks/teal@1"), Some(&teal_hash)) {
        Resolved::Ok { release, .. } => assert_eq!(release, "1.10.0"),
        other => panic!("expected Ok, got {other:?}"),
    }
    match lib.resolve(&item_ref("pack:test.looks/teal@1"), None) {
        Resolved::Ok { release, .. } => assert_eq!(release, "1.10.0"),
        other => panic!("expected Ok, got {other:?}"),
    }
    // Only the newer release has grain.
    match lib.resolve(&item_ref("pack:test.looks/grain@1"), None) {
        Resolved::Ok { release, item } => {
            assert_eq!(release, "1.10.0");
            assert_eq!(item.id, "grain");
        }
        other => panic!("expected Ok, got {other:?}"),
    }
    // A hash that no release has: the installed copy renders and the instance is marked changed.
    match lib.resolve(&item_ref("pack:test.looks/teal@1"), Some("deadbeef")) {
        Resolved::Changed {
            release,
            installed_hash,
            ..
        } => {
            assert_eq!(release, "1.10.0");
            assert_eq!(installed_hash, teal_hash);
        }
        other => panic!("expected Changed, got {other:?}"),
    }
    assert!(matches!(
        lib.resolve(&item_ref("pack:test.looks/missing@1"), None),
        Resolved::Missing
    ));
}

#[test]
fn uninstall_removes_one_release_or_all_and_refuses_the_builtin_pack() {
    let scratch = Scratch::new("uninstall");
    let old = scratch.path("old");
    write_pack(&old, "1.0.0", &[lut("teal", 1, 1.0)]);
    let new = scratch.path("new");
    write_pack(&new, "1.1.0", &[lut("teal", 1, 1.0), lut("grain", 1, 0.5)]);

    let mut lib = lib_at(&scratch);
    lib.install(&old).unwrap();
    lib.install(&new).unwrap();

    let removed = lib.uninstall("test.looks", Some("1.0.0")).unwrap();
    assert_eq!(removed, vec!["test.looks 1.0.0".to_string()]);
    assert!(!scratch.path("library/packs/test.looks/1.0.0").exists());
    assert_eq!(installed(&lib, "test.looks"), vec!["1.1.0"]);

    lib.uninstall("test.looks", None).unwrap();
    assert!(installed(&lib, "test.looks").is_empty());
    assert!(!scratch.path("library/packs/test.looks").exists());
    assert!(matches!(
        lib.uninstall("test.looks", None),
        Err(LibraryError::NotFound(_))
    ));

    refused(lib.uninstall("builtin", None), "cannot be removed");
    let builtin = lib.list().into_iter().last().unwrap();
    assert_eq!(builtin.pack_id, "builtin");
    assert!(!builtin.removable());
    assert!(!builtin.items.is_empty());
}

#[test]
fn disabled_releases_do_not_load_or_resolve() {
    let scratch = Scratch::new("enable");
    let pack = scratch.path("pack");
    write_pack(&pack, "1.0.0", &[lut("teal", 1, 1.0)]);

    let mut lib = lib_at(&scratch);
    lib.install(&pack).unwrap();
    assert_eq!(lib.load_all().len(), 1);

    assert_eq!(lib.set_enabled("test.looks", None, false).unwrap(), 1);
    assert!(lib.load_all().is_empty());
    assert!(matches!(
        lib.resolve(&item_ref("pack:test.looks/teal@1"), None),
        Resolved::Missing
    ));
    assert!(!lib.list()[0].enabled);

    lib.set_enabled("test.looks", Some("1.0.0"), true).unwrap();
    assert!(matches!(
        lib.resolve(&item_ref("pack:test.looks/teal@1"), None),
        Resolved::Ok { .. }
    ));
}

#[test]
fn dev_link_is_read_again_on_every_load_and_never_deleted() {
    let scratch = Scratch::new("dev");
    let dev = scratch.path("my-pack");
    write_pack(&dev, "0.1.0", &[lut("teal", 1, 1.0)]);

    let mut lib = lib_at(&scratch);
    let report = lib.link_dev(&dev).unwrap();
    assert_eq!(report.items_added.get("lut"), Some(&1));
    let record = lib
        .list()
        .into_iter()
        .find(|i| i.pack_id == "test.looks")
        .unwrap();
    assert!(record.is_dev());
    assert!(record.content_sha256.is_none());
    // Nothing was copied into the library.
    assert!(!scratch.path("library/packs").exists());

    let label_of = |lib: &Library| match lib.resolve(&item_ref("pack:test.looks/teal@1"), None) {
        Resolved::Ok { item, .. } => item.label,
        other => panic!("expected Ok, got {other:?}"),
    };
    assert_eq!(label_of(&lib), "teal");

    // Edit the folder: the next load sees it, with no reinstall.
    write_pack(
        &dev,
        "0.1.0",
        &[Item {
            id: "teal",
            version: 1,
            scale: 1.0,
            label: "Teal, edited",
        }],
    );
    assert_eq!(label_of(&lib), "Teal, edited");

    // A folder that breaks at pack level shows its error and loads nothing.
    fs::write(dev.join("pack.json"), "{ not json").unwrap();
    let listed = lib
        .list()
        .into_iter()
        .find(|i| i.pack_id == "test.looks")
        .unwrap();
    assert!(listed.error.is_some());
    assert!(lib.load_all().is_empty());

    // Uninstalling a dev link forgets it; the author's folder stays.
    lib.uninstall("test.looks", None).unwrap();
    assert!(dev.join("pack.json").is_file());
}

#[test]
fn a_pack_level_error_refuses_the_install() {
    let scratch = Scratch::new("refuse");
    let pack = scratch.path("pack");
    fs::create_dir_all(&pack).unwrap();
    // No license: format 2 requires one.
    fs::write(
        pack.join("pack.json"),
        r#"{"format":2,"id":"test.looks","name":"Test Looks","release":"1.0.0"}"#,
    )
    .unwrap();
    let mut lib = lib_at(&scratch);
    refused(lib.install(&pack), "license");
    assert!(installed(&lib, "test.looks").is_empty());
    assert!(!scratch.path("library/library.json").exists());
}

fn write_zip(path: &Path, entries: &[(&str, Vec<u8>)]) {
    let file = File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
}

const PACK_JSON: &[u8] =
    br#"{"format":2,"id":"test.evil","name":"Evil","release":"1.0.0","license":"CC0-1.0"}"#;

/// Refused archives install nothing and leave library.json alone.
fn assert_archive_refused(name: &str, entries: Vec<(&str, Vec<u8>)>, needle: &str) {
    let scratch = Scratch::new("limits");
    let archive = scratch.path(name);
    write_zip(&archive, &entries);
    let mut lib = lib_at(&scratch);
    refused(lib.install(&archive), needle);
    assert!(installed(&lib, "test.evil").is_empty());
    assert!(!scratch.path("library/library.json").exists());
    assert!(lib.list().iter().all(|i| i.pack_id != "test.evil"));
}

#[test]
fn archives_with_path_traversal_are_refused() {
    assert_archive_refused(
        "traversal.rpack",
        vec![
            ("pack.json", PACK_JSON.to_vec()),
            ("../evil.txt", b"x".to_vec()),
        ],
        "not a plain relative path",
    );
}

#[test]
fn archives_with_absolute_paths_are_refused() {
    assert_archive_refused(
        "absolute.rpack",
        vec![
            ("pack.json", PACK_JSON.to_vec()),
            ("/abs.txt", b"x".to_vec()),
        ],
        "absolute or non-portable path",
    );
}

#[test]
fn archives_with_symbolic_links_are_refused() {
    let scratch = Scratch::new("symlink");
    let archive = scratch.path("link.rpack");
    let file = File::create(&archive).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("pack.json", options).unwrap();
    zip.write_all(PACK_JSON).unwrap();
    zip.add_symlink("link", "/etc/passwd", options).unwrap();
    zip.finish().unwrap();

    let mut lib = lib_at(&scratch);
    refused(lib.install(&archive), "symbolic link");
    assert!(lib.list().iter().all(|i| i.pack_id != "test.evil"));
}

#[test]
fn archives_with_too_many_entries_are_refused() {
    let mut entries = vec![("pack.json", PACK_JSON.to_vec())];
    let names: Vec<String> = (0..10_000).map(|i| format!("f{i}.txt")).collect();
    for name in &names {
        entries.push((name.as_str(), Vec::new()));
    }
    assert_archive_refused("many.rpack", entries, "entries; the limit is 10000");
}

#[test]
fn archives_that_expand_too_much_are_refused() {
    // Five megabytes of zeros deflate to a few kilobytes: far over 100:1.
    assert_archive_refused(
        "bomb.rpack",
        vec![
            ("pack.json", PACK_JSON.to_vec()),
            ("big.bin", vec![0u8; 5_000_000]),
        ],
        "expands more than 100:1",
    );
}

#[test]
fn archives_without_a_top_level_pack_json_are_refused() {
    assert_archive_refused(
        "nested.rpack",
        vec![("evil/pack.json", PACK_JSON.to_vec())],
        "no pack.json at its top level",
    );
}

#[test]
fn packs_with_a_broken_lut_item_install_and_report_the_item() {
    let scratch = Scratch::new("item-error");
    let pack = scratch.path("pack");
    write_pack(&pack, "1.0.0", &[lut("teal", 1, 1.0)]);
    fs::remove_file(pack.join("luts/teal-1.cube")).unwrap();
    // The archive build refuses a pack with a failing item; the folder install reports it.
    assert!(packs::build(&pack, &scratch.0).is_err());
    let mut lib = lib_at(&scratch);
    let report = lib.install(&pack).unwrap();
    assert_eq!(report.items.len(), 1);
    assert_eq!(report.items[0].state, "error");
    assert!(report.items[0].reason.is_some());
    assert!(report.items_added.is_empty());
}

#[test]
fn the_starter_pack_builds_installs_and_loads_as_an_rpack() {
    let starter = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/packs/starter");
    assert!(load_pack(&starter).is_ok());
    let scratch = Scratch::new("starter");
    let archive = packs::build(&starter, &scratch.0).unwrap();
    assert_eq!(
        archive.path.file_name().unwrap(),
        "local.starter-1.0.0.rpack"
    );

    let mut lib = lib_at(&scratch);
    let report = lib.install(&archive.path).unwrap();
    assert_eq!(report.pack_id, "local.starter");
    assert_eq!(report.release, "1.0.0");
    assert_eq!(report.format, 1);
    assert_eq!(report.items_added.get("lut"), Some(&1));

    let record = lib
        .list()
        .into_iter()
        .find(|i| i.pack_id == "local.starter")
        .unwrap();
    assert_eq!(record.license.as_deref(), Some("AGPL-3.0-or-later"));
    assert_eq!(
        record.archive_sha256.as_deref(),
        Some(archive.sha256.as_str())
    );

    // Projects that name the starter by its old id resolve the same LUT.
    for text in ["pack:local.starter/punch@1", "pack:starter/punch@1"] {
        assert!(
            matches!(lib.resolve(&item_ref(text), None), Resolved::Ok { .. }),
            "{text} should resolve"
        );
    }
}
