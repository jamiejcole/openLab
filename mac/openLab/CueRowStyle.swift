// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// How one cue state looks in the list. Every state has its own symbol, so state never rests on
/// colour alone (UI-04); `tint` only adds to it. Idle shows no symbol (it is the common state of
/// a 2,000-cue list) but still has an accessibility label.
struct CueStateStyle: Equatable {
    var symbol: String?
    var labelKey: String
    var tint: NSColor?

    var label: String { loc(labelKey) }

    static func style(for state: WireCueState) -> CueStateStyle {
        switch state {
        case .idle: return CueStateStyle(symbol: nil, labelKey: "cue.state.idle", tint: nil)
        case .loaded:
            return CueStateStyle(symbol: "arrow.down.circle.fill", labelKey: "cue.state.loaded", tint: .systemBlue)
        case .preWaiting:
            return CueStateStyle(symbol: "hourglass", labelKey: "cue.state.preWaiting", tint: .systemOrange)
        case .running: return CueStateStyle(symbol: "play.fill", labelKey: "cue.state.running", tint: .systemGreen)
        case .paused: return CueStateStyle(symbol: "pause.fill", labelKey: "cue.state.paused", tint: .systemYellow)
        case .postWaiting:
            return CueStateStyle(symbol: "timer", labelKey: "cue.state.postWaiting", tint: .systemOrange)
        case .fading: return CueStateStyle(symbol: "waveform", labelKey: "cue.state.fading", tint: .systemGreen)
        case .stopping: return CueStateStyle(symbol: "stop.fill", labelKey: "cue.state.stopping", tint: .systemRed)
        case .broken:
            return CueStateStyle(
                symbol: "exclamationmark.triangle.fill", labelKey: "cue.state.broken", tint: .systemRed)
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

/// Symbols and labels for the cue kinds, colours and continue modes (cell content).
enum CueKindStyle {
    static let kinds: [String: (symbol: String, labelKey: String)] = [
        "audio": ("speaker.wave.2", "cue.type.audio"), "group": ("folder", "cue.type.group"),
        "memo": ("note.text", "cue.type.memo"), "wait": ("timer", "cue.type.wait"),
        "fade": ("slider.horizontal.3", "cue.type.fade"), "start": ("play.fill", "cue.type.start"),
        "stop": ("stop.fill", "cue.type.stop"), "pause": ("pause.fill", "cue.type.pause"),
        "load": ("arrow.down.circle", "cue.type.load"), "reset": ("arrow.counterclockwise", "cue.type.reset"),
        "devamp": ("arrow.uturn.forward", "cue.type.devamp"), "go_to": ("arrow.right.to.line", "cue.type.goto"),
        "arm": ("record.circle", "cue.type.arm"), "disarm": ("slash.circle", "cue.type.disarm"),
        "network": ("network", "cue.type.network"),
    ]

    static func kindLabel(_ type: String) -> String { kinds[type].map { loc($0.labelKey) } ?? type }

    /// Each colour has its own shape as well as its tint (UI-04).
    static let colours: [String: (symbol: String, labelKey: String, tint: NSColor)] = [
        "red": ("circle.fill", "cue.colour.red", .systemRed),
        "orange": ("square.fill", "cue.colour.orange", .systemOrange),
        "green": ("triangle.fill", "cue.colour.green", .systemGreen),
        "blue": ("diamond.fill", "cue.colour.blue", .systemBlue),
        "purple": ("hexagon.fill", "cue.colour.purple", .systemPurple),
    ]

    static let continueModes: [String: (symbol: String, labelKey: String)] = [
        "auto_continue": ("arrow.down", "cue.continue.autoContinue"),
        "auto_follow": ("arrow.turn.down.right", "cue.continue.autoFollow"),
    ]
}
