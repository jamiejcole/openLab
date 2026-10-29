// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// One line of the pre-show check report (SM-06): a section heading or one entry of the engine's
/// `preshow_check` answer.
struct PreshowRow: Hashable {
    enum Section: String, CaseIterable {
        case brokenCues, media, devices, network, system
        var titleKey: String { "preshow.section.\(rawValue)" }
    }

    let section: Section
    /// A section heading (not selectable); its `title` is the heading and the entry count.
    let isHeading: Bool
    let title: String
    let detail: String
    /// The cue to select when the row is chosen; nil when it has none or no longer exists.
    let cue: ModelID?
    /// The inspector tab that holds what to fix (`basics` for files and targets); nil: leave it.
    let inspectorTab: String?
    /// False for entries listed for information only (a spare patch, an unchecked destination,
    /// `preload_over_budget`): shown dimmed, not counted.
    let isProblem: Bool
}

/// Turns a `PreshowReport` into rows. Sections come in a fixed order (broken cues, media, devices,
/// network, system), empty ones are left out, and entries keep the engine's order (it sorts every
/// list for display, docs/protocol.md). Cue names come from the app's model, so a cue renamed
/// since the check shows its new name; a cue the report names that the model no longer has is
/// marked deleted and cannot be selected.
enum PreshowRows {
    /// The most cues an entry names before "and N more" (a file used by hundreds of cues).
    static let maxNamedCues = 5

    @MainActor
    static func make(report: PreshowReport, model: WorkspaceModel?) -> [PreshowRow] {
        let index = cueIndex(model)
        let modelLabel = WarningRows.labeler(model)
        // The index is O(1); the labeler searches the tree, so cues go through the index first.
        let label: ProblemText.Label = { id in index[id].map(WarningRows.cueTitle) ?? modelLabel(id) }
        func existing(_ id: ModelID) -> ModelID? { index[id] == nil ? nil : id }
        func usedBy(_ cues: [ModelID]) -> String {
            let named = cues.prefix(maxNamedCues).map(label).joined(separator: ", ")
            let text =
                cues.count > maxNamedCues
                ? String(format: loc("preshow.usedBy.more"), named, cues.count - maxNamedCues) : named
            return String(format: loc("preshow.usedBy"), text)
        }

        var sections: [PreshowRow.Section: [PreshowRow]] = [:]
        func add(
            _ section: PreshowRow.Section, _ title: String, _ detail: String, cue: ModelID?, tab: String?,
            problem: Bool = true
        ) {
            sections[section, default: []].append(
                PreshowRow(
                    section: section, isHeading: false, title: title, detail: detail, cue: cue, inspectorTab: tab,
                    isProblem: problem))
        }

        for c in report.brokenCues {
            let title: String
            if let cue = index[c.cue] {
                title = WarningRows.cueTitle(cue)
            } else {
                let reported = [c.number, c.name].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: "  ")
                title = String(format: loc("preshow.cue.deleted"), reported.isEmpty ? c.cue.raw : reported)
            }
            let fixedInBasics = c.problems.contains { basicsProblems.contains($0.type) }
            add(
                .brokenCues, title, c.problems.map { ProblemText.problem($0, label: label) }.joined(separator: "\n"),
                cue: existing(c.cue), tab: fixedInBasics ? "basics" : nil)
        }
        for m in report.media {
            let status: String
            switch m.status {
            case .missing: status = loc("preshow.media.missing")
            case .unsupported: status = String(format: loc("preshow.media.unsupported"), m.reason ?? "")
            default: status = m.status.raw
            }
            add(
                .media, m.path, status + "\n" + usedBy(m.cues), cue: m.cues.lazy.compactMap(existing).first,
                tab: "basics")
        }
        for p in report.patches {
            let title = p.name ?? String(format: loc("preshow.patch.unknown"), p.patch.raw)
            var lines: [String] = []
            if p.name == nil {
                lines.append(loc("preshow.patch.missing"))
            } else if let r = p.resolution {
                let problem: JSONValue = .object([
                    "type": .string("device_unavailable"), "patch": .string(p.patch.raw), "resolution": r.json,
                ])
                lines.append(problemText(problem, label: label) ?? r.type)
            } else {
                lines.append(loc("preshow.patch.noDevice"))
            }
            lines.append(p.cues.isEmpty ? loc("preshow.patch.unused") : usedBy(p.cues))
            add(
                .devices, title, lines.joined(separator: "\n"), cue: p.cues.lazy.compactMap(existing).first, tab: nil,
                problem: !p.cues.isEmpty)
        }
        for t in report.network {
            let title = t.name ?? String(format: loc("preshow.network.unknown"), t.patch.raw)
            var lines: [String] = []
            let missing = t.status == .missing
            if missing {
                lines.append(loc("preshow.network.missing"))
            } else if t.status == .notChecked {
                var address = t.host ?? ""
                if let port = t.port { address += ":\(port)" }
                if let proto = t.protocol { address += " (\(proto))" }
                lines.append(String(format: loc("preshow.network.notChecked"), address))
            } else {
                lines.append(t.status.raw)
            }
            lines.append(usedBy(t.cues))
            add(
                .network, title, lines.joined(separator: "\n"), cue: t.cues.lazy.compactMap(existing).first, tab: nil,
                problem: missing)
        }
        for w in report.system {
            let cue = w.json["cue"]?.stringValue.map { ModelID($0) }
            add(
                .system, ProblemText.warning(w, label: label), "", cue: cue.flatMap(existing), tab: nil,
                problem: w.json["code"]?.stringValue != "preload_over_budget")
        }

        var rows: [PreshowRow] = []
        for section in PreshowRow.Section.allCases {
            guard let entries = sections[section], !entries.isEmpty else { continue }
            rows.append(
                PreshowRow(
                    section: section, isHeading: true,
                    title: String(format: loc(section.titleKey), entries.count), detail: "", cue: nil,
                    inspectorTab: nil, isProblem: false))
            rows += entries
        }
        return rows
    }

    /// The one-line result above the list.
    static func summary(_ report: PreshowReport) -> String {
        if report.ok { return loc("preshow.summary.ok") }
        let total = Int(report.summary.total)
        // `ok` false with nothing counted: a cause listed on its own (a missing destination).
        if total == 0 { return loc("preshow.summary.problems") }
        return total == 1 ? loc("preshow.summary.one") : String(format: loc("preshow.summary.many"), total)
    }

    /// Problems fixed on the Basics tab: the file and the target are chosen there.
    private static let basicsProblems: Set<String> = [
        "no_target", "target_missing", "target_self", "target_wrong_kind", "no_media", "media_missing",
        "media_unsupported",
    ]

    private static func problemText(_ json: JSONValue, label: ProblemText.Label) -> String? {
        guard let text = try? json.text(), let p = try? JSONDecoder().decode(WireProblem.self, from: Data(text.utf8))
        else { return nil }
        return ProblemText.problem(p, label: label)
    }

    private static func cueIndex(_ model: WorkspaceModel?) -> [ModelID: Cue] {
        var index: [ModelID: Cue] = [:]
        func walk(_ cues: [Cue]) {
            for c in cues {
                index[c.id] = c
                walk(c.children)
            }
        }
        for list in model?.cueLists ?? [] { walk(list.cues) }
        return index
    }
}
