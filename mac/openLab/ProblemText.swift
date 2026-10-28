// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// Words for the engine's problems and warnings (DX-01). The engine sends a tagged variant; the
/// text comes from `Localizable.strings`, looked up by `problem.<type>` / `warning.<type>`, with
/// `.<reason>` (or `.<resolution type>`) appended when that more specific key exists. `{field}`
/// in a string is replaced by the variant's field of that name; ids show as cue or patch names.
/// A type this build has no text for shows its raw type name, never nothing.
enum ProblemText {
    typealias Label = (ModelID) -> String

    static func problem(_ p: WireProblem, label: Label) -> String {
        var fields = p.json
        // The resolution of a device problem is a variant of its own: its fields join the problem's.
        var specific: String?
        if let r = p.json["resolution"], case .object(let o) = r {
            specific = r["type"]?.stringValue
            for (k, v) in o where k != "type" { fields[k] = v }
        }
        return render("problem", p.type, specific: specific, fields: fields, label: label)
    }

    static func warning(_ w: WireWarning, label: Label) -> String {
        if w.type == "notice" {
            let code = w.json["code"]?.stringValue ?? ""
            if let text = text(for: "warning.notice.\(code)") { return fill(text, w.json, label) }
            // A notice only the binary knows: its own (English) message beats an opaque code.
            if let message = w.json["message"]?.stringValue, !message.isEmpty { return message }
        }
        return render("warning", w.type, specific: w.json["reason"]?.stringValue, fields: w.json, label: label)
    }

    static func reason(_ r: WireBrokenReason) -> String { loc("broken.\(r.rawValue)") }

    private static func render(
        _ family: String, _ type: String, specific: String?, fields: JSONValue, label: Label
    ) -> String {
        let base = "\(family).\(type)"
        let template = specific.flatMap { text(for: "\(base).\($0)") } ?? text(for: base)
        guard let template else { return fill(loc("\(family).unknown"), .object(["type": .string(type)]), label) }
        return fill(template, fields, label)
    }

    /// nil when the strings file has no entry (NSLocalizedString would hand back the key).
    private static func text(for key: String) -> String? {
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
}

/// One line of the Warnings tab: a broken cue (with every reason it is broken) or a notice.
struct WarningRow: Hashable {
    let id: String
    /// The cue to select when the row is chosen.
    let cue: ModelID?
    let title: String
    let detail: String
    /// The cue is broken by media that cannot be found: the row offers Relink.
    let canRelink: Bool
}

enum WarningRows {
    /// Broken cues in list order first, then workspace warnings, then runtime warnings (oldest
    /// first). Groups that are only flagged for a broken child are not listed: the child is.
    @MainActor
    static func make(store: WorkspaceStore) -> [WarningRow] {
        let model = store.model
        func label(_ id: ModelID) -> String {
            if let cue = model?.cue(id) { return cueTitle(cue) }
            let patches = (model?.patches ?? []) + (model?.networkPatches ?? [])
            if let name = patches.first(where: { $0.id == id })?.raw["name"]?.stringValue { return name }
            if let list = model?.cueLists.first(where: { $0.id == id }) { return list.name }
            return id.raw
        }
        var rows: [WarningRow] = []
        func walk(_ cues: [Cue]) {
            for cue in cues {
                let problems = store.problems[cue.id] ?? []
                let reason = store.cueStates[cue.id]?.reason
                if !problems.isEmpty || reason != nil {
                    var reasons = problems.map { ProblemText.problem($0, label: label) }
                    if let reason { reasons.append(ProblemText.reason(reason)) }
                    rows.append(
                        WarningRow(
                            id: "c:\(cue.id.raw)", cue: cue.id, title: cueTitle(cue),
                            detail: reasons.joined(separator: "\n"),
                            canRelink: problems.contains { $0.type == "media_missing" }))
                }
                walk(cue.children)
            }
        }
        for list in model?.cueLists ?? [] { walk(list.cues) }
        var seen: [String: Int] = [:]
        for (prefix, list) in [("w", store.warnings), ("r", store.runtimeWarnings)] {
            for w in list {
                let key = prefix + ((try? w.json.text()) ?? w.type)
                seen[key, default: 0] += 1
                rows.append(
                    WarningRow(
                        id: "\(key)#\(seen[key] ?? 0)", cue: w.json["cue"]?.stringValue.map { ModelID($0) },
                        title: ProblemText.warning(w, label: label), detail: "", canRelink: false))
            }
        }
        return rows
    }

    static func cueTitle(_ cue: Cue) -> String {
        let name = cue.listName
        return [cue.number, name].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: "  ")
    }
}
