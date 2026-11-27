// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Several cues selected (ED-04; QLab 5, manual p. 123): the inspector stays live and edits what the
/// cues share. Only tabs holding fields that can be set on all of them are shown: Basics and Triggers
/// for cues of different kinds, and some of the kind's own tabs when all are of one kind. A value that
/// is not the same on every cue is drawn as mixed (an empty field, a dash, a mixed tick box); an edit
/// sets that one value on every selected cue, the rest of each cue kept, as one undo step. A cue's
/// number and hotkey name one cue, so they are not edited here.
extension InspectorViewController {
    /// The kind tabs that hold fields set on several cues at once. Time & Loops (each cue's own file)
    /// and a Fade's curve and levels (each fades its own target) are edited one cue at a time.
    static let batchKindTabs: [String: [String]] = [
        "audio": ["io", "levels"], "group": ["mode"], "load": ["loadTime"], "devamp": ["devamp"],
        "network": ["network"], "video": ["io"], "target": ["newTarget"],
    ]

    /// The tabs shown for several selected cues of `kinds`. `allSound`: every one of them has sound,
    /// which gives Video cues the Levels tab Audio cues have.
    static func batchTabs(forKinds kinds: Set<String>, allSound: Bool = false) -> [String] {
        let common = ["basics", "triggers"]
        guard kinds.count == 1, let kind = kinds.first else { return common }
        let own = Set(batchKindTabs[kind] ?? [])
        let ids = InspectorTabs.ids(forKind: kind).filter { common.contains($0) || own.contains($0) }
        return kind == "video" && allSound ? ids + ["levels"] : ids
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
        [
            numberField, hotkeyField, hotkeyClear, networkSendButton, chooseButton, clearTargetButton,
        ]
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
                // Notes that differ show as an empty view with a Mixed hint, as a text field does.
                notesView.placeholder = shown.count > 1 ? loc("inspector.mixed") : nil
            } else if let tf = field(f) {
                if tf.stringValue != text { tf.stringValue = text }
                // A number belongs to one cue: its field is off and empty, not mixed.
                tf.placeholderString = shown.count > 1 && f != .number ? loc("inspector.mixed") : nil
                if f == .number { tf.stringValue = "" }
            }
        }
        let style = kind.flatMap { _ in CueKindStyle.typeStyle(kind: cues.first?.raw["kind"]) }
        kindIcon.image = style.map { $0.icon.image($0.label, compact: true) }
        renderBatchTarget(cues, kinds: kinds)
        for c in singleCueControls { c.isEnabled = false }
        let modes = cues.map { $0.raw["continue_mode"]?.stringValue ?? "do_not_continue" }
        // Typed when every cue's is (Wait and Fade cues, stills).
        durationField.isEnabled = cues.allSatisfy {
            InspectorEdit.editsDuration($0, media: store.mediaDurations[$0.id])
        }
        postWaitField.isEnabled = modes.contains { $0 != "auto_follow" }
        loadTimeField.isEnabled = kind == "load"
        // Several Target cues take one new target together (QLab 5 shows a kind's tab for a selection of that kind).
        newTargetField.isEnabled = kind == "target"
        networkMessageField.isEnabled = kind == "network"
        // The others' fields are switched on or off by their boxes before the hints below are judged.
        renderBatchOthers(cues)
        // A field that is off shows no Mixed hint, so it does not read as one to type in.
        for f in InspectorField.allCases { if field(f)?.isEnabled == false { field(f)?.placeholderString = nil } }
        // An empty field that is off draws like one that is on (AppKit dims only its text): it is dimmed whole.
        for f in batchDimmed { f.alphaValue = f.isEnabled ? 1 : Self.offAlpha }
        let allSound = cues.allSatisfy { $0.sounds(store.mediaDurations[$0.id]) }
        syncTabs(Self.batchTabs(forKinds: kinds, allSound: allSound), kind: kind ?? "batch")
        showChoice(colourPopup, Self.colourItems, cues.map { $0.raw["color"]?.stringValue ?? "none" })
        let seconds = cues.map { $0.raw["second_color"]?.stringValue ?? "none" }
        showChoice(secondColourPopup, Self.colourItems, seconds)
        if secondColourUnlocked {
            secondColourCheck.allowsMixedState = false
            secondColourCheck.state = .on
        } else {
            showTicks(secondColourCheck, seconds.map { $0 != "none" })
        }
        secondColourPopup.isEnabled = secondColourCheck.state != .off && !isLocked
        showTicks(autoLoadCheck, cues.map { $0.raw["auto_load"] == .bool(true) })
        showTicks(skipCheck, cues.map { $0.raw["skip_if_disarmed"] == .bool(true) })
        // Each cue's own switch: mixed when they differ (the box sets it on every cue).
        showTicks(hotkeyEnableCheck, cues.map { $0.raw["triggers"]?["hotkey_enabled"] != .bool(false) })
        hotkeyEnableCheck.isEnabled = !isLocked
        hotkeyField.textColor = .controlTextColor
        renderBatchWallClock(cues)
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
            case "video": renderBatchVideo(cues, first: first, model: model, allSound: allSound)
            case "network":
                renderNetwork(first, model: model)
                // A destination that is not a patch is the last item ("no patch"), as for one cue.
                let known = Set(model.networkPatches.map(\.id.raw))
                let chosen = cues.map { c -> String in
                    let d = c.raw["kind"]?["destination"]?.stringValue ?? ""
                    return known.contains(d) ? d : ""
                }
                showChoice(
                    networkPatchPopup, networkPatchIDs.map { $0?.raw }, chosen, fallback: networkPatchIDs.count - 1)
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
        let files = kinds == ["audio"] || kinds == ["video"]
        if targetField.isEditable != cueTargets {
            targetField.isEditable = cueTargets
            targetField.isSelectable = cueTargets
        }
        targetField.isEnabled = cueTargets || files
        targetFieldWidth.isActive = !files
        if files {
            let paths = Set(
                cues.map { c -> String in
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
        targetPath.show(url: nil)
        for v in [chooseButton, clearTargetButton, jumpTargetButton, revealTargetButton, targetNote] as [NSView] {
            v.isHidden = true
        }
    }

    private func renderBatchMode(_ cues: [Cue], first: Cue) {
        renderMode(first)
        let modes = cues.map { $0.raw["kind"]?["mode"] }
        let types = modes.map { $0?["type"]?.stringValue ?? "timeline" }
        showGroupModes(types)
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

    /// The I/O tab's patch and the Levels tab of several Audio cues.
    private func renderBatchAudio(_ cues: [Cue], first: Cue, model: WorkspaceModel) {
        renderIO(first, model: model)
        renderBatchSound(cues)
        let v = videoIO
        if Set(cues.map { $0.raw["kind"]?["media"]?.stringValue ?? "" }).count > 1 {
            v.showMixedFile()
        }
        if Set(cues.map { store.mediaDurations[$0.id]?.audioFile }).count > 1 {
            v.soundLabel.stringValue = loc("inspector.mixed")
            v.soundLabel.textColor = .secondaryLabelColor
        }
        // The first cue may have no file while others do: the row shows when any of them has one.
        if cues.contains(where: Self.hasFile) { v.soundRow?.isHidden = false }
    }

    static func hasFile(_ cue: Cue) -> Bool {
        !(cue.raw["kind"]?["media"]?.stringValue ?? "").isEmpty
    }

    /// The output patch and the Levels tab of several cues with sound: the patch they share or
    /// mixed, and the first cue's levels with the levels that differ shown as mixed, over the most
    /// file channels and outputs any of the cues has. After `renderIO` of the first.
    private func renderBatchSound(_ cues: [Cue]) {
        if let model = store.model { renderOutputFormat(cues, model: model) }
        // A patch that is gone shows no choice, as for one cue; a Video cue's none is the last item.
        showChoice(
            outputPatchPopup, outputPatchChoices, cues.map { $0.raw["kind"]?["patch"]?.stringValue ?? "" },
            fallback: -1)
        // One patch on all of them: its output names over the columns, and Edit opens it.
        let shared = store.model.flatMap { Self.sharedPatch(of: cues, model: $0) }
        editPatchButton.isEnabled = !isLocked && shared != nil
        audioLevels.hasPatch = shared != nil
        audioLevels.matrix.outputNames =
            shared == nil
            ? [] : store.model.flatMap { m in cues.first.map { Self.outputNames(of: $0, model: m) } } ?? []
        followVolumeLimits()
        audioLevels.meter.source = nil
        let sets = cues.compactMap { LevelSet(kind: $0.raw["kind"] ?? .null) }
        guard let base = sets.first else { return audioLevels.show(nil, inputs: 0, outputs: 0) }
        let inputs = cues.map { Self.inputRows(of: $0, durations: store.mediaDurations) }.max() ?? 0
        // As many columns as the widest of them shows on its own (its patch's count), or more where
        // a cue's own levels name more.
        let outputs =
            cues.compactMap { cue -> Int? in
                guard let model = store.model, let set = LevelSet(kind: cue.raw["kind"] ?? .null) else { return nil }
                return max(Self.shownOutputs(of: cue, set, model: model), set.outputs.count)
            }.max() ?? 0
        audioLevels.show(base, inputs: inputs, outputs: outputs)
        audioLevels.gangsMixed = Set(sets.map(\.gangs)).count > 1
        audioLevels.editsSeveral = true
        audioLevels.mixed = LevelSet.mixedSlots(sets, inputs: inputs, outputs: outputs)
    }

    /// The I/O tab of several Video cues: the stage they share or mixed; the file and format rows
    /// say mixed when the cues' differ. The sound format, the output patch and the Levels tab are
    /// theirs only when every one of them has sound; the audio track is chosen one cue at a time.
    private func renderBatchVideo(_ cues: [Cue], first: Cue, model: WorkspaceModel, allSound: Bool) {
        renderIO(first, model: model)
        let v = videoIO
        // The first cue may have no file while others do: the picture format row shows when any has one.
        if cues.contains(where: Self.hasFile) { v.videoRows.first?.isHidden = false }
        v.soundRow?.isHidden = !allSound
        v.showOutputSection(allSound)
        if allSound {
            renderBatchSound(cues)
            // Each cue's own track, as text: the same on all of them, or mixed.
            let played = Set(
                cues.map { c -> AudioTrackFormat? in
                    let m = store.mediaDurations[c.id]
                    let tracks = m?.audioTracks ?? []
                    return tracks.indices.contains(m?.audioTrack ?? 0) ? tracks[m?.audioTrack ?? 0] : tracks.first
                })
            v.soundLabel.isHidden = false
            v.trackPopup.isHidden = true
            v.soundLabel.stringValue =
                played.count == 1
                ? played.first.flatMap { $0 }.map(VideoIOControls.soundText) ?? "" : loc("inspector.mixed")
            v.soundLabel.textColor = played.count == 1 ? .labelColor : .secondaryLabelColor
        }
        let stages = cues.map { $0.raw["kind"]?["stage"]?.stringValue }
        // None is the last item; a stage that is gone shows no choice, as for one cue.
        let known = Set(model.stages.map(\.id.raw))
        let chosen = stages.map { s in s.map { known.contains($0) ? $0 : "\u{1}" } ?? "" }
        // The separator matches nothing; none (the last item) matches "".
        let choices = v.stageIDs.enumerated().map { i, id in i == v.stageIDs.count - 2 ? "\u{2}" : id?.raw ?? "" }
        showChoice(v.stagePopup, choices, chosen, fallback: -1)
        if Set(stages).count > 1 { v.stageSizeLabel.stringValue = "" }
        let files = Set(cues.map { $0.raw["kind"]?["media"]?.stringValue ?? "" })
        if files.count > 1 {
            v.showMixedFile()
        }
        let formats = Set(cues.map { store.mediaDurations[$0.id]?.picture })
        if formats.count > 1 {
            v.formatLabel.stringValue = loc("inspector.mixed")
            v.formatLabel.textColor = .secondaryLabelColor
        }
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
    /// The text fields several selected cues can turn off, and how faint they are then.
    var batchDimmed: [NSTextField] {
        [numberField, durationField, targetField, hotkeyField, loadTimeField, networkMessageField, newTargetField]
    }
    static let offAlpha: CGFloat = 0.45

    func endBatchControls() {
        for c in [
            flaggedCheck, armedCheck, onReleaseCheck, groupLoopCheck, groupShuffleCheck, devampStartNextCheck,
            devampStopCheck, autoLoadCheck, skipCheck, secondColourCheck, fadeOthersCheck, duckCheck, hotkeyEnableCheck,
            wallClockCheck,
        ] {
            c.allowsMixedState = false
        }
        for c in singleCueControls + [numberField, loadTimeField, networkMessageField, newTargetField] {
            c.isEnabled = !isLocked
        }
        for f in batchDimmed { f.alphaValue = 1 }
        audioLevels.mixed = []
        audioLevels.gangsMixed = false
        audioLevels.editsSeveral = false
        field(.message)?.placeholderString = loc("inspector.network.messageHint")
        notesView.placeholder = nil
        for f in [InspectorField.number, .preWait, .postWait, .duration, .loadTime] {
            field(f)?.placeholderString = nil
        }
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
            switch InspectorEdit.command(
                f, text: text, cue: cue, model: model, typed: true, media: store.mediaDurations[cue.id])
            {
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
        // Notes sent to several cues and not saved are offered back as one cue's are.
        send(command, cue: first.id, key: "\(f)", revert: [f], notes: f == .notes ? text : nil)
        if InspectorField.times.contains(f), let s = CueTime.parse(text) {
            field(f)?.stringValue = CueTime.precise(s)
            field(f)?.placeholderString = nil
        }
        return true
    }

    /// A levels edit while several Audio cues are shown: each cue does what the operator did to the
    /// shown levels, to its own levels (the level moved, with its own gang; a mute; a gang; set all
    /// silent or to the defaults). No other level of any cue is written.
    func sendBatchLevels(_ set: LevelSet, gesture: Int?) {
        guard let action = audioLevels.lastEdit else { return }
        if case .level(let slot) = action { audioLevels.mixed.remove(slot) }
        if case .mute(let slot) = action { audioLevels.mixed.remove(slot) }
        let limits = audioLevels.limits
        sendKindEach(
            { [store] cue, kind in
                guard let own = LevelSet(kind: kind) else { return kind }
                let outputs = store.model.map { Self.shownOutputs(of: cue, own, model: $0) } ?? 0
                return own.taking(action, from: set, limits: limits, drag: gesture != nil, outputs: outputs)
                    .json(into: kind)
            }, key: "levels", coalesce: gesture.map { "levels:batch:\(Self.launchToken):\($0)" })
    }
}
