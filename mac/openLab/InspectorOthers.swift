// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The level "duck others" changes the other cues by, as its field shows and reads it. Unlike a
/// level box a number without a sign is taken as written: a positive number boosts (QLab 5, manual
/// p. 130). -inf (or anything at -120 or below) silences them; above +12 is held at +12.
enum OthersLevel {
    static let range = LevelText.silence...LevelText.maxDb

    static func format(_ db: Double) -> String {
        if db.isNaN || db <= LevelText.silence { return "-inf" }
        if db == db.rounded() { return String(Int(db)) }
        return String(format: "%.1f", db)
    }

    /// nil when `text` is not a level.
    static func parse(_ text: String) -> Double? {
        var t = text.trimmingCharacters(in: .whitespaces).lowercased().replacingOccurrences(of: ",", with: ".")
        t = t.replacingOccurrences(of: "db", with: "").trimmingCharacters(in: .whitespaces)
        if t == "-inf" || t == "-∞" { return LevelText.silence }
        guard !t.isEmpty, let v = Double(t), v.isFinite else { return nil }
        return min(max(v, range.lowerBound), range.upperBound)
    }

    /// Whether the field's wording is a boost: the value typed so far is above 0 dB.
    static func isBoost(_ text: String) -> Bool { (parse(text) ?? 0) > 0 }
}

extension InspectorEdit {
    /// The cue's "fade and stop others", with the defaults of a show that does not have it.
    static func fadeOthers(_ cue: Cue) -> (enabled: Bool, scope: String, duration: Double) {
        let v = cue.raw["fade_and_stop_others"]
        return (
            v?["enabled"] == .bool(true), v?["scope"]?.stringValue ?? "peers", v?["duration"]?.doubleValue ?? 1
        )
    }

    /// The cue's "duck others", with the defaults of a show that does not have it.
    static func duckOthers(_ cue: Cue) -> (enabled: Bool, db: Double, duration: Double) {
        let v = cue.raw["duck_others"]
        return (v?["enabled"] == .bool(true), v?["db"]?.doubleValue ?? -12, v?["duration"]?.doubleValue ?? 1)
    }

    /// One of the Triggers tab's typed fields for other cues: that value set, the rest of the cue's
    /// own setting kept.
    static func othersCommand(_ field: InspectorField, text: String, cue: Cue) -> Result<Command?, Rejection> {
        let fade = fadeOthers(cue)
        let duck = duckOthers(cue)
        switch field {
        case .fadeOthersTime:
            switch time(text) {
            case .failure(let r): return .failure(r)
            case .success(let s):
                if s == fade.duration { return .success(nil) }
                return .success(
                    .setProperty(cue.id, .fadeAndStopOthers(enabled: fade.enabled, scope: fade.scope, duration: s)))
            }
        case .duckTime:
            switch time(text) {
            case .failure(let r): return .failure(r)
            case .success(let s):
                if s == duck.duration { return .success(nil) }
                return .success(.setProperty(cue.id, .duckOthers(enabled: duck.enabled, db: duck.db, duration: s)))
            }
        default:
            guard let db = OthersLevel.parse(text) else { return .failure(.invalidLevel) }
            if db == duck.db { return .success(nil) }
            return .success(.setProperty(cue.id, .duckOthers(enabled: duck.enabled, db: db, duration: duck.duration)))
        }
    }
}

/// The Triggers tab's right side: what the cue does to other cues when its action starts (QLab 5,
/// manual pp. 129-130), then what a second trigger does.
extension InspectorViewController {
    func othersColumn(row: (String, NSView) -> NSView) -> NSStackView {
        fadeOthersCheck.setAX("inspector-fade-others")
        fadeOthersScopePopup.setAX("inspector-fade-others-scope")
        fadeOthersTimeField.setAX("inspector-fade-others-time", label: loc("inspector.fadeOthers.time"))
        duckCheck.setAX("inspector-duck-others")
        duckDbField.setAX("inspector-duck-others-db", label: loc("inspector.duckOthers.db"))
        duckTimeField.setAX("inspector-duck-others-time", label: loc("inspector.duckOthers.time"))
        for key in Self.fadeOthersScopes {
            fadeOthersScopePopup.menu?.addItem(
                NSMenuItem(title: loc("inspector.fadeOthers.\(key)"), action: nil, keyEquivalent: ""))
        }
        fadeOthersScopePopup.controlSize = .small
        fadeOthersScopePopup.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        fadeOthersScopePopup.target = self
        fadeOthersScopePopup.action = #selector(othersChanged(_:))
        for c in [fadeOthersCheck, duckCheck] {
            c.target = self
            c.action = #selector(othersChanged(_:))
        }
        for (f, width) in [(fadeOthersTimeField, 80.0), (duckTimeField, 80.0), (duckDbField, 52.0)] {
            f.widthAnchor.constraint(equalToConstant: width).isActive = true
        }
        // The wording follows the sign: kept to one line, so a long translation truncates.
        duckCheck.lineBreakMode = .byTruncatingTail
        duckCheck.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        func small(_ text: String) -> NSTextField {
            let label = NSTextField(labelWithString: text)
            label.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            label.textColor = .secondaryLabelColor
            return label
        }
        func line(_ views: [NSView]) -> NSStackView {
            let s = NSStackView(views: views)
            s.spacing = 6
            s.alignment = .firstBaseline
            return s
        }
        let heading = small(loc("inspector.others.heading"))
        let fadeRow = line([
            fadeOthersCheck, fadeOthersScopePopup, small(loc("inspector.others.over")), fadeOthersTimeField,
        ])
        let duckBy = line([
            small(loc("inspector.duckOthers.by")), duckDbField, small(loc("inspector.duckOthers.dbOver")),
            duckTimeField,
        ])
        duckBy.edgeInsets = NSEdgeInsets(top: 0, left: 20, bottom: 0, right: 0)
        let column = NSStackView(views: [
            heading, fadeRow, duckCheck, duckBy, row(loc("inspector.secondTrigger"), secondTriggerPopup),
        ])
        column.orientation = .vertical
        column.alignment = .leading
        column.spacing = 6
        return column
    }

    /// The right side from one cue: the boxes, the choice, the values (kept while off and drawn off).
    func renderOthers(_ cue: Cue) {
        let fade = InspectorEdit.fadeOthers(cue)
        let duck = InspectorEdit.duckOthers(cue)
        fadeOthersCheck.state = fade.enabled ? .on : .off
        fadeOthersScopePopup.selectItem(at: Self.fadeOthersScopes.firstIndex(of: fade.scope) ?? 0)
        duckCheck.state = duck.enabled ? .on : .off
        enableOthers(fade: fade.enabled, duck: duck.enabled)
        if !isEditing(.duckDb) { relabelDuck(OthersLevel.format(duck.db)) }
    }

    /// Several cues: a value that differs is mixed; a part is on while any cue has it on.
    func renderBatchOthers(_ cues: [Cue]) {
        let fades = cues.map(InspectorEdit.fadeOthers)
        let ducks = cues.map(InspectorEdit.duckOthers)
        showTicks(fadeOthersCheck, fades.map(\.enabled))
        showChoice(fadeOthersScopePopup, Self.fadeOthersScopes, fades.map(\.scope))
        showTicks(duckCheck, ducks.map(\.enabled))
        enableOthers(fade: fades.contains { $0.enabled }, duck: ducks.contains { $0.enabled })
        if !isEditing(.duckDb) { relabelDuck(duckDbField.stringValue) }
    }

    private func enableOthers(fade: Bool, duck: Bool) {
        fadeOthersScopePopup.isEnabled = fade && !isLocked
        fadeOthersTimeField.isEnabled = fade && !isLocked
        duckDbField.isEnabled = duck && !isLocked
        duckTimeField.isEnabled = duck && !isLocked
    }

    /// The box says duck at or below 0 dB and boost above, from the text in the field (typed or not).
    func relabelDuck(_ text: String) {
        let title = loc(OthersLevel.isBoost(text) ? "inspector.boostOthers" : "inspector.duckOthers")
        if duckCheck.title != title { duckCheck.title = title }
    }

    /// A box or the scope: only that part changes, on each cue from its own setting (a mixed one is
    /// not written back).
    @objc func othersChanged(_ sender: NSControl) {
        guard !isLocked else { return }
        if sender === fadeOthersScopePopup {
            let i = fadeOthersScopePopup.indexOfSelectedItem
            guard Self.fadeOthersScopes.indices.contains(i) else { return }
            let scope = Self.fadeOthersScopes[i]
            sendEach(key: "fadeOthers") { cue in
                let f = InspectorEdit.fadeOthers(cue)
                guard f.scope != scope else { return nil }
                return .setProperty(cue.id, .fadeAndStopOthers(enabled: f.enabled, scope: scope, duration: f.duration))
            }
        } else if let check = sender as? NSButton {
            let on = check.state != .off
            check.allowsMixedState = false
            check.state = on ? .on : .off
            if check === fadeOthersCheck {
                sendEach(key: "fadeOthers") { cue in
                    let f = InspectorEdit.fadeOthers(cue)
                    guard f.enabled != on else { return nil }
                    return .setProperty(cue.id, .fadeAndStopOthers(enabled: on, scope: f.scope, duration: f.duration))
                }
            } else {
                sendEach(key: "duckOthers") { cue in
                    let d = InspectorEdit.duckOthers(cue)
                    guard d.enabled != on else { return nil }
                    return .setProperty(cue.id, .duckOthers(enabled: on, db: d.db, duration: d.duration))
                }
            }
        }
        render()
    }
}
