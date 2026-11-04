// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// What a workspace window remembers about itself on this machine (UI-02): its frame, the two
/// split positions, which areas are shown and the sidebar's tab. It is UI-owned layout: it lives
/// in UserDefaults keyed by the workspace file's path and never in the show file. Every field is
/// optional so damaged or partial stored data costs only the fields that are bad.
struct WindowLayout: Equatable {
    var frame: NSRect?
    var inspectorHeight: CGFloat?
    var sidebarWidth: CGFloat?
    var shown: [LayoutArea: Bool] = [:]
    var sidebarTab: Int?

    /// Sizes beyond this are nonsense, whatever the stored data says.
    static let sizeLimit: CGFloat = 20_000
    static let sidebarTabCount = 3

    init() {}

    /// Reads whatever `value` holds; a field of the wrong type, non-finite, negative or absurd is skipped.
    init(stored value: Any?) {
        guard let dict = value as? [String: Any] else { return }
        if let numbers = dict["frame"] as? [Any], numbers.count == 4 {
            let n = numbers.map { Self.number($0) }
            if let x = n[0], let y = n[1], let w = n[2], let h = n[3], w > 0, h > 0, w <= Self.sizeLimit,
                h <= Self.sizeLimit, abs(x) <= 10 * Self.sizeLimit, abs(y) <= 10 * Self.sizeLimit
            {
                frame = NSRect(x: x, y: y, width: w, height: h)
            }
        }
        inspectorHeight = Self.size(dict["inspectorHeight"])
        sidebarWidth = Self.size(dict["sidebarWidth"])
        if let areas = dict["shown"] as? [String: Any] {
            for area in LayoutArea.allCases {
                if let on = areas[area.storedName] as? Bool { shown[area] = on }
            }
        }
        if let t = dict["sidebarTab"] as? Int, (0..<Self.sidebarTabCount).contains(t) { sidebarTab = t }
    }

    /// The property-list form written to UserDefaults.
    var stored: [String: Any] {
        var out: [String: Any] = [:]
        if let f = frame { out["frame"] = [f.origin.x, f.origin.y, f.size.width, f.size.height] }
        if let h = inspectorHeight { out["inspectorHeight"] = Double(h) }
        if let w = sidebarWidth { out["sidebarWidth"] = Double(w) }
        if !shown.isEmpty {
            out["shown"] = Dictionary(uniqueKeysWithValues: shown.map { ($0.key.storedName, $0.value) })
        }
        if let t = sidebarTab { out["sidebarTab"] = t }
        return out
    }

    private static func number(_ v: Any?) -> CGFloat? {
        guard let d = (v as? NSNumber)?.doubleValue, d.isFinite else { return nil }
        return CGFloat(d)
    }

    private static func size(_ v: Any?) -> CGFloat? {
        guard let n = number(v), n > 0, n <= sizeLimit else { return nil }
        return n
    }

    /// `frame` moved and shrunk to lie wholly on one of `screens` (their visible frames), at least
    /// `minSize` where the screen allows. Nil when there is no screen to judge by.
    static func fit(_ frame: NSRect, screens: [NSRect], minSize: NSSize) -> NSRect? {
        guard let first = screens.first else { return nil }
        func overlap(_ s: NSRect) -> CGFloat {
            let i = s.intersection(frame)
            return i.isNull ? 0 : i.width * i.height
        }
        let best = screens.max { overlap($0) < overlap($1) }.flatMap { overlap($0) > 0 ? $0 : nil }
        let screen = best ?? first
        let width = min(max(frame.width, minSize.width), screen.width)
        let height = min(max(frame.height, minSize.height), screen.height)
        var out = NSRect(x: frame.minX, y: frame.minY, width: width, height: height)
        // On no screen at all (its display is gone): centred on the main one.
        if best == nil { out.origin = NSPoint(x: screen.midX - width / 2, y: screen.midY - height / 2) }
        out.origin.x = min(max(out.minX, screen.minX), screen.maxX - width)
        out.origin.y = min(max(out.minY, screen.minY), screen.maxY - height)
        return out
    }
}

extension LayoutArea {
    var storedName: String {
        switch self {
        case .toolbar: return "toolbar"
        case .masthead: return "masthead"
        case .inspector: return "inspector"
        case .sidebar: return "sidebar"
        }
    }
}

/// The saved layouts of every workspace on this machine: one UserDefaults entry keyed by file path.
struct WindowLayoutStore {
    static let key = "windowLayouts"
    /// Past this many files one other entry goes, so the entry cannot grow without bound.
    static let limit = 200
    let defaults: UserDefaults

    /// The entry's name: the path as the filesystem resolves it, so the path a file was asked for
    /// and the one the engine reports name the same entry.
    func key(for path: String) -> String {
        URL(fileURLWithPath: path).resolvingSymlinksInPath().path
    }

    func layout(forPath path: String) -> WindowLayout { layout(forEntry: key(for: path)) }

    func save(_ layout: WindowLayout, forPath path: String) { save(layout, forEntry: key(for: path)) }

    /// The same by an entry name already resolved by `key(for:)`: resolving can touch the disk, so
    /// the window resolves once and saves by entry.
    func layout(forEntry entry: String) -> WindowLayout {
        WindowLayout(stored: defaults.dictionary(forKey: Self.key)?[entry])
    }

    func save(_ layout: WindowLayout, forEntry entry: String) {
        var all = defaults.dictionary(forKey: Self.key) ?? [:]
        all[entry] = layout.stored
        if all.count > Self.limit, let drop = all.keys.filter({ $0 != entry }).sorted().first { all[drop] = nil }
        defaults.set(all, forKey: Self.key)
    }
}
