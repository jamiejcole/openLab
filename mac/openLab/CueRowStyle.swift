// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// How one cue state looks in the list. Every state has its own symbol, so state never rests on
/// colour alone (UI-04); `tint` only adds to it. Idle shows no symbol (it is the common state of
/// a 2,000-cue list) but still has an accessibility label.
struct CueStateStyle: Equatable {
    var icon: Icon?
    var labelKey: String
    var tint: NSColor?

    var label: String { loc(labelKey) }

    static func style(for state: WireCueState) -> CueStateStyle {
        switch state {
        case .idle: return CueStateStyle(icon: nil, labelKey: "cue.state.idle", tint: nil)
        case .loaded:
            return CueStateStyle(icon: .stateLoaded, labelKey: "cue.state.loaded", tint: .systemBlue)
        case .preWaiting:
            return CueStateStyle(icon: .statePreWaiting, labelKey: "cue.state.preWaiting", tint: .systemOrange)
        case .running: return CueStateStyle(icon: .stateRunning, labelKey: "cue.state.running", tint: .systemGreen)
        case .paused: return CueStateStyle(icon: .statePaused, labelKey: "cue.state.paused", tint: .systemYellow)
        case .postWaiting:
            return CueStateStyle(icon: .statePostWaiting, labelKey: "cue.state.postWaiting", tint: .systemOrange)
        case .fading: return CueStateStyle(icon: .stateFading, labelKey: "cue.state.fading", tint: .systemGreen)
        case .stopping: return CueStateStyle(icon: .stateStopping, labelKey: "cue.state.stopping", tint: .systemRed)
        case .broken:
            return CueStateStyle(
                icon: .stateBroken, labelKey: "cue.state.broken", tint: .systemRed)
        }
    }

    /// Why a cue is broken, for the tooltip: a run-time reason, else its static problems, else a
    /// group's warning about a descendant.
    /// Problems read as the Warnings tab words them, fields filled in (`label` names ids).
    static func brokenDetail(
        reason: WireBrokenReason?, problems: [WireProblem], groupWarning: Bool, label: ProblemText.Label
    ) -> String? {
        if let reason { return ProblemText.reason(reason) }
        if !problems.isEmpty {
            return problems.map { ProblemText.problem($0, label: label) }.joined(separator: "\n")
        }
        return groupWarning ? loc("cue.broken.group") : nil
    }
}

/// What a row draws over its colour: a disarmed cue a slanted stripe, and a disarmed cue that is
/// skipped (QLab 5, manual p. 128) upright wavy lines of its own.
enum RowPattern: Equatable {
    case disarmed, skipped

    static func of(_ cue: Cue) -> RowPattern? {
        guard cue.raw["armed"] == .bool(false) else { return nil }
        return cue.isSkipped ? .skipped : .disarmed
    }
}

/// The playhead's pointer: white on a dark list and near-black on a light one, never the green of
/// a running cue.
enum PlayheadStyle {
    static let colour = NSColor(name: nil) { appearance in
        appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? .white : NSColor(white: 0.15, alpha: 1)
    }
}

/// Symbols and labels for the cue kinds, colours and continue modes (cell content).
enum CueKindStyle {
    static let kinds: [String: (icon: Icon, labelKey: String)] = [
        "audio": (.kindAudio, "cue.type.audio"), "group": (.kindGroup, "cue.type.group"),
        "memo": (.kindMemo, "cue.type.memo"), "wait": (.kindWait, "cue.type.wait"),
        "fade": (.kindFade, "cue.type.fade"), "start": (.kindStart, "cue.type.start"),
        "stop": (.kindStop, "cue.type.stop"), "pause": (.kindPause, "cue.type.pause"),
        "load": (.kindLoad, "cue.type.load"), "reset": (.kindReset, "cue.type.reset"),
        "devamp": (.kindDevamp, "cue.type.devamp"), "go_to": (.kindGoTo, "cue.type.goto"),
        "arm": (.kindArm, "cue.type.arm"), "disarm": (.kindDisarm, "cue.type.disarm"),
        "network": (.kindNetwork, "cue.type.network"),
    ]

    static func kindLabel(_ type: String) -> String { kinds[type].map { loc($0.labelKey) } ?? type }

    /// A Group cue's icon and label for each mode (the type column follows the mode, as QLab's does).
    static let groupModes: [String: (icon: Icon, labelKey: String)] = [
        "timeline": (.kindGroup, "cue.type.group.timeline"),
        "playlist": (.kindGroupPlaylist, "cue.type.group.playlist"),
        "start_first_and_enter": (.kindGroupEnter, "cue.type.group.enter"),
        "start_first_and_go_to_next": (.kindGroupNext, "cue.type.group.next"),
        "start_random": (.kindGroupRandom, "cue.type.group.random"),
    ]

    /// The icon and label the type column shows for a cue of `kind` (its `kind` object in the model).
    static func typeStyle(kind: JSONValue?) -> (icon: Icon, label: String)? {
        let type = kind?["type"]?.stringValue ?? ""
        if type == "group", let mode = groupModes[kind?["mode"]?["type"]?.stringValue ?? ""] {
            return (mode.icon, loc(mode.labelKey))
        }
        return kinds[type].map { ($0.icon, loc($0.labelKey)) }
    }

    /// The cue colours in the order the colour menus list them (QLab 5, manual p. 127: eight basic
    /// and twelve extended), under the show file's names.
    static let basicColours = ["red", "orange", "yellow", "green", "cyan", "blue", "purple", "magenta"]
    static let extendedColours = [
        "dark_red", "apricot", "olive", "dark_green", "light_blue", "navy", "indigo", "lilac", "plum", "raspberry",
        "pink", "grey",
    ]

    /// A cue colour only tints its row, as in QLab; it is not a state, so it has no shape. A colour
    /// this app does not know is drawn as none.
    static let colours: [String: (labelKey: String, tint: NSColor)] = {
        func rgb(_ r: CGFloat, _ g: CGFloat, _ b: CGFloat) -> NSColor {
            NSColor(srgbRed: r, green: g, blue: b, alpha: 1)
        }
        let tints: [String: NSColor] = [
            "red": .systemRed, "orange": .systemOrange, "yellow": .systemYellow, "green": .systemGreen,
            "cyan": rgb(0.20, 0.78, 0.86), "blue": .systemBlue, "purple": .systemPurple,
            "magenta": rgb(0.88, 0.20, 0.82),
            "dark_red": rgb(0.62, 0.10, 0.12), "apricot": rgb(0.98, 0.69, 0.47), "olive": rgb(0.55, 0.56, 0.12),
            "dark_green": rgb(0.10, 0.45, 0.18), "light_blue": rgb(0.55, 0.78, 1.0), "navy": rgb(0.20, 0.27, 0.66),
            "indigo": rgb(0.36, 0.20, 0.72), "lilac": rgb(0.78, 0.64, 0.92), "plum": rgb(0.56, 0.26, 0.50),
            "raspberry": rgb(0.86, 0.12, 0.40), "pink": rgb(1.0, 0.60, 0.76), "grey": rgb(0.56, 0.56, 0.58),
        ]
        var out: [String: (labelKey: String, tint: NSColor)] = [:]
        for key in basicColours + extendedColours { out[key] = ("cue.colour.\(key)", tints[key] ?? .gray) }
        return out
    }()

    /// The colour a cue's row shows: its second colour once it has been started since its last
    /// reset (QLab 5, manual p. 127), else its colour.
    static func shownColour(of cue: Cue, played: Bool) -> String {
        let second = cue.raw["second_color"]?.stringValue ?? "none"
        if played, colours[second] != nil { return second }
        return cue.raw["color"]?.stringValue ?? "none"
    }

    /// A round swatch of `key`'s tint for the colour menus; an empty ring for none (or a colour this
    /// app does not know). Drawn when shown, so its ring follows the menu's appearance.
    static func swatch(_ key: String, size: CGFloat = 12) -> NSImage {
        let tint = colours[key]?.tint
        let image = NSImage(size: NSSize(width: size, height: size), flipped: false) { rect in
            let circle = NSBezierPath(ovalIn: rect.insetBy(dx: 0.75, dy: 0.75))
            circle.lineWidth = 1
            if let tint {
                tint.setFill()
                circle.fill()
            }
            NSColor.secondaryLabelColor.setStroke()
            circle.stroke()
            return true
        }
        image.accessibilityDescription = loc(colours[key]?.labelKey ?? "cue.colour.none")
        return image
    }

    static let continueModes: [String: (icon: Icon, labelKey: String)] = [
        "auto_continue": (.continueAuto, "cue.continue.autoContinue"),
        "auto_follow": (.continueFollow, "cue.continue.autoFollow"),
    ]
}
