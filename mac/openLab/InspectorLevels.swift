// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The Levels tab of an Audio cue (AUD-06, AUD-08) and the Fade tab of a Fade cue (CT-10, AUD-09).
/// Levels, routing, curve and stop flag are edited by sending the cue's whole `kind` (core's
/// `CueProperty::Kind`; there is no finer command, docs/commands.md), built from the model's kind with
/// only the edited fields changed; the Fade's duration has its own `duration` property. Dragging a
/// fader or a matrix cell sends every move with one `coalesce` key, so the whole drag is one undo step.
extension InspectorViewController {
    /// How many file channels the matrix shows when the cue's routing mentions fewer. The engine does
    /// not tell the UI a file's channel count yet, so a stereo file is assumed.
    static let defaultInputs = 2

    func buildLevelsTabs() {
        audioLevels.onEdit = { [weak self] set, gesture in self?.sendLevels(set, gesture: gesture) }
        fadeLevels.onEdit = { [weak self] set, gesture in self?.sendLevels(set, gesture: gesture) }
        audioLevels.onGestureEnd = { [weak self] in self?.render() }
        fadeLevels.onGestureEnd = { [weak self] in self?.render() }
        fadeLevels.onSetFromTarget = { [weak self] in self?.setFadeLevelsFromTarget() }
        fadeSettings.onChange = { [weak self] change in self?.sendFade(change) }
        fadeSettings.onInvalid = { [weak self] in
            self?.messageLabel.stringValue = loc("inspector.invalid.time")
        }

        let levels = NSTabViewItem(identifier: "levels")
        levels.label = loc("inspector.tab.levels")
        levels.view = padded(audioLevels.view)
        levelsItem = levels

        let fadeColumns = NSStackView(views: [fadeSettings.view, fadeLevels.view])
        fadeColumns.orientation = .horizontal
        fadeColumns.alignment = .top
        fadeColumns.spacing = 20
        fadeLevels.view.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let fade = NSTabViewItem(identifier: "fade")
        fade.label = loc("inspector.tab.fade")
        fade.view = padded(fadeColumns)
        fadeItem = fade
    }

    private func padded(_ content: NSView) -> NSView {
        let box = NSView()
        content.translatesAutoresizingMaskIntoConstraints = false
        box.addSubview(content)
        NSLayoutConstraint.activate([
            content.topAnchor.constraint(equalTo: box.topAnchor, constant: 8),
            content.leadingAnchor.constraint(equalTo: box.leadingAnchor, constant: 12),
            content.trailingAnchor.constraint(equalTo: box.trailingAnchor, constant: -12),
            content.bottomAnchor.constraint(equalTo: box.bottomAnchor, constant: -8),
        ])
        return box
    }

    /// Puts `item` in the tab view or takes it out.
    func syncTab(_ item: NSTabViewItem?, present wanted: Bool) {
        guard let item else { return }
        let present = tabView.tabViewItems.contains { $0 === item }
        if wanted, !present {
            tabView.addTabViewItem(item)
        } else if !wanted, present {
            tabView.removeTabViewItem(item)
        }
    }

    /// Shows the Levels tab for an Audio cue and the Fade tab for a Fade cue, and none otherwise.
    func renderLevels(_ cue: Cue, model: WorkspaceModel) {
        let kind = cue.raw["kind"] ?? .null
        let audio = cue.kindType == "audio"
        let fade = cue.kindType == "fade"
        syncTab(levelsItem, present: audio)
        syncTab(fadeItem, present: fade)
        let set = LevelSet(kind: kind)
        if audio, let set {
            audioLevels.show(set, inputs: max(Self.defaultInputs, set.inputExtent), outputs: set.outputs.count)
        }
        if fade {
            fadeSettings.show(kind: kind)
            let target = kind["target"]?.stringValue.flatMap { model.cue(ModelID($0)) }
            let extent = target.map { Self.levelExtent(of: $0) } ?? (inputs: 0, outputs: 0)
            fadeLevels.canSetFromTarget = target.flatMap { Self.levels(ofAudio: $0) } != nil
            if let set {
                fadeLevels.show(
                    set, inputs: max(Self.defaultInputs, set.inputExtent, extent.inputs),
                    outputs: max(set.outputExtent, extent.outputs))
            } else {
                fadeLevels.show(nil, inputs: 0, outputs: 0)
            }
        }
    }

    /// The file channels and cue outputs an Audio cue has, or the most any Audio cue inside a Group has.
    static func levelExtent(of cue: Cue) -> (inputs: Int, outputs: Int) {
        var inputs = 0
        var outputs = 0
        func visit(_ c: Cue) {
            if c.kindType == "audio", let s = LevelSet(kind: c.raw["kind"] ?? .null) {
                inputs = max(inputs, s.inputExtent)
                outputs = max(outputs, s.outputs.count)
            }
            c.children.forEach(visit)
        }
        visit(cue)
        return (inputs, outputs)
    }

    /// The levels of an Audio cue (nil for any other cue).
    static func levels(ofAudio cue: Cue) -> LevelSet? {
        cue.kindType == "audio" ? LevelSet(kind: cue.raw["kind"] ?? .null) : nil
    }

    // MARK: Sending

    private func sendKind(
        _ build: (JSONValue) -> JSONValue, key: String, coalesce: ((ModelID) -> String)? = nil
    ) {
        guard !isLocked, let cue = cueForEdit, let kind = cue.raw["kind"] else { return }
        let new = build(kind)
        guard new != kind else { return }
        messageLabel.stringValue = ""
        send(.setProperty(cue.id, .kind(new)), cue: cue.id, key: key, revert: [], coalesce: coalesce?(cue.id))
    }

    func sendLevels(_ set: LevelSet, gesture: Int?) {
        sendKind({ set.json(into: $0) }, key: "levels", coalesce: gesture.map { g in { "levels:\($0):\(g)" } })
    }

    func sendFade(_ change: FadeSettingsPanel.Change) {
        // The duration has its own property (M5.6b): the rest of the kind is not resent.
        if case .duration(let seconds) = change {
            guard !isLocked, let cue = cueForEdit else { return }
            messageLabel.stringValue = ""
            send(.setProperty(cue.id, .duration(seconds)), cue: cue.id, key: "duration", revert: [.duration])
            return
        }
        sendKind(
            { kind in
                var kind = kind
                switch change {
                case .duration: break  // sent above
                case .curve(let curve): kind.set("curve", .string(curve))
                case .stopTarget(let stop): kind.set("stop_target_when_done", .bool(stop))
                }
                return kind
            }, key: "fade")
    }

    /// "Set Levels from Target" (QLab, manual p. 200): the Fade moves every level of its Audio target
    /// to the level the target has now, so the fade starts from where the target is.
    func setFadeLevelsFromTarget() {
        guard let cue = cue, let model = store.model,
            let id = cue.raw["kind"]?["target"]?.stringValue, let target = model.cue(ModelID(id)),
            let source = Self.levels(ofAudio: target), var set = fadeLevels.levels
        else { return }
        set.master = source.master
        set.outputs = source.outputs.map { LevelSet.Output(db: $0.db) }
        set.crosspoints = source.crosspoints.filter { $0.value.db > LevelText.silence }
            .mapValues { LevelSet.Cross(db: $0.db) }
        fadeLevels.replace(with: set)
    }
}
