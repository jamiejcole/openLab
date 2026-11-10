// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// What one row of the OSC passcode table lets a connected peer do (`osc_access` of the
/// settings; NET-02). Edit and Control need View: the engine refuses them without it.
struct OscRights: Hashable, Sendable {
    var view = false
    var edit = false
    var control = false

    static let all = OscRights(view: true, edit: true, control: true)
    static let none = OscRights()

    var json: JSONValue {
        .object(["view": .bool(view), "edit": .bool(edit), "control": .bool(control)])
    }

    init(view: Bool = false, edit: Bool = false, control: Bool = false) {
        self.view = view
        self.edit = edit
        self.control = control
    }

    init(json: JSONValue?) {
        func flag(_ key: String) -> Bool {
            if case .bool(let b)? = json?[key] { return b }
            return false
        }
        self.init(view: flag("view"), edit: flag("edit"), control: flag("control"))
    }
}

/// The workspace's OSC passcode table as the engine has it.
struct OscAccessTable: Hashable, Sendable {
    struct Row: Hashable, Sendable {
        var passcode: String
        var rights: OscRights
    }

    /// The rights of a peer that has not connected or connects without a passcode.
    var noPasscode = OscRights.none
    var rows: [Row] = []

    /// The engine's limit on passcode rows (`MAX_OSC_PASSCODES`).
    static let maxRows = 16

    /// The app takes passcodes of exactly four digits.
    static func isValidPasscode(_ text: String) -> Bool {
        text.utf8.count == 4 && text.utf8.allSatisfy { $0 >= 0x30 && $0 <= 0x39 }
    }

    init() {}

    init(settings: JSONValue?) {
        let access = settings?["osc_access"]
        noPasscode = OscRights(json: access?["no_passcode"])
        rows = (access?["passcodes"]?.arrayValue ?? []).compactMap { row in
            row["passcode"]?.stringValue.map { Row(passcode: $0, rights: OscRights(json: row["rights"])) }
        }
    }
}

extension Command {
    /// `AddOscPasscode { index, passcode, rights }`.
    static func addOscPasscode(_ passcode: String, rights: OscRights, at index: Int) -> Command {
        Command(
            .object([
                "type": .string("add_osc_passcode"), "index": .int(Int64(index)), "passcode": .string(passcode),
                "rights": rights.json,
            ]))
    }

    /// `RemoveOscPasscode { passcode }`.
    static func removeOscPasscode(_ passcode: String) -> Command {
        Command(.object(["type": .string("remove_osc_passcode"), "passcode": .string(passcode)]))
    }

    /// `SetOscRights` of one row (`passcode`), or of the No Passcode row when nil.
    static func setOscRights(_ passcode: String?, _ rights: OscRights) -> Command {
        let target: JSONValue =
            passcode.map { .object(["type": .string("passcode"), "passcode": .string($0)]) }
            ?? .object(["type": .string("no_passcode")])
        return Command(
            .object(["type": .string("set_osc_rights"), "target": target, "rights": rights.json]))
    }
}
