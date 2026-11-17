// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// Words for the engine's problems and warnings (DX-01). The engine sends a tagged variant; the
/// text comes from `Localizable.strings`, looked up by `problem.<type>` / `warning.<type>`, with
/// `.<reason>` (or `.<resolution type>`) appended when that more specific key exists. `{field}`
/// in a string is replaced by the variant's field of that name; ids show as cue or patch names.
/// A type this build has no text for shows its raw type name, never nothing.
enum ProblemText {
    typealias Label = (ModelID) -> String

    /// What the engine resolves a patch on the system output to when the Mac has no output
    /// device selected (`docs/protocol.md`, `list_patch_mappings`).
    static let systemOutputName = "system output"

    static func problem(_ p: WireProblem, label: Label) -> String {
        var fields = p.json
        // The resolution of a device problem is a variant of its own: its fields join the problem's.
        var specific: String?
        if let r = p.json["resolution"], case .object(let o) = r {
            specific = r["type"]?.stringValue
            for (k, v) in o where k != "type" { fields[k] = v }
            // A patch on the system output with no output device selected names no device: it
            // says so, as the settings table does.
            if specific == "device_missing", r["device"]?.stringValue == systemOutputName {
                specific = "system_output_missing"
            }
        }
        // The inspector's "no patch" choice is the nil id: say so instead of naming an id.
        if p.type == "network_patch_missing", p.json["patch"]?.stringValue == ModelID.unpatched.raw {
            specific = "none"
        }
        return render("problem", p.type, specific: specific, fields: fields, label: label)
    }

    static func warning(_ w: WireWarning, label: Label) -> String {
        if w.type == "notice" {
            let code = w.json["code"]?.stringValue ?? ""
            if let text = text(for: "warning.notice.\(code)") { return fill(text, w.json, label) }
            // A notice only the binary knows: its own (English) message beats an opaque code. The
            // codes that quote item ids have them named as the rest of the list names them; any
            // other text (file paths, volume names) is shown as it is, an id-shaped name included.
            if let message = w.json["message"]?.stringValue, !message.isEmpty {
                return idQuotingNotices.contains(code) ? namingIDs(message, label) : message
            }
        }
        return render("warning", w.type, specific: w.json["reason"]?.stringValue, fields: w.json, label: label)
    }

    static func reason(_ r: WireBrokenReason) -> String { loc("broken.\(r.rawValue)") }

    /// Why a cue reads `paused` with a reason: its output device was lost while it played or
    /// waited, so there is nothing to resume.
    static func pausedReason(_ r: WireBrokenReason) -> String { loc("paused.\(r.rawValue)") }

    /// A run-time reason as the cue's state calls for it.
    static func reason(_ r: WireBrokenReason, state: WireCueState?) -> String {
        state == .paused ? pausedReason(r) : reason(r)
    }

    /// A warning's words under its cue's title in the Warnings list: `warning.<type>.detail`, which
    /// leaves the cue out (the title names it), else the whole warning.
    static func warningDetail(_ w: WireWarning, label: Label) -> String {
        if w.type != "notice", let template = text(for: "warning.\(w.type).detail") {
            return fill(template, w.json, label)
        }
        return warning(w, label: label)
    }

    private static func render(
        _ family: String, _ type: String, specific: String?, fields: JSONValue, label: Label
    ) -> String {
        let base = "\(family).\(type)"
        let template = specific.flatMap { text(for: "\(base).\($0)") } ?? text(for: base)
        guard let template else { return fill(loc("\(family).unknown"), .object(["type": .string(type)]), label) }
        return fill(template, fields, label)
    }

    /// nil when the strings file has no entry (NSLocalizedString would hand back the key).
    static func text(for key: String) -> String? {
        let missing = "\u{1}"
        let value = NSLocalizedString(key, value: missing, comment: "")
        return value == missing ? nil : value
    }

    private static func fill(_ template: String, _ fields: JSONValue, _ label: Label) -> String {
        guard case .object(let o) = fields else { return template }
        var out = template
        for (key, value) in o where out.contains("{\(key)}") {
            let shown: String
            switch value {
            case .string(let s): shown = idKeys.contains(key) ? label(ModelID(s)) : s
            case .int(let n): shown = String(n)
            case .uint(let n): shown = String(n)
            case .double(let d): shown = String(d)
            // Channel lists are 0-based in the file and 1-based on screen.
            case .array(let a):
                shown = a.compactMap { $0.doubleValue.map { String(Int($0) + 1) } }.joined(separator: ", ")
            default: shown = ""
            }
            out = out.replacingOccurrences(of: "{\(key)}", with: shown)
        }
        return out
    }

    private static let idKeys: Set<String> = ["cue", "target", "patch", "list"]

    /// Notice codes whose engine text quotes cue, list, patch or stage ids: the show file's repairs.
    static let idQuotingNotices: Set<String> = ["load_repair"]

    private static let idPattern = try? NSRegularExpression(
        pattern: "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")

    /// `text` with every internal id in it replaced by what `label` names it.
    static func namingIDs(_ text: String, _ label: Label) -> String {
        guard let idPattern else { return text }
        var out = text
        let whole = NSRange(out.startIndex..., in: out)
        for match in idPattern.matches(in: out, range: whole).reversed() {
            guard let range = Range(match.range, in: out) else { continue }
            out.replaceSubrange(range, with: label(ModelID(String(out[range]).lowercased())))
        }
        return out
    }
}

/// One line of the Warnings tab: a broken cue (with every reason it is broken) or a notice.
struct WarningRow: Hashable {
    let id: String
    /// The cue to select when the row is chosen.
    let cue: ModelID?
    let title: String
    let detail: String
    /// The cue is broken by media that cannot be found: the row offers Find Missing Files.
    let canRelink: Bool
}

enum WarningRows {
    /// The most of a stage's name a warning row shows before its reason.
    static let stageNameLength = 24

    /// The most of a device's name a warning row's title shows.
    static let deviceNameLength = 48

    /// Broken cues in list order first, then workspace warnings, then runtime warnings (oldest
    /// first). Groups that are only flagged for a broken child are not listed: the child is.
    ///
    /// The list says what is wrong, once: the engine reports every refusal and every loss (the
    /// show log keeps each), and here repeats become one entry. A refusal to start a broken cue
    /// is told on that cue's own entry, with how often it happened, and goes once the cue is
    /// playable again. A warning that comes again is one entry with its latest words and a count.
    /// A device's notices are one entry per device and problem, worded here from the notice's
    /// code; the one sent at each loss is not listed, as the entry in force says the same.
    @MainActor
    static func make(store: WorkspaceStore) -> [WarningRow] {
        let model = store.model
        let label = labeler(model)
        let notices = [("w", store.warnings), ("r", store.runtimeWarnings)].flatMap { prefix, list in
            list.map { (prefix, $0) }
        }
        var refusals: [ModelID: Int] = [:]
        for (_, w) in notices where w.type == "cue_broken" {
            if let id = w.json["cue"]?.stringValue { refusals[ModelID(id), default: 0] += 1 }
        }
        var rows: [WarningRow] = []
        func walk(_ cues: [Cue]) {
            for cue in cues {
                let problems = store.problems[cue.id] ?? []
                let run = store.cueStates[cue.id]
                let reason = run?.reason
                if !problems.isEmpty || reason != nil {
                    var reasons = problems.map { ProblemText.problem($0, label: label) }
                    if let reason { reasons.append(ProblemText.reason(reason, state: run?.state)) }
                    if let n = refusals[cue.id] {
                        reasons.append(
                            n == 1 ? loc("warnings.refused.one") : String(format: loc("warnings.refused.many"), n))
                    }
                    rows.append(
                        WarningRow(
                            id: "c:\(cue.id.raw)", cue: cue.id,
                            title: cueTitle(cue, name: model?.listName(cue) ?? CueKindStyle.kindLabel(cue.kindType)),
                            detail: reasons.joined(separator: "\n"),
                            canRelink: problems.contains { $0.type == "media_missing" }))
                }
                walk(cue.children)
            }
        }
        for list in model?.cueLists ?? [] { walk(list.cues) }
        // Entries by key in the order each first came, with the latest warning and how many came.
        var order: [String] = []
        var latest: [String: (warning: WireWarning, count: Int)] = [:]
        var devices: [String: (name: String, codes: [String])] = [:]
        // A patch routing past its device: one entry per device and patch, naming both.
        var beyond: [String: (device: String, patch: String)] = [:]
        for (prefix, w) in notices {
            // Told on the broken cue's own entry, or past: the cue plays again.
            if w.type == "cue_broken" { continue }
            // The system output with no output device selected: one entry, as a device's, titled
            // as the patch pop-up names that choice, the reason under it in our words.
            if w.type == "notice", w.json["code"]?.stringValue == "patch_no_system_output" {
                let key = "s:system-output"
                if latest[key] == nil {
                    order.append(key)
                    latest[key] = (w, 1)
                }
                continue
            }
            if w.type == "notice", let device = w.json["device"]?.stringValue,
                w.json["code"]?.stringValue == "patch_routing_beyond_device",
                let patch = w.json["patch"]?.stringValue
            {
                let key = "b:\(device):\(patch)"
                if beyond[key] == nil {
                    order.append(key)
                    let name = model?.patches.first { $0.id == ModelID(patch) }?.raw["name"]?.stringValue ?? ""
                    beyond[key] = (device, name.isEmpty ? loc("settings.audio.untitledPatch") : name)
                }
                continue
            }
            if w.type == "notice", let device = w.json["device"]?.stringValue {
                let code = w.json["code"]?.stringValue ?? ""
                if prefix == "r", code == "device_lost" { continue }
                let key = "d:\(deviceFamily(code)):\(device)"
                if devices[key] == nil { order.append(key) }
                devices[key, default: (device, [])].codes.append(code)
                continue
            }
            var fields = w.json
            // A late start is the same warning however late it was: the latest lateness shows.
            fields.set("late_ms", .null)
            let key = prefix + ((try? fields.text()) ?? w.type)
            if let seen = latest[key] {
                latest[key] = (w, seen.count + 1)
            } else {
                order.append(key)
                latest[key] = (w, 1)
            }
        }
        for key in order {
            if let device = devices[key] {
                rows.append(deviceRow(key: key, name: device.name, codes: device.codes))
            } else if let b = beyond[key] {
                rows.append(
                    WarningRow(
                        id: key, cue: nil,
                        title: String(
                            format: loc("warnings.device"),
                            WorkspaceSettingsModel.middleCut(b.device, limit: deviceNameLength)),
                        detail: String(format: loc("warnings.patchRoutingBeyond"), b.patch), canRelink: false))
            } else if key == "s:system-output" {
                rows.append(
                    WarningRow(
                        id: key, cue: nil, title: loc("settings.audio.systemOutputNone"),
                        detail: loc("warnings.systemOutputNone"), canRelink: false))
            } else if let (w, count) = latest[key] {
                rows.append(warningRow(key: key, w, count: count, model: model, label: label))
            }
        }
        for text in store.appNotices {
            rows.append(WarningRow(id: "k:\(text)", cue: nil, title: text, detail: "", canRelink: false))
        }
        // A stage that cannot be seen on this computer (VID-01): no display chosen, its display
        // unplugged, or failing.
        for entry in store.videoOutputs.stages {
            guard let state = entry.state, let why = Self.stageReason(state) else { continue }
            // A stage gone from the workspace since the engine's last word has no row. The title is
            // the stage's name (a long one shortened); the detail a reason short enough for one line.
            guard let full = model?.stages.first(where: { $0.id == entry.stage })?.raw["name"]?.stringValue
            else { continue }
            let name = full.count > Self.stageNameLength ? full.prefix(Self.stageNameLength - 1) + "…" : full
            rows.append(
                WarningRow(
                    id: "v:\(entry.stage.raw)", cue: nil, title: String(format: loc("warnings.stage"), String(name)),
                    detail: why, canRelink: false))
        }
        return rows
    }

    /// The device problems that are one entry: a device lost and a patch whose device is not
    /// connected say the same thing.
    private static func deviceFamily(_ code: String) -> String {
        code == "patch_device_missing" ? "device_lost" : code
    }

    /// One device's entry: its name as the title, what is wrong and what to do under it.
    @MainActor
    private static func deviceRow(key: String, name: String, codes: [String]) -> WarningRow {
        // A lost device says more than a patch's device not being there.
        let code = codes.contains("device_lost") ? "device_lost" : codes.first ?? ""
        return WarningRow(
            id: key, cue: nil,
            title: String(
                format: loc("warnings.device"), WorkspaceSettingsModel.middleCut(name, limit: deviceNameLength)),
            detail: ProblemText.text(for: "warnings.device.\(code)") ?? loc("warnings.device.other"),
            canRelink: false)
    }

    /// A workspace or runtime warning's entry, `count` the times it came.
    private static func warningRow(
        key: String, _ w: WireWarning, count: Int, model: WorkspaceModel?, label: ProblemText.Label
    ) -> WarningRow {
        func counted(_ text: String) -> String {
            count == 1 ? text : String(format: loc("warnings.repeated"), text, count)
        }
        if let id = w.json["cue"]?.stringValue.map({ ModelID($0) }) {
            // A warning about a cue reads as a broken cue's row: the cue as the title, what
            // happened on the line under it, so a long cue name never cuts the reason off. A cue
            // gone from the workspace is never named by its id.
            let cue = model?.cue(id)
            let title =
                cue.map { cueTitle($0, name: model?.listName($0) ?? CueKindStyle.kindLabel($0.kindType)) }
                ?? loc("warnings.cueGone")
            return WarningRow(
                id: key, cue: cue == nil ? nil : id, title: title,
                detail: counted(ProblemText.warningDetail(w, label: label)), canRelink: false)
        }
        return WarningRow(
            id: key, cue: nil, title: counted(ProblemText.warning(w, label: label)), detail: "", canRelink: false)
    }

    /// Why a stage cannot be seen, in a few words for the Warnings list; nil for one that is fine.
    static func stageReason(_ state: StageState) -> String? {
        switch state.state {
        case "no_display": return loc("warnings.stage.noDisplay")
        case "display_missing": return loc("warnings.stage.missing")
        case "failed": return loc("warnings.stage.failed")
        default: return nil
        }
    }

    /// Names an id in problem and warning text: a cue's number and name, a patch or list name.
    static func labeler(_ model: WorkspaceModel?) -> ProblemText.Label {
        { id in
            if let model, let cue = model.cue(id) { return cueTitle(cue, name: model.listName(cue)) }
            let patches = (model?.patches ?? []) + (model?.networkPatches ?? [])
            if let name = patches.first(where: { $0.id == id })?.raw["name"]?.stringValue { return name }
            if let list = model?.cueLists.first(where: { $0.id == id }) { return list.name }
            if let stage = model?.stages.first(where: { $0.id == id })?.raw["name"]?.stringValue { return stage }
            // Never the id itself: it means nothing to the operator.
            return loc("warnings.unknownItem")
        }
    }

    static func cueTitle(_ cue: Cue, name: String) -> String {
        return [cue.number, name].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: "  ")
    }
}
