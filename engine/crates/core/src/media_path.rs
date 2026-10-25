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

/// True for `/x`, `//server/share`, `C:/x` and a bare `C:/`. `C:x` (drive-relative) is not.
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
    let (root, rest) = if let Some(rest) = portable.strip_prefix("//") {
        ("//", rest)
    } else {
        // `root_len` counts ASCII bytes only, so the split is on a char boundary.
        portable
            .split_at_checked(root_len(portable))
            .unwrap_or(("", portable))
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
/// A relative `native` is taken as relative to the show folder. With no usable `show_dir` (an
/// unsaved workspace) the path is only normalised.
pub fn portable_ref(native: &str, show_dir: Option<&str>, style: PathStyle) -> MediaRef {
    let path = normalize_nfc(&to_portable(native, style));
    let Some(dir) = show_dir
        .map(|d| normalize_nfc(&to_portable(d, style)))
        .filter(|d| is_absolute(d))
    else {
        return MediaRef::new(lexical_normalize(&path));
    };
    let absolute = resolve(&path, &dir);
    MediaRef::new(relativize(&absolute, &dir).unwrap_or(absolute))
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
        // A relative path climbing out of the show folder becomes absolute.
        assert_eq!(r("../Lib/b.wav", Some("/Show"), P), "/Lib/b.wav");
        assert_eq!(lexical_normalize("/../../a"), "/a");
        assert_eq!(lexical_normalize("../../a/../b"), "../../b");
        assert_eq!(lexical_normalize("C:/a/../../b"), "C:/b");
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
