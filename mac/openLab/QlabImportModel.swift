// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The logic of "New Workspace from QLab File": what the unsupported-cues step's buttons are and
/// send, how the outputs step pre-fills, and the notes the new workspace comes with. No views.

/// The unsupported-cues step: one Delete / Placeholder choice per cue, Delete to start with.
struct QlabUnsupportedChoices: Equatable {
    enum Button: Equatable {
        case deleteAll, leaveAll, proceed

        var titleKey: String {
            switch self {
            case .deleteAll: return "qlab.unsupported.deleteAll"
            case .leaveAll: return "qlab.unsupported.leaveAll"
            case .proceed: return "qlab.unsupported.continue"
            }
        }
    }

    let rows: [QlabUnsupported]
    private(set) var actions: [String: QlabCueAction]

    init(rows: [QlabUnsupported]) {
        self.rows = rows
        actions = Dictionary(rows.map { ($0.qlabId, QlabCueAction.delete) }, uniquingKeysWith: { a, _ in a })
    }

    func action(of row: QlabUnsupported) -> QlabCueAction { actions[row.qlabId] ?? .delete }

    mutating func set(_ action: QlabCueAction, for row: QlabUnsupported) { actions[row.qlabId] = action }

    mutating func setAll(_ action: QlabCueAction) {
        for row in rows { actions[row.qlabId] = action }
    }

    /// The action every row has, nil when they differ.
    var uniform: QlabCueAction? {
        guard let first = rows.first.map(action(of:)) else { return .delete }
        return rows.allSatisfy { action(of: $0) == first } ? first : nil
    }

    /// The default button does what the table says; the other button is the opposite bulk choice,
    /// and goes while the rows differ.
    var primary: Button {
        switch uniform {
        case .delete?: return .deleteAll
        case .placeholder?: return .leaveAll
        case nil: return .proceed
        }
    }

    var secondary: Button? {
        switch uniform {
        case .delete?: return .leaveAll
        case .placeholder?: return .deleteAll
        case nil: return nil
        }
    }

    /// What `importQlab` takes. Uniform rows send no list (a big show would push one past the
    /// frame limit); mixed rows send the cues not of the majority action.
    func request(pressing button: Button) -> (cues: [QlabCueChoice], other: QlabCueAction) {
        switch button {
        case .deleteAll: return ([], .delete)
        case .leaveAll: return ([], .placeholder)
        case .proceed:
            let deletes = rows.filter { action(of: $0) == .delete }.count
            let majority: QlabCueAction = deletes * 2 > rows.count ? .delete : .placeholder
            let cues = rows.filter { action(of: $0) != majority }.map {
                QlabCueChoice(qlabId: $0.qlabId, action: action(of: $0))
            }
            return (cues, majority)
        }
    }

    /// The rows that are cues: a cart is a cue list, so it is in none of the notes' counts.
    private var cueRows: [QlabUnsupported] { rows.filter { $0.reason != .cart } }

    /// Cues deleted and kept as placeholders after `button`.
    func counts(pressing button: Button) -> (deleted: Int, placeholders: Int) {
        let r = request(pressing: button)
        let named = Dictionary(r.cues.map { ($0.qlabId, $0.action) }, uniquingKeysWith: { a, _ in a })
        let deleted = cueRows.filter { (named[$0.qlabId] ?? r.other) == .delete }.count
        return (deleted, cueRows.count - deleted)
    }

    /// Cues converted, of the file's `cueCount` (which counts no carts).
    func converted(of cueCount: Int) -> Int { max(0, cueCount - cueRows.count) }
}

/// A QLab audio patch's choice on this Mac.
enum QlabAudioChoice: Hashable {
    case none
    case systemOutput
    case device(String)
}

/// The outputs step: each QLab audio patch and video output with a pop-up choice, pre-filled from
/// the report.
struct QlabOutputChoices: Equatable {
    enum Tag: Equatable { case matched, notFound }

    struct AudioRow: Equatable {
        let patch: QlabAudioPatch
        let match: QlabAudioChoice
        var choice: QlabAudioChoice
    }

    struct VideoRow: Equatable {
        let output: QlabVideoOutput
        /// A `listDisplays` id; nil: none.
        let match: String?
        var choice: String?
    }

    var audio: [AudioRow]
    var video: [VideoRow]
    let devices: [DeviceEntry]

    init(report: QlabReport, devices: [DeviceEntry], displays: [StageDisplay]) {
        self.devices = devices
        audio = report.audioPatches.map { patch in
            let match = Self.match(patch, devices)
            return AudioRow(patch: patch, match: match, choice: match)
        }
        video = report.videoOutputs.map { output in
            let match = Self.match(output, displays)
            return VideoRow(output: output, match: match, choice: match)
        }
    }

    /// By device name, exactly and then ignoring case (`list_devices` has names only); a system
    /// output patch goes to the system output.
    static func match(_ patch: QlabAudioPatch, _ devices: [DeviceEntry]) -> QlabAudioChoice {
        if patch.system { return .systemOutput }
        let name = patch.deviceName
        guard !name.isEmpty else { return .none }
        if let d = devices.first(where: { $0.name == name })
            ?? devices.first(where: { $0.name.caseInsensitiveCompare(name) == .orderedSame })
        {
            return .device(d.name)
        }
        return .none
    }

    static func match(_ output: QlabVideoOutput, _ displays: [StageDisplay]) -> String? {
        let name = output.screen
        guard !name.isEmpty else { return nil }
        return
            (displays.first(where: { $0.name == name })
            ?? displays.first(where: { $0.name.caseInsensitiveCompare(name) == .orderedSame }))?.id
    }

    /// What the tag beside an audio row says: nothing once the operator chose another device.
    func tag(audio row: AudioRow) -> Tag? {
        if row.match != .none { return row.choice == row.match ? .matched : nil }
        return row.choice == .none ? .notFound : nil
    }

    func tag(video row: VideoRow) -> Tag? {
        if row.match != nil { return row.choice == row.match ? .matched : nil }
        return row.choice == nil ? .notFound : nil
    }

    func mapping(for row: AudioRow) -> PatchMapping? {
        let count = Int(row.patch.outputs)
        switch row.choice {
        case .none: return nil
        case .systemOutput:
            return .systemOutput(channels: (0..<max(count, 1)).map(UInt32.init))
        case .device(let name):
            guard let d = devices.first(where: { $0.name == name }) else { return nil }
            let n = min(max(count, 1), d.outputChannels)
            return n > 0 ? PatchMapping(device: name, channels: (0..<n).map(UInt32.init)) : nil
        }
    }

    var audioRequest: [QlabPatchChoice] {
        audio.map { QlabPatchChoice(qlabId: $0.patch.qlabId, mapping: mapping(for: $0)) }
    }

    var videoRequest: [QlabOutputChoice] {
        video.map { QlabOutputChoice(qlabId: $0.output.qlabId, display: $0.choice) }
    }
}

/// Everything the operator decided: what the import request carries.
struct QlabImportChoice: Equatable {
    var cues: [QlabCueChoice] = []
    var otherCues: QlabCueAction = .placeholder
    var audioPatches: [QlabPatchChoice] = []
    var videoOutputs: [QlabOutputChoice] = []
    var converted = 0
    var placeholders = 0
    var deleted = 0
}

/// What the notes sheet shows after the workspace opened.
struct QlabNotesInfo: Equatable {
    let fileName: String
    let report: QlabReport
    let converted: Int
    let placeholders: Int
    let deleted: Int

    /// A note's line: its cue's number and name, then the engine's sentence.
    static func text(of note: QlabNote) -> String {
        let head = [note.number, note.name].filter { !$0.isEmpty }.joined(separator: " ")
        return head.isEmpty ? note.detail : "\(head): \(note.detail)"
    }

    /// The plain text Save Notes writes.
    var plainText: String {
        var lines = [String(format: loc("qlab.notes.title"), fileName), ""]
        lines.append(String(format: loc("qlab.notes.converted"), converted))
        lines.append(String(format: loc("qlab.notes.placeholders"), placeholders))
        lines.append(String(format: loc("qlab.notes.deleted"), deleted))
        lines.append("")
        if report.notes.isEmpty {
            lines.append(loc("qlab.notes.none"))
        } else {
            lines += report.notes.map { "- " + Self.text(of: $0) }
            if report.notesOmitted > 0 { lines.append(String(format: loc("qlab.notes.omitted"), report.notesOmitted)) }
        }
        return lines.joined(separator: "\n") + "\n"
    }
}
