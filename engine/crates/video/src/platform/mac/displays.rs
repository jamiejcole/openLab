// SPDX-License-Identifier: GPL-3.0-or-later

//! The Mac's displays as the stage link names them: by UUID (stable across restarts and
//! reconnection, unlike the CoreGraphics display id), with their name and pixel size.

use objc2_core_foundation::{CFRetained, CFUUID};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayCopyDisplayMode, CGDisplayIsBuiltin, CGDisplayMode,
    CGGetActiveDisplayList,
};
use openlab_core::stage_link::{Display, MAX_DISPLAYS};

#[link(name = "ColorSync", kind = "framework")]
unsafe extern "C" {
    /// ColorSync's stable id for a display (no Rust binding has it). Returns a +1 `CFUUID`, or
    /// null for an id that is not a display.
    fn CGDisplayCreateUUIDFromDisplayID(display: CGDirectDisplayID) -> *mut CFUUID;
}

/// A connected display: its CoreGraphics id (for finding its screen now) and what the stage
/// link says about it.
pub struct Found {
    pub cg: CGDirectDisplayID,
    pub display: Display,
}

/// The active displays. Names come from `name_of` (AppKit's screen names need the main
/// thread); a display it does not name gets a plain one.
pub fn active(name_of: impl Fn(CGDirectDisplayID) -> Option<String>) -> Vec<Found> {
    let mut ids = [0 as CGDirectDisplayID; MAX_DISPLAYS];
    let mut count = 0u32;
    // SAFETY: `ids` holds MAX_DISPLAYS entries, which is the capacity passed; `count` is a
    // valid out pointer.
    let err = unsafe { CGGetActiveDisplayList(MAX_DISPLAYS as u32, ids.as_mut_ptr(), &mut count) };
    if err.0 != 0 {
        return Vec::new();
    }
    let count = (count as usize).min(MAX_DISPLAYS);
    ids.get(..count)
        .unwrap_or_default()
        .iter()
        .enumerate()
        .filter_map(|(n, &cg)| {
            let id = uuid_of(cg)?;
            let mode = CGDisplayCopyDisplayMode(cg);
            let width = u32::try_from(CGDisplayMode::pixel_width(mode.as_deref())).ok()?;
            let height = u32::try_from(CGDisplayMode::pixel_height(mode.as_deref())).ok()?;
            if !openlab_core::side_ok(width) || !openlab_core::side_ok(height) {
                return None;
            }
            let fallback = if CGDisplayIsBuiltin(cg) {
                "Built-in Display".to_owned()
            } else {
                format!("Display {}", n + 1)
            };
            let name = name_of(cg)
                .filter(|n| !n.is_empty() && !n.chars().any(char::is_control))
                .map(|n| truncate(n, openlab_core::stage_link::MAX_TEXT_BYTES))
                .unwrap_or(fallback);
            Some(Found {
                cg,
                display: Display {
                    id,
                    name,
                    width,
                    height,
                },
            })
        })
        .collect()
}

/// `cg`'s UUID as text, `None` if it has none.
fn uuid_of(cg: CGDirectDisplayID) -> Option<String> {
    // SAFETY: any id may be passed; a non-null result is a +1 CFUUID we take ownership of.
    let raw = unsafe { CGDisplayCreateUUIDFromDisplayID(cg) };
    let ptr = std::ptr::NonNull::new(raw)?;
    // SAFETY: created above with a +1 retain count, released when `uuid` drops.
    let uuid: CFRetained<CFUUID> = unsafe { CFRetained::from_raw(ptr) };
    let text = CFUUID::new_string(None, Some(&uuid))?;
    Some(text.to_string())
}

fn truncate(mut text: String, max: usize) -> String {
    while text.len() > max {
        text.pop();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every active display has a UUID, a pixel size and a name, all within the link's limits
    /// (the engine refuses anything else). Reads the displays only; opens no window.
    #[test]
    fn active_displays_are_listed_within_the_link_limits() {
        let found = active(|_| None);
        for f in &found {
            let d = &f.display;
            assert_eq!(d.id.len(), 36, "{d:?}");
            assert_eq!(uuid_of(f.cg).as_deref(), Some(d.id.as_str()));
            assert!(openlab_core::side_ok(d.width) && openlab_core::side_ok(d.height));
            assert!(!d.name.is_empty());
        }
        // A machine running tests has a display, a CI runner a virtual one.
        assert!(!found.is_empty());
        assert_eq!(uuid_of(0xFFFF_FFF0), None, "not a display");
    }

    #[test]
    fn long_names_are_cut_to_the_limit_on_a_character_boundary() {
        let cut = truncate("é".repeat(400), 512);
        assert!(cut.len() <= 512);
        assert_eq!(cut, "é".repeat(256));
    }
}
