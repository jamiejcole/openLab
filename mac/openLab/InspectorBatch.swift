// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Several cues selected (ED-04; QLab 5, manual p. 123): the inspector stays live and edits what the
/// cues share. Only tabs holding fields that can be set on all of them are shown: Basics and Triggers
/// for cues of different kinds, and some of the kind's own tabs when all are of one kind. A value that
/// is not the same on every cue is drawn as mixed (an empty field, a dash, a mixed tick box); an edit
/// sets that one value on every selected cue, the rest of each cue kept, as one undo step. A cue's
/// number, hotkey and OSC address name one cue, so they are not edited here.
extension InspectorViewController {
    /// The kind tabs that hold fields set on several cues at once. Time & Loops (each cue's own file)
    /// and a Fade's curve and levels (each fades its own target) are edited one cue at a time.
    static let batchKindTabs: [String: [String]] = [
        "audio": ["io", "levels"], "group": ["mode"], "load": ["loadTime"], "devamp": ["devamp"],
        "network": ["network"],
    ]

    /// The tabs shown for several selected cues of `kinds`.
    static func batchTabs(forKinds kinds: Set<String>) -> [String] {
        let common = ["basics", "triggers"]
        guard kinds.count == 1, let kind = kinds.first else { return common }
        let own = Set(batchKindTabs[kind] ?? [])
        return InspectorTabs.ids(forKind: kind).filter { common.contains($0) || own.contains($0) }
    }

    /// Several cues are selected.
    var isBatch: Bool { selection.count > 1 }

    /// The selected cues still in the show, each with its unanswered edits (several selected only).
    var batchCues: [Cue] {
        guard isBatch, let model = store.model else { return [] }
        return selection.compactMap { model.cue($0) }.map(withPendingEdits)
    }

    /// The cues an edit is for: the one shown, or every selected cue.
    var editCues: [Cue] { isBatch ? batchCues : (cueForEdit.map { [$0] } ?? []) }

    /// The controls only one cue at a time can set: off while several are selected.
    var singleCueControls: [NSControl] {
        [numberField, oscField, hotkeyField, hotkeyClear, networkSendButton, chooseButton, clearTargetButton]
    }

    // MARK: Showing several cues

    func renderBatch(_ cues: [Cue], model: WorkspaceModel, force: Set<InspectorField>) {
        tabView.isHidden = false
        messageLabel.isHidden = false
        emptyLabel.isHidden = true
        let kinds = Set(cues.map(\.kindType))
        let kind = kinds.count == 1 ? kinds.first : nil
        for f in InspectorField.allCases where force.contains(f) || !isEditing(f) {
            let shown = Set(
                cues.map { InspectorEdit.display(f, cue: $0, model: model, media: store.mediaDurations[$0.id]) })
            let text = shown.count == 1 ? shown.first ?? "" : ""
            if f == .notes {
                if notesView.string != text { notesView.string = text }
            } else if let tf = field(f) {
                if tf.stringValue != text { tf.stringValue = text }
                tf.placeholderString = shown.count > 1 ? loc("inspector.mixed") : nil
            }
        }
        let style = kind.flatMap { _ in CueKindStyle.typeStyle(kind: cues.first?.raw["kind"]) }
        kindIcon.image = style.map { $0.icon.image($0.label, compact: true) }
        renderBatchTarget(cues, kinds: kinds)
        for c in singleCueControls { c.isEnabled = false }
        let modes = cues.map { $0.raw["continue_mode"]?.stringValue ?? "do_not_continue" }
        durationField.isEnabled = kinds.isSubset(of: InspectorEdit.durationKinds)
        postWaitField.isEnabled = modes.contains { $0 != "auto_follow" }
        loadTimeField.isEnabled = kind == "load"
        networkMessageField.isEnabled = kind == "network"
        syncTabs(Self.batchTabs(forKinds: kinds))
        showChoice(colourPopup, Self.colours, cues.map { $0.raw["color"]?.stringValue ?? "none" })
        showChoice(continuePopup, Self.continueModes, modes)
        showChoice(
            secondTriggerPopup, Self.secondTriggers, cues.map { $0.raw["second_trigger"]?.stringValue ?? "nothing" })
        hotkeyField.stringValue = ""
        onReleaseCheck.isEnabled = true
        showTicks(onReleaseCheck, cues.map { $0.raw["second_trigger_on_release"] == .bool(true) })
        showTicks(flaggedCheck, cues.map { $0.raw["flagged"] == .bool(true) })
        showTicks(armedCheck, cues.map { $0.raw["armed"] != .bool(false) })
        if let first = cues.first {
            switch kind {
            case "group": renderBatchMode(cues, first: first)
            case "devamp": renderBatchDevamp(cues, first: first)
            case "audio": renderBatchAudio(cues, first: first, model: model)
            case "network":
                renderNetwork(first, model: model)
                // A destination that is not a patch is the last item ("no patch"), as for one cue.
                let known = Set(model.networkPatches.map(\.id.raw))
                let chosen = cues.map { c -> String in
                    let d = c.raw["kind"]?["destination"]?.stringValue ?? ""
                    return known.contains(d) ? d : ""
                }
                showChoice(networkPatchPopup, networkPatchIDs.map { $0?.raw }, chosen, fallback: networkPatchIDs.count - 1)
            default: break
            }
        }
        updatePreviewButton()
        if isLocked { applyLock() }
    }

    /// The Target row: the shared target's number (or file) or mixed; typed only when every cue acts
    /// on a cue. Choose, clear and jump act on one cue.
    private func renderBatchTarget(_ cues: [Cue], kinds: Set<String>) {
        let cueTargets = kinds.isSubset(of: InspectorEdit.targetKinds)
        let files = kinds == ["audio"]
        if targetField.isEditable != cueTargets {
            targetField.isEditable = cueTargets
            targetField.isSelectable = cueTargets
        }
        targetField.isEnabled = cueTargets || files
        targetFieldWidth.isActive = !files
        if files {
            let paths = Set(cues.map { c -> String in
                let media = c.raw["kind"]?["media"]?.stringValue ?? ""
                return CueRowMenu.mediaURL(media, showPath: store.document?.path)?.path ?? media
            })
            targetField.stringValue = paths.count == 1 ? paths.first ?? "" : ""
            targetField.placeholderString = paths.count == 1 ? nil : loc("inspector.mixed")
        } else if !cueTargets {
            targetField.stringValue = ""
            targetField.placeholderString = nil
        }
        targetField.toolTip = nil
        targetField.isHidden = false
        targetPath.isHidden = true
        targetPath.url = nil
        for v in [chooseButton, clearTargetButton, jumpTargetButton, targetNote] as [NSView] { v.isHidden = true }
    }

    private func renderBatchMode(_ cues: [Cue], first: Cue) {
        renderMode(first)
        let modes = cues.map { $0.raw["kind"]?["mode"] }
        let types = modes.map { $0?["type"]?.stringValue ?? "timeline" }
        showChoice(groupModePopup, Self.groupModes, types)
        let playlists = types.allSatisfy { $0 == "playlist" }
        groupLoopCheck.isEnabled = playlists
        groupShuffleCheck.isEnabled = playlists
        showTicks(groupLoopCheck, modes.map { playlists && $0?["loop"] == .bool(true) })
        showTicks(groupShuffleCheck, modes.map { playlists && $0?["shuffle"] == .bool(true) })
    }

    private func renderBatchDevamp(_ cues: [Cue], first: Cue) {
        renderDevamp(first)
        let kinds = cues.map { $0.raw["kind"] ?? .null }
        showChoice(devampModePopup, Self.devampModes, kinds.map { $0["devamp_mode"]?.stringValue ?? "" })
        showTicks(devampStartNextCheck, kinds.map { $0["start_next_cue"] == .bool(true) })
        showTicks(devampStopCheck, kinds.map { $0["stop_target_when_slice_ends"] == .bool(true) })
    }

    /// The I/O tab's patch and the Levels tab: the first cue's levels with the levels that differ
    /// shown as mixed, over the most file channels and outputs any of the cues has.
    private func renderBatchAudio(_ cues: [Cue], first: Cue, model: WorkspaceModel) {
        renderIO(first, model: model)
        let patchIDs = outputPatchIDs.map(\.raw)
        // A patch that is gone shows no choice, as for one cue.
        showChoice(
            outputPatchPopup, patchIDs, cues.map { $0.raw["kind"]?["patch"]?.stringValue ?? "" }, fallback: -1)
        followVolumeLimits()
        audioLevels.meter.source = nil
        let sets = cues.compactMap { LevelSet(kind: $0.raw["kind"] ?? .null) }
        guard let base = sets.first else { return audioLevels.show(nil, inputs: 0, outputs: 0) }
        let inputs = cues.map { Self.inputRows(of: $0, durations: store.mediaDurations) }.max() ?? 0
        let outputs = sets.map(\.outputs.count).max() ?? 0
        audioLevels.show(base, inputs: inputs, outputs: outputs)
        batchLevels = audioLevels.levels
        audioLevels.mixed = LevelSet.mixedSlots(sets, inputs: inputs, outputs: outputs)
    }

    // MARK: Mixed controls

    /// Selects in `popup` the item of `values` (the choices, in item order) every cue has, or shows
    /// the mixed item when they differ. `fallback` is the item for a value none of the choices is.
    func showChoice(_ popup: NSPopUpButton, _ choices: [String?], _ values: [String?], fallback: Int = 0) {
        let mixedTag = Self.mixedItemTag
        let distinct = Set(values.map { $0 ?? "\u{0}" })
        if distinct.count > 1 {
            if popup.menu?.items.last?.tag != mixedTag {
                let item = NSMenuItem(title: loc("inspector.mixed"), action: nil, keyEquivalent: "")
                item.tag = mixedTag
                item.isHidden = true
                popup.menu?.addItem(item)
            }
            popup.selectItem(withTag: mixedTag)
            return
        }
        let index = values.first.flatMap { v in choices.firstIndex { $0 == v } } ?? fallback
        if popup.indexOfSelectedItem != index { popup.selectItem(at: index) }
    }

    /// The mixed item's tag: no item of a choice pop-up has one.
    static let mixedItemTag = 0x6D78

    /// A tick box on when every cue has the value, off when none has, mixed otherwise. A click on a
    /// mixed box sets it on for every cue (AppKit's order after mixed is on).
    func showTicks(_ check: NSButton, _ values: [Bool]) {
        let on = values.filter { $0 }.count
        let mixed = on > 0 && on < values.count
        check.allowsMixedState = mixed
        check.state = mixed ? .mixed : on > 0 ? .on : .off
    }

    /// What one cue shows: tick boxes take two states again, and the controls only one cue can set
    /// come back. Called before the single-cue render sets each control from the cue.
    func endBatchControls() {
        for c in [flaggedCheck, armedCheck, onReleaseCheck, groupLoopCheck, groupShuffleCheck, devampStartNextCheck,
            devampStopCheck]
        {
            c.allowsMixedState = false
        }
        for c in singleCueControls + [numberField, loadTimeField, networkMessageField] { c.isEnabled = !isLocked }
        audioLevels.mixed = []
        batchLevels = nil
        for (f, text) in [(InspectorField.osc, "inspector.osc.placeholder"), (.message, "inspector.network.messageHint")] {
            field(f)?.placeholderString = loc(text)
        }
        for f in [InspectorField.number, .preWait, .postWait, .duration, .loadTime] { field(f)?.placeholderString = nil }
    }

    // MARK: Editing several cues

    /// Sends `make`'s command for each cue an edit is for: one cue's alone, several as one transaction.
    func sendEach(
        key: String, revert: Set<InspectorField> = [], coalesce: String? = nil, _ make: (Cue) -> Command?
    ) {
        guard !isLocked else { return }
        let cues = editCues
        let commands = cues.compactMap(make)
        guard let first = cues.first, let command = Command.transaction(commands) else { return }
        messageLabel.stringValue = ""
        send(command, cue: first.id, key: key, revert: revert, coalesce: coalesce)
    }

    /// A kind edit on every cue: `build` gives each cue's new kind from its own; only the fields it
    /// changed go out. Each cue's new kind is what its next edit starts from until the engine answers.
    func sendKindEach(_ build: (Cue, JSONValue) -> JSONValue, key: String, coalesce: String? = nil) {
        guard !isLocked else { return }
        var commands: [Command] = []
        var kinds: [ModelID: JSONValue] = [:]
        for cue in editCues {
            guard let kind = cue.raw["kind"] else { continue }
            let new = build(cue, kind)
            var changed: [String: JSONValue] = [:]
            for (field, value) in new.objectValue ?? [:] where kind[field] != value { changed[field] = value }
            guard !changed.isEmpty else { continue }
            commands.append(.setProperty(cue.id, .kindFields(changed)))
            kinds[cue.id] = new
        }
        guard let first = commands.first, let command = Command.transaction(commands),
            let id = first.json["id"]?.stringValue
        else { return }
        messageLabel.stringValue = ""
        send(command, cue: ModelID(id), key: key, revert: [], coalesce: coalesce, kinds: kinds)
    }

    /// A text field committed while several cues are selected: only text the operator typed is sent,
    /// so a mixed field that was only passed through changes nothing. Each cue's command comes from
    /// the same check one cue gets; one refusal refuses the edit for all.
    func commitBatch(_ f: InspectorField, text: String, typed: Bool) -> Bool {
        guard typed, f != .number, let model = store.model else {
            render(force: [f])
            return true
        }
        var commands: [Command] = []
        for cue in batchCues {
            switch InspectorEdit.command(f, text: text, cue: cue, model: model, typed: true) {
            case .failure(let r):
                showRefusal(loc(r.messageKey))
                render(force: [f])
                return false
            case .success(let c?): commands.append(c)
            case .success(nil): break
            }
        }
        guard let first = batchCues.first, let command = Command.transaction(commands) else {
            render(force: [f])
            return true
        }
        messageLabel.stringValue = ""
        send(command, cue: first.id, key: "\(f)", revert: [f])
        if [.preWait, .postWait, .duration, .loadTime].contains(f), let s = CueTime.parse(text) {
            field(f)?.stringValue = CueTime.format(s)
            field(f)?.placeholderString = nil
        }
        return true
    }

    /// A levels edit while several Audio cues are shown: what changed from the levels last shown is
    /// set on every cue, each cue's other levels kept. The changed levels are no longer mixed.
    func sendBatchLevels(_ set: LevelSet, gesture: Int?) {
        guard let old = batchLevels else { return }
        batchLevels = set
        let changed = Set(audioLevels.mixed.filter { set.db($0) != old.db($0) || set.isMuted($0) != old.isMuted($0) })
        audioLevels.mixed.subtract(changed)
        sendKindEach(
            { _, kind in
                guard let own = LevelSet(kind: kind) else { return kind }
                return own.applying(from: old, to: set).json(into: kind)
            }, key: "levels", coalesce: gesture.map { "levels:batch:\(Self.launchToken):\($0)" })
    }
}
