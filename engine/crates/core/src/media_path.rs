// SPDX-License-Identifier: GPL-3.0-or-later

//! Portable media paths (WS-03, NFR-32): conversion between native paths and the forward-slash
//! form stored in show files, relative/absolute choice against the show file's folder, and the
//! comparison key that makes names equal across macOS (NFD, case-insensitive) and Windows
//! (NFC, case-insensitive). Strings only; the engine does the file-system work.
//!
//! Case folding uses `str::to_lowercase`, not full Unicode case folding: `ß` and `SS` stay
//! different. Both platforms' file systems treat them as different names too.

use unicode_normalization::UnicodeNormalization;

use crate::cue::MediaRef;

/// How the host spells paths. On Windows both `\` and `/` separate components; on macOS a
/// backslash is an ordinary file-name character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathStyle {
    Posix,
    Windows,
}

impl PathStyle {
    /// The style of the platform this build runs on.
    pub const fn host() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }
}

/// NFC form. macOS file systems hand back NFD names; show files are written NFC.
pub fn normalize_nfc(text: &str) -> String {
    text.nfc().collect()
}

/// Native path text to the forward-slash form. Does not touch Unicode form or case.
pub fn to_portable(native: &str, style: PathStyle) -> String {
    match style {
        PathStyle::Windows => native.replace('\\', "/"),
        PathStyle::Posix => native.to_owned(),
    }
}

/// True for `/x`, `//server/share`, `C:/x` and a bare `C:/`. `C:x` (drive-relative) is not: it
/// is treated as a relative path whose first component is the literal name `C:x`. Joined to a
/// show folder that is not a file that exists on any platform, so the media shows as missing
/// instead of resolving against the process's current directory on drive C. Three or more
/// leading slashes are one root, as POSIX says; only exactly two start a network share.
pub fn is_absolute(portable: &str) -> bool {
    root_len(portable) > 0
}

fn root_len(path: &str) -> usize {
    match path.as_bytes() {
        [b'/', ..] => 1,
        [letter, b':', b'/', ..] if letter.is_ascii_alphabetic() => 3,
        _ => 0,
    }
}

/// A path split into a root and clean components.
struct Parts {
    /// `""` (relative), `"/"`, `"//"` (network share) or `"C:/"`.
    root: String,
    comps: Vec<String>,
}

fn parse(portable: &str) -> Parts {
    let (root, rest) = match portable.strip_prefix("//") {
        Some(rest) if !rest.starts_with('/') => ("//", rest),
        // `root_len` counts ASCII bytes only, so the split is on a char boundary.
        _ => portable
            .split_at_checked(root_len(portable))
            .unwrap_or(("", portable)),
    };
    let absolute = !root.is_empty();
    let mut comps: Vec<String> = Vec::new();
    for comp in rest.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                if comps.last().is_some_and(|last| last != "..") {
                    comps.pop();
                } else if !absolute {
                    comps.push("..".to_owned());
                }
                // Above the root of an absolute path: stays at the root.
            }
            _ => comps.push(comp.to_owned()),
        }
    }
    Parts {
        root: root.to_owned(),
        comps,
    }
}

impl Parts {
    fn render(&self) -> String {
        format!("{}{}", self.root, self.comps.join("/"))
    }
}

/// Collapses `.`, `..` and repeated separators, lexically (symlinks are not consulted). A `..`
/// that climbs above the root of an absolute path is dropped; in a relative path it is kept.
pub fn lexical_normalize(portable: &str) -> String {
    parse(portable).render()
}

/// The text two paths must share to name the same file on macOS or Windows: lexically
/// normalised, NFC, lower case.
pub fn comparison_key(portable: &str) -> String {
    let text = normalize_nfc(&lexical_normalize(portable));
    normalize_nfc(&text.to_lowercase())
}

/// Same file name on macOS and Windows: NFC- and case-insensitive.
pub fn paths_equal(a: &str, b: &str) -> bool {
    comparison_key(a) == comparison_key(b)
}

/// A short, stable name for a path: 16 hex digits of FNV-1a over its [`comparison_key`], so the
/// same file spelled differently (case, NFD, `..`) gets the same name on every run and platform.
/// Used to keep per-copy backups and recovery files apart.
pub fn path_hash(portable: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in comparison_key(portable).bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The last component of a portable path, or `""`.
pub fn file_name(portable: &str) -> &str {
    portable
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
}

/// Comparison key of just the file name (for find-in-folder matching).
pub fn file_name_key(portable: &str) -> String {
    comparison_key(file_name(portable))
}

/// `absolute` made relative to `dir` when it lies strictly inside it, comparing components by
/// [`comparison_key`] but keeping `absolute`'s own spelling. `None` otherwise (also when either
/// path is relative).
pub fn relativize(absolute: &str, dir: &str) -> Option<String> {
    let (a, d) = (parse(absolute), parse(dir));
    if a.root.is_empty()
        || d.root.is_empty()
        || comparison_key(&a.root) != comparison_key(&d.root)
        || a.comps.len() <= d.comps.len()
    {
        return None;
    }
    let (head, tail) = a.comps.split_at(d.comps.len());
    let inside = d
        .comps
        .iter()
        .zip(head)
        .all(|(x, y)| comparison_key(x) == comparison_key(y));
    inside.then(|| tail.join("/"))
}

/// Joins a media path to the show file's folder, normalised. An absolute `media` is returned
/// normalised and `dir` ignored.
pub fn resolve(media: &str, dir: &str) -> String {
    if is_absolute(media) || dir.is_empty() {
        lexical_normalize(media)
    } else {
        lexical_normalize(&format!("{}/{media}", dir.trim_end_matches('/')))
    }
}

/// Builds the [`MediaRef`] for a file the user picked: forward slashes (also for a native
/// Windows relative path), NFC, relative to `show_dir` when inside it and absolute otherwise.
/// A path that is already relative is kept as written (apart from slashes and NFC): it may
/// climb out of the show folder on purpose (`../Audio/x.wav`), and the file format reads such
/// paths as written. With no usable `show_dir` (an unsaved workspace) an absolute path is only
/// normalised.
///
/// This trusts the lexical comparison; [`portable_ref_checked`] is the form that asks the file
/// system.
pub fn portable_ref(native: &str, show_dir: Option<&str>, style: PathStyle) -> MediaRef {
    portable_ref_checked(native, show_dir, style, &|_, _| true)
}

/// [`portable_ref`] that stores a relative path only when `same_file` confirms it names the file
/// the client sent. The comparison is case-insensitive and collapses `..` lexically, which is
/// wrong on a case-sensitive volume and through a symlink; core has no file system, so the caller
/// answers. `same_file(as_sent, candidate)` gets the absolute path as the client wrote it (NFC,
/// forward slashes, `..` not collapsed) and the absolute path the relative result would resolve
/// to. When it says no, the path is kept as sent.
pub fn portable_ref_checked(
    native: &str,
    show_dir: Option<&str>,
    style: PathStyle,
    same_file: &dyn Fn(&str, &str) -> bool,
) -> MediaRef {
    let path = normalize_nfc(&to_portable(native, style));
    if !is_absolute(&path) {
        return MediaRef::new(path);
    }
    let Some(dir) = show_dir
        .map(|d| normalize_nfc(&to_portable(d, style)))
        .filter(|d| is_absolute(d))
    else {
        return MediaRef::new(lexical_normalize(&path));
    };
    let absolute = lexical_normalize(&path);
    match relativize(&absolute, &dir) {
        Some(relative) if same_file(&path, &resolve(&relative, &dir)) => MediaRef::new(relative),
        Some(_) => MediaRef::new(path),
        None => MediaRef::new(absolute),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: PathStyle = PathStyle::Windows;
    const P: PathStyle = PathStyle::Posix;

    fn r(native: &str, dir: Option<&str>, style: PathStyle) -> String {
        portable_ref(native, dir, style).as_str().to_owned()
    }

    #[test]
    fn inside_is_relative_outside_is_absolute() {
        assert_eq!(r("/Show/media/a.wav", Some("/Show"), P), "media/a.wav");
        assert_eq!(r("/Other/a.wav", Some("/Show"), P), "/Other/a.wav");
        // Sibling with the same prefix is not inside.
        assert_eq!(r("/Show2/a.wav", Some("/Show"), P), "/Show2/a.wav");
        // The folder itself is not a media file inside it.
        assert_eq!(r("/Show", Some("/Show"), P), "/Show");
    }

    #[test]
    fn windows_paths_use_forward_slashes() {
        assert_eq!(
            r(r"C:\Show\media\a.wav", Some(r"C:\Show"), W),
            "media/a.wav"
        );
        assert_eq!(r(r"D:\Lib\a.wav", Some(r"C:\Show"), W), "D:/Lib/a.wav");
        // Case-insensitive drive letter and folder names.
        assert_eq!(
            r(r"c:\show\Media\a.wav", Some(r"C:\Show"), W),
            "Media/a.wav"
        );
        assert_eq!(
            r(r"\\srv\share\a.wav", Some(r"C:\Show"), W),
            "//srv/share/a.wav"
        );
        assert_eq!(
            r(r"\\srv\share\x\a.wav", Some(r"\\SRV\Share"), W),
            "x/a.wav"
        );
    }

    #[test]
    fn native_relative_windows_path_gets_forward_slashes() {
        assert_eq!(
            r(r"media\sfx\a.wav", Some(r"C:\Show"), W),
            "media/sfx/a.wav"
        );
        // No show folder yet: still converted.
        assert_eq!(r(r"media\a.wav", None, W), "media/a.wav");
    }

    #[test]
    fn backslash_is_a_name_character_on_posix() {
        assert_eq!(
            r(r"/Show/sfx/Door\Slam.wav", Some("/Show"), P),
            r"sfx/Door\Slam.wav"
        );
    }

    #[test]
    fn dot_dot_is_resolved_lexically() {
        assert_eq!(
            r("/Show/a/../media/./b.wav", Some("/Show"), P),
            "media/b.wav"
        );
        assert_eq!(r("/Show/../Other/b.wav", Some("/Show"), P), "/Other/b.wav");
        assert_eq!(lexical_normalize("/../../a"), "/a");
        assert_eq!(lexical_normalize("../../a/../b"), "../../b");
        assert_eq!(lexical_normalize("C:/a/../../b"), "C:/b");
    }

    #[test]
    fn a_relative_path_is_kept_as_written() {
        // It may leave the show folder on purpose (Show/ and Audio/ side by side).
        assert_eq!(r("../Audio/b.wav", Some("/Show"), P), "../Audio/b.wav");
        assert_eq!(r("../Audio/b.wav", None, P), "../Audio/b.wav");
        assert_eq!(r("a/./b.wav", Some("/Show"), P), "a/./b.wav");
        assert_eq!(
            r(r"..\Audio\b.wav", Some(r"C:\Show"), W),
            "../Audio/b.wav"
        );
    }

    #[test]
    fn a_relative_result_needs_the_file_system_to_agree() {
        let check = |as_sent: &str, dir: &str, answer: bool| {
            let seen = std::cell::RefCell::new(None);
            let out = portable_ref_checked(as_sent, Some(dir), P, &|a, b| {
                *seen.borrow_mut() = Some((a.to_owned(), b.to_owned()));
                answer
            });
            (out.as_str().to_owned(), seen.into_inner())
        };
        let (out, seen) = check("/Show/media/link/../x.wav", "/Show", true);
        assert_eq!(out, "media/x.wav");
        // The checker sees the path as sent, not the collapsed one.
        assert_eq!(
            seen,
            Some((
                "/Show/media/link/../x.wav".to_owned(),
                "/Show/media/x.wav".to_owned()
            ))
        );
        // Refused: the path stays as the client wrote it.
        let (out, _) = check("/Show/media/link/../x.wav", "/Show", false);
        assert_eq!(out, "/Show/media/link/../x.wav");
        // A path outside the folder is never offered to the checker.
        let (out, seen) = check("/Other/x.wav", "/Show", false);
        assert_eq!((out.as_str(), seen), ("/Other/x.wav", None));
    }

    #[test]
    fn nfd_input_is_stored_nfc() {
        let nfd = "/Show/Cafe\u{301}.wav";
        assert_eq!(r(nfd, Some("/Show"), P), "Caf\u{e9}.wav");
        // The show folder itself may be NFD (macOS hands back NFD names).
        assert_eq!(
            r("/Show/Cafe\u{301}/a.wav", Some("/Show/Caf\u{e9}"), P),
            "a.wav"
        );
    }

    #[test]
    fn comparison_ignores_form_case_and_separators() {
        assert!(paths_equal("media/Caf\u{e9}.WAV", "media/Cafe\u{301}.wav"));
        assert!(paths_equal("C:/Show/A.wav", "c:/show/./a.WAV"));
        assert!(!paths_equal("media/a.wav", "media/b.wav"));
        assert!(!paths_equal("media/a.wav", "/media/a.wav"));
        assert_eq!(
            file_name_key("x/y/Cafe\u{301}.WAV"),
            file_name_key("Caf\u{e9}.wav")
        );
        // Hangul and Vietnamese compose too.
        assert!(paths_equal("\u{1112}\u{1161}\u{11ab}.wav", "\u{d55c}.wav"));
    }

    #[test]
    fn path_hash_is_stable_and_spelling_independent() {
        assert_eq!(
            path_hash("/Show/a.openlab"),
            path_hash("/show/./x/../A.OPENLAB")
        );
        assert_eq!(path_hash("/Caf\u{e9}"), path_hash("/Cafe\u{301}"));
        assert_ne!(path_hash("/Show/a.openlab"), path_hash("/Show/b.openlab"));
        assert_eq!(path_hash("/Show/a.openlab").len(), 16);
        // Pinned so file names stay valid across releases.
        assert_eq!(path_hash(""), "cbf29ce484222325");
    }

    #[test]
    fn three_or_more_leading_slashes_are_one_root_not_a_share() {
        for style in [P, W] {
            assert_eq!(r("///x", None, style), "/x");
            assert_eq!(r("////x/y", None, style), "/x/y");
        }
        assert_eq!(lexical_normalize("///"), "/");
        assert_eq!(path_hash("///x"), path_hash("/x"));
        assert_eq!(resolve("///x/y", "/Show"), "/x/y");
        assert_eq!(r("///Show/a.wav", Some("/Show"), P), "a.wav");
        // Exactly two slashes remain a share root.
        assert_eq!(lexical_normalize("//srv/share/a"), "//srv/share/a");
        assert!(is_absolute("///x"));
    }

    #[test]
    fn drive_relative_is_a_relative_name_never_a_drive_root() {
        assert!(!is_absolute("C:foo"));
        assert!(!is_absolute("C:"));
        assert_eq!(lexical_normalize("C:foo/bar"), "C:foo/bar");
        // It lands inside the show folder as a literal name, not on drive C.
        assert_eq!(resolve("C:foo", "D:/Show"), "D:/Show/C:foo");
        assert_eq!(r("C:foo", Some("D:/Show"), W), "C:foo");
        assert_eq!(r(r"C:foo\bar", Some("D:/Show"), W), "C:foo/bar");
        assert_eq!(r("C:foo", None, P), "C:foo");
        // `C:/foo` is still absolute.
        assert_eq!(r("C:/foo", Some("D:/Show"), W), "C:/foo");
    }

    #[test]
    fn odd_input_does_not_panic() {
        for text in [
            "",
            "/",
            "//",
            "///",
            ".",
            "..",
            "C:",
            "C:/",
            "C:\\",
            "\\",
            "\0",
            "a\0b",
            "\u{301}",
            "é",
            "//a",
            "C:/..",
            "x/",
            "/x//y/",
            "\u{feff}a",
            "😀/😀.wav",
        ] {
            for style in [W, P] {
                let _ = portable_ref(text, Some(text), style);
                let _ = portable_ref(text, Some("/Show"), style);
                let _ = portable_ref(text, None, style);
            }
            let _ = comparison_key(text);
            let _ = relativize(text, text);
            let _ = resolve(text, text);
            let _ = file_name(text);
        }
        assert_eq!(file_name(""), "");
        assert_eq!(file_name("/"), "");
    }

    #[test]
    fn resolve_joins_and_keeps_absolute() {
        assert_eq!(resolve("media/a.wav", "/Show"), "/Show/media/a.wav");
        assert_eq!(resolve("media/a.wav", "C:/Show/"), "C:/Show/media/a.wav");
        assert_eq!(resolve("/Lib/a.wav", "/Show"), "/Lib/a.wav");
        assert_eq!(resolve("D:/a.wav", "C:/Show"), "D:/a.wav");
    }

    #[test]
    fn unsaved_workspace_keeps_absolute() {
        assert_eq!(r("/Lib/a.wav", None, P), "/Lib/a.wav");
        // A relative show dir is not usable as a base.
        assert_eq!(r("/Lib/a.wav", Some("rel"), P), "/Lib/a.wav");
    }
}
