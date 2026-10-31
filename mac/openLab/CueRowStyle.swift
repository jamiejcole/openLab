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

    /// A non-blocking warning: the cue still plays. Nothing raises one yet (a group with a broken
    /// descendant shows broken, like the child).
    static let warning = CueStateStyle(icon: .stateWarning, labelKey: "cue.state.warning", tint: .systemYellow)

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

    /// Each colour has its own shape as well as its tint (UI-04).
    static let colours: [String: (icon: Icon, labelKey: String, tint: NSColor)] = [
        "red": (.colourRed, "cue.colour.red", .systemRed),
        "orange": (.colourOrange, "cue.colour.orange", .systemOrange),
        "green": (.colourGreen, "cue.colour.green", .systemGreen),
        "blue": (.colourBlue, "cue.colour.blue", .systemBlue),
        "purple": (.colourPurple, "cue.colour.purple", .systemPurple),
    ]

    static let continueModes: [String: (icon: Icon, labelKey: String)] = [
        "auto_continue": (.continueAuto, "cue.continue.autoContinue"),
        "auto_follow": (.continueFollow, "cue.continue.autoFollow"),
    ]
}
