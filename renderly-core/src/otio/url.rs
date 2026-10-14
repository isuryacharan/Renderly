//! `file://` URL <-> path conversion for OTIO `target_url`s.
//!
//! Pure string functions (no `std::path` semantics) so the Windows and POSIX forms are
//! handled identically — and testable — on every platform. Paths use `/` separators here;
//! `D:\media\a.mp4` and `D:/media/a.mp4` are the same path.
//!
//! | path | URL |
//! |---|---|
//! | `D:\media\my clip.mp4` | `file:///D:/media/my%20clip.mp4` |
//! | `/home/u/clip.mp4` | `file:///home/u/clip.mp4` |
//! | `\\server\share\clip.mp4` | `file://server/share/clip.mp4` |
//! | `media/clip.mp4` (relative) | `media/clip.mp4` (a relative reference, no scheme) |
//!
//! Reading also accepts what other tools write: `file://localhost/D%3a/x.mov` (Premiere),
//! `file:///D|/x`, `file://D:/x`, an unencoded raw path, and `%XX` in any case.

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

/// Percent-encode everything except unreserved characters and `/` (UTF-8 bytes of non-ASCII
/// characters are encoded byte by byte).
pub fn percent_encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if is_unreserved(b) || b == b'/' {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 15) as usize] as char);
        }
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decode `%XX` sequences (either case). Malformed sequences are left as they are; invalid
/// UTF-8 is replaced.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `X:` or `X:/` or `X:\` at the start.
fn has_drive(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || b[2] == b'/' || b[2] == b'\\')
}

/// True for `D:/x`, `D:\x`, `/x`, `\x` and UNC paths.
pub fn is_absolute_path(p: &str) -> bool {
    has_drive(p) || p.starts_with('/') || p.starts_with('\\')
}

/// Path -> URL (see the module table). Absolute paths become `file://` URLs; a relative path
/// becomes a percent-encoded relative reference.
pub fn path_to_file_url(path: &str) -> String {
    let mut p = path.replace('\\', "/");
    if let Some(rest) = p.strip_prefix("//?/") {
        // Windows verbatim prefix (`\\?\C:\x`).
        p = rest.to_string();
    }
    if has_drive(&p) {
        let (drive, rest) = p.split_at(2);
        format!("file:///{}{}", drive, percent_encode_path(rest))
    } else if let Some(unc) = p.strip_prefix("//") {
        format!("file://{}", percent_encode_path(unc))
    } else if p.starts_with('/') {
        format!("file://{}", percent_encode_path(&p))
    } else {
        percent_encode_path(&p)
    }
}

/// The URL scheme (`file`, `http`, ...), if `s` starts with one. A single letter is a drive,
/// not a scheme.
pub fn scheme_of(s: &str) -> Option<&str> {
    let end = s.find(':')?;
    let scheme = &s[..end];
    let ok = scheme.len() >= 2
        && scheme.as_bytes()[0].is_ascii_alphabetic()
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'));
    ok.then_some(scheme)
}

/// URL -> path with `/` separators. `None` for a non-`file` scheme (`http:`, `s3:`, ...).
/// A string without a scheme is taken as a (possibly relative) path, percent-decoded.
pub fn file_url_to_path(url: &str) -> Option<String> {
    let u = url.trim();
    let raw: String = match scheme_of(u) {
        Some(s) if s.eq_ignore_ascii_case("file") => {
            let rest = &u[s.len() + 1..];
            if let Some(after) = rest.strip_prefix("//") {
                let (auth, path) = match after.find('/') {
                    Some(i) => (&after[..i], &after[i..]),
                    None => (after, ""),
                };
                let auth = percent_decode(auth);
                if auth.is_empty() || auth.eq_ignore_ascii_case("localhost") {
                    path.to_string()
                } else if has_drive(&auth) {
                    // `file://D:/x`, written by sloppy tools.
                    format!("/{auth}{path}")
                } else {
                    // `file://server/share/x` is a UNC path.
                    format!("//{auth}{path}")
                }
            } else {
                rest.to_string()
            }
        }
        Some(_) => return None,
        None => u.to_string(),
    };
    let mut p = percent_decode(&raw);
    p = p.replace('\\', "/");
    let b = p.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && matches!(b[2], b':' | b'|') {
        // `/D:/x` and `/D|/x` -> `D:/x`.
        let mut s = p[1..].to_string();
        if s.as_bytes()[1] == b'|' {
            s.replace_range(1..2, ":");
        }
        p = s;
    }
    Some(p)
}

/// Join `base` and a relative path with `/` (no `.`/`..` collapsing — the OS resolves those).
pub fn join(base: &str, rel: &str) -> String {
    let base = base.replace('\\', "/");
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        rel.trim_start_matches("./")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_drive_paths() {
        assert_eq!(
            path_to_file_url(r"D:\media\my clip.mp4"),
            "file:///D:/media/my%20clip.mp4"
        );
        assert_eq!(
            file_url_to_path("file:///D:/media/my%20clip.mp4").as_deref(),
            Some("D:/media/my clip.mp4")
        );
        // Mixed separators and a lowercase drive survive the trip.
        assert_eq!(path_to_file_url("c:/a/b.mov"), "file:///c:/a/b.mov");
        assert_eq!(
            file_url_to_path("file:///c:/a/b.mov").as_deref(),
            Some("c:/a/b.mov")
        );
    }

    #[test]
    fn posix_paths() {
        assert_eq!(
            path_to_file_url("/home/u/clip.mp4"),
            "file:///home/u/clip.mp4"
        );
        assert_eq!(
            file_url_to_path("file:///home/u/clip.mp4").as_deref(),
            Some("/home/u/clip.mp4")
        );
        assert_eq!(
            path_to_file_url("/mnt/My Videos/a#b?.mp4"),
            "file:///mnt/My%20Videos/a%23b%3F.mp4"
        );
        assert_eq!(
            file_url_to_path("file:///mnt/My%20Videos/a%23b%3F.mp4").as_deref(),
            Some("/mnt/My Videos/a#b?.mp4")
        );
    }

    #[test]
    fn non_ascii_and_percent_in_names() {
        let p = "/media/caf\u{e9} 100%.mp4";
        let url = path_to_file_url(p);
        assert_eq!(url, "file:///media/caf%C3%A9%20100%25.mp4");
        assert_eq!(file_url_to_path(&url).as_deref(), Some(p));
    }

    #[test]
    fn unc_paths() {
        assert_eq!(
            path_to_file_url(r"\\server\share\a b.mp4"),
            "file://server/share/a%20b.mp4"
        );
        assert_eq!(
            file_url_to_path("file://server/share/a%20b.mp4").as_deref(),
            Some("//server/share/a b.mp4")
        );
        // Windows verbatim prefix.
        assert_eq!(path_to_file_url(r"\\?\C:\x\a.mp4"), "file:///C:/x/a.mp4");
    }

    #[test]
    fn relative_paths_stay_relative_references() {
        assert_eq!(path_to_file_url("media/a b.mp4"), "media/a%20b.mp4");
        assert_eq!(path_to_file_url(r"media\a.mp4"), "media/a.mp4");
        assert_eq!(
            file_url_to_path("media/a%20b.mp4").as_deref(),
            Some("media/a b.mp4")
        );
        assert!(!is_absolute_path("media/a.mp4"));
        assert!(is_absolute_path("D:/a.mp4") && is_absolute_path("/a.mp4"));
        assert!(is_absolute_path(r"\\srv\share\a"));
    }

    #[test]
    fn foreign_spellings_are_accepted() {
        // Premiere: localhost authority, lowercase percent-encoded colon.
        assert_eq!(
            file_url_to_path("file://localhost/D%3a/media/sc01_sh010_anim.mov").as_deref(),
            Some("D:/media/sc01_sh010_anim.mov")
        );
        assert_eq!(
            file_url_to_path("file:///D|/x/a.mov").as_deref(),
            Some("D:/x/a.mov")
        );
        assert_eq!(
            file_url_to_path("file://D:/x/a.mov").as_deref(),
            Some("D:/x/a.mov")
        );
        assert_eq!(
            file_url_to_path("FILE:///tmp/a.mov").as_deref(),
            Some("/tmp/a.mov")
        );
        assert_eq!(
            file_url_to_path("file:/tmp/a.mov").as_deref(),
            Some("/tmp/a.mov")
        );
        // A raw Windows path (no scheme) is a path, not a URL.
        assert_eq!(
            file_url_to_path(r"C:\x\a.mov").as_deref(),
            Some("C:/x/a.mov")
        );
    }

    #[test]
    fn non_file_schemes_are_not_paths() {
        assert_eq!(file_url_to_path("http://example.com/a.mov"), None);
        assert_eq!(file_url_to_path("s3://bucket/a.mov"), None);
        assert_eq!(scheme_of("C:/x"), None);
        assert_eq!(scheme_of("file:///x"), Some("file"));
    }

    #[test]
    fn decode_leaves_malformed_sequences_alone() {
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("a%zzb"), "a%zzb");
        assert_eq!(percent_decode("%41%62"), "Ab");
        assert_eq!(percent_decode("%4"), "%4");
    }

    #[test]
    fn join_handles_separators() {
        assert_eq!(join(r"D:\proj\", "a/b.mp4"), "D:/proj/a/b.mp4");
        assert_eq!(join("/p", "./a.mp4"), "/p/a.mp4");
    }

    #[test]
    fn roundtrip_is_stable_for_every_form() {
        for p in [
            "D:/a b/c d.mp4",
            "/x/y z/é.mov",
            "//srv/sh/a b.mov",
            "rel/a b.mov",
            "C:/",
        ] {
            let url = path_to_file_url(p);
            assert_eq!(file_url_to_path(&url).as_deref(), Some(p), "{p} via {url}");
        }
    }
}
