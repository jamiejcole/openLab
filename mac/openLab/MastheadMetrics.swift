// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Sizes of the masthead's parts (SM-01), from the window's content size. Show Mode grows the GO
/// button from about 7% x 9% of the window to 12% x 13%, the standby line to twice its size and the
/// notes to 1.7 times theirs.
/// The standby field's room above and below its text.
let StandbyFieldInset: CGFloat = 3

struct MastheadMetrics: Equatable {
    var goSize: NSSize
    var goFontSize: CGFloat
    var standbyFontSize: CGFloat
    var notesFontSize: CGFloat
    var notesHeight: CGFloat
    /// The masthead's whole height: its taller column plus the margins.
    var height: CGFloat

    static let editStandbyFont: CGFloat = 22
    static let editNotesFont: CGFloat = 13
    static let editNotesHeight: CGFloat = 44

    static func make(showMode: Bool, window: NSSize) -> MastheadMetrics {
        let (wf, hf): (CGFloat, CGFloat) = showMode ? (0.12, 0.13) : (0.07, 0.09)
        // Floors keep the button usable in a small window.
        let go = NSSize(width: max(80, (window.width * wf).rounded()), height: max(56, (window.height * hf).rounded()))
        let standby = showMode ? editStandbyFont * 2 : editStandbyFont
        let notesFont = showMode ? editNotesFont * 1.7 : editNotesFont
        let notesHeight = showMode ? editNotesHeight * 1.7 : editNotesHeight
        // The standby field's box keeps room above and below the text (`StandbyField`), on top of its line.
        let column = (standby * 1.25).rounded(.up) + 2 * StandbyFieldInset + 6 + notesHeight.rounded()
        return MastheadMetrics(
            goSize: go, goFontSize: min(go.height * 0.5, go.width * 0.4), standbyFontSize: standby,
            notesFontSize: notesFont.rounded(), notesHeight: notesHeight.rounded(),
            height: max(go.height, column) + 20)
    }
}
