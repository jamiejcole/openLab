// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The masthead's colours in Show Mode (SM-03): a black field with white text and a bright GO
/// button with black text, readable from across a dark booth. openLab's own scheme (the app has no
/// QLab control for this). Fixed sRGB values so the contrast ratios are testable numbers, not
/// whatever the system appearance resolves to.
enum ShowModePalette {
    struct RGB: Equatable {
        var r: Double, g: Double, b: Double
        var color: NSColor { NSColor(srgbRed: r, green: g, blue: b, alpha: 1) }
    }

    static let background = RGB(r: 0, g: 0, b: 0)
    static let standbyText = RGB(r: 1, g: 1, b: 1)
    static let notesText = RGB(r: 0.88, g: 0.88, b: 0.88)
    static let goFill = RGB(r: 0.2, g: 0.85, b: 0.4)
    static let goText = RGB(r: 0, g: 0, b: 0)

    /// WCAG 2 relative luminance of an sRGB colour.
    static func luminance(_ c: RGB) -> Double {
        func lin(_ v: Double) -> Double { v <= 0.03928 ? v / 12.92 : pow((v + 0.055) / 1.055, 2.4) }
        return 0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    }

    /// WCAG 2 contrast ratio, 1 (none) to 21 (black on white).
    static func contrast(_ a: RGB, _ b: RGB) -> Double {
        let (x, y) = (luminance(a), luminance(b))
        return (max(x, y) + 0.05) / (min(x, y) + 0.05)
    }
}

/// Sizes of the masthead's parts (SM-01), from the window's content size. Show Mode grows the GO
/// button from about 7% x 9% of the window to 12% x 13%, the standby line to twice its size and the
/// notes to 1.7 times theirs.
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
        let column = (standby * 1.25).rounded(.up) + 6 + notesHeight.rounded()
        return MastheadMetrics(
            goSize: go, goFontSize: min(go.height * 0.5, go.width * 0.4), standbyFontSize: standby,
            notesFontSize: notesFont.rounded(), notesHeight: notesHeight.rounded(),
            height: max(go.height, column) + 20)
    }
}
