// SPDX-License-Identifier: GPL-3.0-or-later

//! The OSC passcode table (NET-02, M4.5c): which rights a remote that sends `/connect` gets.
//! As in QLab 5, every passcode carries View / Edit / Control rights, and a "No Passcode" row
//! holds the rights of a peer that has not connected (or connected without a passcode). Edit and
//! Control require View. Pure data; the engine's OSC layer decides what each right allows
//! (`docs/osc.md`, "Rights").

use serde::{Deserialize, Serialize};

/// Most passcodes in the table. Far above any show's need; bounds what every snapshot carries
/// and how many comparisons a `/connect` costs.
pub const MAX_OSC_PASSCODES: usize = 16;
/// Longest passcode a stored table may hold, bytes. Far above any typed passcode; keeps a hostile
/// client from putting megabytes into every snapshot. A show file written before the table
/// existed could hold any text, which load keeps (with a warning) if it fits.
pub const MAX_OSC_PASSCODE_BYTES: usize = 256;
/// Longest passcode a command may add: ASCII digits only (QLab's passcode fields are numeric),
/// 1 to this many.
pub const MAX_OSC_PASSCODE_DIGITS: usize = 32;

/// One kind of access. `Edit` and `Control` include `View`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OscRight {
    View,
    Edit,
    Control,
}

/// What a peer may do over OSC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OscRights {
    pub view: bool,
    pub edit: bool,
    pub control: bool,
}

impl OscRights {
    pub const NONE: Self = Self {
        view: false,
        edit: false,
        control: false,
    };
    pub const ALL: Self = Self {
        view: true,
        edit: true,
        control: true,
    };

    /// Whether the rights include `right`. Edit and Control count only together with View, so a
    /// table that breaks the rule (a hand-edited file) can never grant more than it says.
    pub fn allows(self, right: OscRight) -> bool {
        match right {
            OscRight::View => self.view,
            OscRight::Edit => self.view && self.edit,
            OscRight::Control => self.view && self.control,
        }
    }

    /// Whether Edit and Control are only set together with View.
    pub fn is_consistent(self) -> bool {
        self.view || !(self.edit || self.control)
    }

    /// These rights with Edit and Control cleared when View is not set (the less privileged
    /// reading of an inconsistent set).
    pub fn consistent(self) -> Self {
        if self.view { self } else { Self::NONE }
    }

    /// QLab's text for the rights: `view|edit|control`, any subset in that order, `""` for none.
    pub fn text(self) -> String {
        let mut parts = Vec::new();
        if self.view {
            parts.push("view");
        }
        if self.edit {
            parts.push("edit");
        }
        if self.control {
            parts.push("control");
        }
        parts.join("|")
    }
}

/// One row of the table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OscPasscode {
    pub passcode: String,
    pub rights: OscRights,
}

/// The table: the rights without a passcode, and the passcodes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OscAccess {
    /// A peer that has not connected, or sent `/connect` with no argument.
    pub no_passcode: OscRights,
    pub passcodes: Vec<OscPasscode>,
}

impl Default for OscAccess {
    /// Open to everyone, as a workspace without a passcode was before the table existed.
    fn default() -> Self {
        Self {
            no_passcode: OscRights::ALL,
            passcodes: Vec::new(),
        }
    }
}

impl OscAccess {
    /// A new workspace's table (QLab 5): No Passcode has no rights, and there is one 4-digit
    /// passcode (1000 to 9999: a seed below 1000 gets 1000 added, so no leading zero, so a console that sends it as a number still
    /// matches) with all rights, made from `seed`. The caller draws the seed (core has no
    /// randomness), so the result is the same for the same seed.
    pub fn new_workspace(seed: u32) -> Self {
        Self {
            no_passcode: OscRights::NONE,
            passcodes: vec![OscPasscode {
                passcode: format!(
                    "{}",
                    seed % 10_000 + if seed % 10_000 < 1000 { 1000 } else { 0 }
                ),
                rights: OscRights::ALL,
            }],
        }
    }

    /// Whether a peer that has not connected may not even read: the table asks for a passcode
    /// before anything else.
    pub fn requires_passcode(&self) -> bool {
        !self.no_passcode.allows(OscRight::View)
    }

    /// The row for exactly this passcode.
    pub fn entry(&self, passcode: &str) -> Option<&OscPasscode> {
        self.passcodes.iter().find(|e| e.passcode == passcode)
    }
}

/// Whether `passcode` may be added by a command: 1 to [`MAX_OSC_PASSCODE_DIGITS`] ASCII digits.
pub fn is_valid_new_passcode(passcode: &str) -> bool {
    !passcode.is_empty()
        && passcode.len() <= MAX_OSC_PASSCODE_DIGITS
        && passcode.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rights_text_lists_the_subset_in_order() {
        assert_eq!(OscRights::ALL.text(), "view|edit|control");
        assert_eq!(OscRights::NONE.text(), "");
        let edit = OscRights {
            view: true,
            edit: true,
            control: false,
        };
        assert_eq!(edit.text(), "view|edit");
    }

    #[test]
    fn edit_and_control_count_only_with_view() {
        let broken = OscRights {
            view: false,
            edit: true,
            control: true,
        };
        assert!(!broken.is_consistent());
        assert!(!broken.allows(OscRight::Edit) && !broken.allows(OscRight::Control));
        assert_eq!(broken.consistent(), OscRights::NONE);
        assert!(OscRights::ALL.is_consistent() && OscRights::NONE.is_consistent());
    }

    #[test]
    fn a_new_workspace_gets_one_four_digit_passcode_from_the_seed() {
        let access = OscAccess::new_workspace(42);
        assert_eq!(access.no_passcode, OscRights::NONE);
        assert_eq!(access.passcodes.len(), 1);
        assert_eq!(access.passcodes[0].passcode, "1042");
        assert_eq!(access.passcodes[0].rights, OscRights::ALL);
        assert_eq!(
            OscAccess::new_workspace(123_456).passcodes[0].passcode,
            "3456"
        );
        assert!(access.requires_passcode());
        assert!(!OscAccess::default().requires_passcode());
    }

    #[test]
    fn new_passcodes_are_digits_only() {
        assert!(is_valid_new_passcode("0"));
        assert!(is_valid_new_passcode(&"9".repeat(MAX_OSC_PASSCODE_DIGITS)));
        assert!(!is_valid_new_passcode(""));
        assert!(!is_valid_new_passcode("12a4"));
        assert!(!is_valid_new_passcode("１２３"));
        assert!(!is_valid_new_passcode(
            &"9".repeat(MAX_OSC_PASSCODE_DIGITS + 1)
        ));
    }
}
