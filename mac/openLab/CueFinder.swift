// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// Find (ED-09): which cues of one cue list match a typed text, and where Find Next / Previous go.
/// Pure: the window controller owns the state and the bar.
enum CueFinder {
    /// The cues of `roots` (depth first, in list order, collapsed groups included) whose number,
    /// name, notes, audio file name or Network message contain `query`, ignoring case. Blank text finds nothing.
    static func matches(_ query: String, in roots: [CueNode]) -> [ModelID] {
        let text = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return [] }
        var found: [ModelID] = []
        func walk(_ nodes: [CueNode]) {
            for node in nodes {
                if matches(text, cue: node.cue) { found.append(node.id) }
                walk(node.children)
            }
        }
        walk(roots)
        return found
    }

    /// Every cue id of `roots` in the order Find walks them.
    static func order(of roots: [CueNode]) -> [ModelID] {
        roots.flatMap { [$0.id] + order(of: $0.children) }
    }

    static func matches(_ text: String, cue: Cue) -> Bool {
        let fields =
            [cue.number ?? "", cue.name, cue.raw["notes"]?.stringValue ?? "", fileName(of: cue)] + message(of: cue)
        return fields.contains { $0.range(of: text, options: [.caseInsensitive, .diacriticInsensitive]) != nil }
    }

    /// A Network cue's address and arguments (QLab searches the contents of Network cues). The
    /// model has no Text, Video or Script cues yet; their contents join here when it does.
    private static func message(of cue: Cue) -> [String] {
        guard cue.kindType == "network", let kind = cue.raw["kind"] else { return [] }
        let args = (kind["args"]?.arrayValue ?? []).compactMap { arg -> String? in
            guard let value = arg["value"] else { return nil }
            return value.stringValue ?? (try? value.text())
        }
        return [kind["address"]?.stringValue ?? ""] + args
    }

    /// The last `/` component of an Audio cue's file (as the cue list shows it), else empty.
    private static func fileName(of cue: Cue) -> String {
        guard cue.kindType == "audio", let media = cue.raw["kind"]?["media"]?.stringValue else { return "" }
        return media.split(separator: "/", omittingEmptySubsequences: true).last.map(String.init) ?? ""
    }

    /// The index in `matches` that Next (`forward`) or Previous goes to from `current` (nil: nothing
    /// found yet), wrapping at both ends. With nothing selected yet Next takes the first match after
    /// the cue in `anchor` (the list order `order`), Previous the last one before it.
    static func step(
        _ matches: [ModelID], current: Int?, forward: Bool, anchor: ModelID?, order: [ModelID]
    ) -> Int? {
        guard !matches.isEmpty else { return nil }
        let count = matches.count
        if let current, matches.indices.contains(current) {
            return (current + (forward ? 1 : count - 1)) % count
        }
        guard let anchor, let at = order.firstIndex(of: anchor) else { return forward ? 0 : count - 1 }
        let position = Dictionary(order.enumerated().map { ($1, $0) }, uniquingKeysWith: { a, _ in a })
        if forward { return matches.firstIndex { (position[$0] ?? -1) > at } ?? 0 }
        return matches.lastIndex { (position[$0] ?? Int.max) < at } ?? count - 1
    }
}
