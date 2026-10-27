// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// Bottom inspector: the Basics tab for the selected cue (ED-01, CUE-01..CUE-03). Edits go to the
/// store as model commands; fields show the store's model, so a refused or invalid edit simply
/// puts the model's value back. A field being edited is never overwritten by a model change.
@MainActor
final class InspectorViewController: NSViewController, NSTextFieldDelegate, NSTextViewDelegate {
    static let colours = ["none", "red", "orange", "green", "blue", "purple"]
    static let continueModes = ["do_not_continue", "auto_continue", "auto_follow"]

    let store: WorkspaceStore
    let emptyLabel = NSTextField(labelWithString: loc("inspector.empty"))
    let tabView = NSTabView()
    let numberField = NSTextField()
    let nameField = NSTextField()
    let preWaitField = NSTextField()
    let postWaitField = NSTextField()
    let targetField = NSTextField()
    let oscField = NSTextField()
    let hotkeyField = HotkeyRecorder()
    let hotkeyClear = NSButton(title: loc("inspector.hotkey.clear"), target: nil, action: nil)
    let secondTriggerPopup = NSPopUpButton()
    let onReleaseCheck = NSButton(checkboxWithTitle: loc("inspector.onRelease"), target: nil, action: nil)
    /// Seams: the workspace's transport keys and the menu bar's keys, for hotkey validation.
    var transportKeys: (() -> TransportKeys)?
    var menuChords: @MainActor () -> Set<KeyChord> = HotkeyRules.currentMenuChords
    static let secondTriggers = [
        "nothing", "panic", "stop", "hard_stop", "hard_stop_and_restart", "devamp", "play_next", "play_previous",
    ]
    let durationLabel = NSTextField(labelWithString: "")
    let kindIcon = NSImageView()
    let notesView = NSTextView()
    let colourPopup = NSPopUpButton()
    let continuePopup = NSPopUpButton()
    let flaggedCheck = NSButton(checkboxWithTitle: loc("inspector.flagged"), target: nil, action: nil)
    let armedCheck = NSButton(checkboxWithTitle: loc("inspector.armed"), target: nil, action: nil)
    let messageLabel = NSTextField(labelWithString: "")

    private(set) var selection: [ModelID] = []
    private var subscription: AnyCancellable?
    /// Fields typed in since their last commit or cancel. Only these count as "being edited": a
    /// focused field the user has not changed takes model updates (after Return, say).
    private(set) var dirty: Set<InspectorField> = []
    /// Newest edit number per cue and control; a refusal only reverts when it is still the newest.
    private var editSeq: [String: Int] = [:]
    private var inFlight: [Int: Task<Void, Never>] = [:]
    private var nextTask = 0
    private var sendFailures = 0

    init(store: WorkspaceStore) {
        self.store = store
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    var cue: Cue? {
        guard selection.count == 1, let id = selection.first else { return nil }
        return store.model?.cue(id)
    }

    /// `triggers` is replaced whole, so an edit made before the engine answered the previous one
    /// (OSC typed, then a key recorded) must start from what was sent, not from the stale model.
    private var pendingTriggers: [ModelID: JSONValue] = [:]

    /// The selected cue as edits should see it: the model's, with unanswered triggers applied.
    private var cueForEdit: Cue? {
        guard var c = cue else { return nil }
        if let t = pendingTriggers[c.id] { c.raw.set("triggers", t) }
        return c
    }

    func field(_ f: InspectorField) -> NSTextField? {
        switch f {
        case .number: return numberField
        case .name: return nameField
        case .preWait: return preWaitField
        case .postWait: return postWaitField
        case .target: return targetField
        case .osc: return oscField
        case .notes: return nil
        }
    }

    // MARK: Layout

    override func loadView() {
        let root = NSView()
        root.setAX(AX.inspector)
        emptyLabel.setAX(AX.inspectorEmpty)
        emptyLabel.font = .systemFont(ofSize: 24, weight: .medium)
        emptyLabel.textColor = .tertiaryLabelColor
        for (f, id) in [
            (numberField, AX.inspectorNumber), (nameField, AX.inspectorName), (preWaitField, AX.inspectorPreWait),
            (postWaitField, AX.inspectorPostWait), (targetField, AX.inspectorTarget), (oscField, AX.inspectorOSC),
        ] {
            f.setAX(id)
            f.delegate = self
            f.lineBreakMode = .byTruncatingTail
            f.usesSingleLineMode = true
        }
        durationLabel.setAX(AX.inspectorDuration)
        notesView.setAX(AX.inspectorNotes)
        notesView.delegate = self
        notesView.isRichText = false
        notesView.isAutomaticQuoteSubstitutionEnabled = false
        notesView.isAutomaticDashSubstitutionEnabled = false
        notesView.isVerticallyResizable = true
        notesView.autoresizingMask = .width
        notesView.font = .systemFont(ofSize: NSFont.systemFontSize)
        notesView.textContainerInset = NSSize(width: 2, height: 4)
        colourPopup.setAX(AX.inspectorColour)
        continuePopup.setAX(AX.inspectorContinue)
        flaggedCheck.setAX(AX.inspectorFlagged)
        armedCheck.setAX(AX.inspectorArmed)
        messageLabel.setAX(AX.inspectorMessage)
        messageLabel.textColor = .systemRed
        messageLabel.lineBreakMode = .byTruncatingTail
        messageLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        for p in [colourPopup, continuePopup] {
            p.target = self
            p.action = #selector(popupChanged(_:))
        }
        for c in [flaggedCheck, armedCheck] {
            c.target = self
            c.action = #selector(checkChanged(_:))
        }
        fillPopups()

        let scroll = NSScrollView()
        scroll.documentView = notesView
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 48).isActive = true

        func row(_ title: String, _ control: NSView) -> NSView {
            let label = NSTextField(labelWithString: title)
            label.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            label.textColor = .secondaryLabelColor
            let s = NSStackView(views: [label, control])
            s.orientation = .vertical
            s.alignment = .leading
            s.spacing = 2
            return s
        }
        func column(_ views: [NSView], width: CGFloat? = nil) -> NSStackView {
            let s = NSStackView(views: views)
            s.orientation = .vertical
            s.alignment = .leading
            s.spacing = 8
            if let width { s.widthAnchor.constraint(equalToConstant: width).isActive = true }
            return s
        }
        for f in [numberField, preWaitField, postWaitField] {
            f.widthAnchor.constraint(equalToConstant: 130).isActive = true
        }
        kindIcon.widthAnchor.constraint(equalToConstant: 20).isActive = true
        let nameRow = NSStackView(views: [kindIcon, nameField])
        nameRow.spacing = 4
        nameField.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let left = column(
            [
                row(loc("cuelist.col.number"), numberField), row(loc("cuelist.col.duration"), durationLabel),
                row(loc("cuelist.col.preWait"), preWaitField), row(loc("cuelist.col.postWait"), postWaitField),
                row(loc("inspector.continue"), continuePopup),
            ], width: 150)
        let centre = column([
            row(loc("cuelist.col.name"), nameRow), row(loc("cuelist.col.target"), targetField),
            row(loc("inspector.notes"), scroll),
        ])
        for v in centre.arrangedSubviews { v.widthAnchor.constraint(equalTo: centre.widthAnchor).isActive = true }
        nameRow.widthAnchor.constraint(equalTo: centre.widthAnchor).isActive = true
        targetField.widthAnchor.constraint(equalTo: centre.widthAnchor).isActive = true
        scroll.widthAnchor.constraint(equalTo: centre.widthAnchor).isActive = true
        let right = column([row(loc("inspector.colour"), colourPopup), flaggedCheck, armedCheck], width: 140)
        let columns = NSStackView(views: [left, centre, right])
        columns.orientation = .horizontal
        columns.alignment = .top
        columns.spacing = 20
        columns.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 8, right: 12)

        let item = NSTabViewItem(identifier: "basics")
        item.label = loc("inspector.tab.basics")
        item.view = columns
        tabView.addTabViewItem(item)
        tabView.addTabViewItem(triggersTab(row: row, column: column))
        tabView.setAX(AX.inspectorTabs)

        for v in [tabView, emptyLabel, messageLabel] {
            v.translatesAutoresizingMaskIntoConstraints = false
            root.addSubview(v)
        }
        NSLayoutConstraint.activate([
            emptyLabel.centerXAnchor.constraint(equalTo: root.centerXAnchor),
            emptyLabel.centerYAnchor.constraint(equalTo: root.centerYAnchor),
            tabView.topAnchor.constraint(equalTo: root.topAnchor, constant: 4),
            tabView.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 4),
            tabView.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -4),
            tabView.bottomAnchor.constraint(equalTo: messageLabel.topAnchor, constant: -2),
            messageLabel.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 12),
            messageLabel.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -12),
            messageLabel.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -4),
            root.heightAnchor.constraint(greaterThanOrEqualToConstant: 100),
        ])
        view = root
        subscription = store.changes.sink { [weak self] change in
            switch change {
            case .reset, .model: MainActor.assumeIsolated { self?.render() }
            case .runtime, .progress, .notices: break
            }
        }
        render()
    }

    /// The Triggers tab: hotkey (recorder, Clear), second trigger and on-release, OSC address.
    private func triggersTab(
        row: (String, NSView) -> NSView, column: ([NSView], CGFloat?) -> NSStackView
    ) -> NSTabViewItem {
        hotkeyField.setAX(AX.inspectorHotkey)
        hotkeyField.isEditable = false
        hotkeyField.isSelectable = false
        hotkeyField.isBezeled = true
        hotkeyField.bezelStyle = .roundedBezel
        hotkeyField.placeholderString = loc("inspector.hotkey.click")
        hotkeyField.onRecord = { [weak self] event in
            if let chord = KeyChord(event: event) { self?.record(chord) }
        }
        hotkeyClear.setAX(AX.inspectorHotkeyClear)
        hotkeyClear.target = self
        hotkeyClear.action = #selector(clearHotkey)
        secondTriggerPopup.setAX(AX.inspectorSecondTrigger)
        secondTriggerPopup.target = self
        secondTriggerPopup.action = #selector(popupChanged(_:))
        for key in Self.secondTriggers {
            secondTriggerPopup.menu?.addItem(
                NSMenuItem(title: loc("inspector.secondTrigger.\(key)"), action: nil, keyEquivalent: ""))
        }
        onReleaseCheck.setAX(AX.inspectorOnRelease)
        onReleaseCheck.target = self
        onReleaseCheck.action = #selector(checkChanged(_:))
        oscField.placeholderString = loc("inspector.osc.placeholder")
        hotkeyField.widthAnchor.constraint(equalToConstant: 200).isActive = true
        oscField.widthAnchor.constraint(equalToConstant: 260).isActive = true
        let hotkeyRow = NSStackView(views: [hotkeyField, hotkeyClear])
        hotkeyRow.spacing = 6
        let left = column(
            [
                row(loc("inspector.hotkey"), hotkeyRow), onReleaseCheck, row(loc("inspector.osc"), oscField),
            ], nil)
        let right = column([row(loc("inspector.secondTrigger"), secondTriggerPopup)], 200)
        let columns = NSStackView(views: [left, right])
        columns.alignment = .top
        columns.spacing = 40
        columns.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 8, right: 12)
        let item = NSTabViewItem(identifier: "triggers")
        item.label = loc("inspector.tab.triggers")
        item.view = columns
        return item
    }

    /// A recorded chord: refused with a message, or sent as the cue's new hotkey. Returns whether
    /// it was sent. The other trigger fields are kept.
    @discardableResult
    func record(_ chord: KeyChord) -> Bool {
        guard let cue = cueForEdit, let model = store.model else { return false }
        if let error = HotkeyRules.validate(
            chord, for: cue.id, in: model, keys: transportKeys?() ?? TransportKeys(settings: model.settings),
            menuChords: menuChords())
        {
            messageLabel.stringValue = loc(HotkeyRules.messageKey(error))
            return false
        }
        messageLabel.stringValue = ""
        send(InspectorEdit.setTriggers(cue, hotkey: .some(chord.hotkeyJSON)), cue: cue.id, key: "hotkey", revert: [])
        return true
    }

    @objc func clearHotkey() {
        guard let cue = cueForEdit, cue.raw["triggers"]?["hotkey"].map({ $0 != .null }) == true else { return }
        messageLabel.stringValue = ""
        send(InspectorEdit.setTriggers(cue, hotkey: .some(nil)), cue: cue.id, key: "hotkey", revert: [])
    }

    private func fillPopups() {
        for key in Self.colours {
            let style = CueKindStyle.colours[key]
            let item = NSMenuItem(title: loc(style?.labelKey ?? "cue.colour.none"), action: nil, keyEquivalent: "")
            if let style,
                let image = symbolImage(style.symbol, "")?.withSymbolConfiguration(.init(paletteColors: [style.tint]))
            {
                item.image = image
            }
            colourPopup.menu?.addItem(item)
        }
        for key in Self.continueModes {
            continuePopup.menu?.addItem(
                NSMenuItem(title: loc("inspector.continue.\(key)"), action: nil, keyEquivalent: ""))
        }
    }

    // MARK: Showing the model

    func setSelection(_ ids: [ModelID]) {
        // The list re-announces an unchanged selection after model changes; that must not wipe
        // what the user is typing or the message they are reading.
        if ids == selection { return }
        selection = ids
        messageLabel.stringValue = ""
        dirty = []
        render(force: Set(InspectorField.allCases))
    }

    func isEditing(_ f: InspectorField) -> Bool { dirty.contains(f) }

    /// Puts the model's values in the controls. A field being edited keeps its text unless `force`.
    func render(force: Set<InspectorField> = []) {
        _ = view
        guard let cue, let model = store.model else {
            tabView.isHidden = true
            messageLabel.isHidden = true
            emptyLabel.isHidden = false
            emptyLabel.stringValue = loc(selection.count > 1 ? "inspector.multiple" : "inspector.empty")
            return
        }
        tabView.isHidden = false
        messageLabel.isHidden = false
        emptyLabel.isHidden = true
        for f in InspectorField.allCases where force.contains(f) || !isEditing(f) {
            let text = InspectorEdit.display(f, cue: cue, model: model)
            if f == .notes {
                if notesView.string != text { notesView.string = text }
            } else if let tf = field(f), tf.stringValue != text {
                tf.stringValue = text
            }
        }
        let kind = cue.kindType
        let style = CueKindStyle.kinds[kind]
        kindIcon.image = style.flatMap { symbolImage($0.symbol, CueKindStyle.kindLabel(kind)) }
        let isTarget = InspectorEdit.targetKinds.contains(kind)
        targetField.placeholderString = isTarget ? nil : loc("inspector.target.notApplicable")
        if kind == "audio" {
            targetField.stringValue = cue.raw["kind"]?["media"]?.stringValue ?? ""
        } else if !isTarget {
            targetField.stringValue = ""
        }
        targetField.isEnabled = isTarget
        // Duration comes from content (read-only here): Fade and Wait carry theirs in the model.
        let hasDuration = kind == "fade" || kind == "wait"
        durationLabel.stringValue =
            hasDuration ? InspectorTime.format(cue.raw["kind"]?["duration"]?.doubleValue ?? 0) : ""
        let colour = cue.raw["color"]?.stringValue ?? "none"
        colourPopup.selectItem(at: Self.colours.firstIndex(of: colour) ?? 0)
        let mode = cue.raw["continue_mode"]?.stringValue ?? "do_not_continue"
        continuePopup.selectItem(at: Self.continueModes.firstIndex(of: mode) ?? 0)
        // Auto-follow takes its post-wait from the action's end (core: not used), so it is not editable.
        postWaitField.isEnabled = mode != "auto_follow"
        let hotkey = cue.raw["triggers"]?["hotkey"].flatMap { $0 == .null ? nil : $0 }
        hotkeyField.stringValue = hotkey.map { KeyChord(hotkey: $0)?.display ?? ($0["key"]?.stringValue ?? "") } ?? ""
        hotkeyClear.isEnabled = hotkey != nil
        onReleaseCheck.isEnabled = hotkey != nil
        onReleaseCheck.state = cue.raw["second_trigger_on_release"] == .bool(true) ? .on : .off
        let second = cue.raw["second_trigger"]?.stringValue ?? "nothing"
        secondTriggerPopup.selectItem(at: Self.secondTriggers.firstIndex(of: second) ?? 0)
        flaggedCheck.state = cue.raw["flagged"] == .bool(true) ? .on : .off
        armedCheck.state = cue.raw["armed"] == .bool(false) ? .off : .on
    }

    // MARK: Committing edits

    /// Sends the edit for `f` if the text changed; rejects bad input by putting the model's text back.
    @discardableResult
    func commit(_ f: InspectorField) -> Bool {
        dirty.remove(f)
        guard let cue = cueForEdit, let model = store.model else { return true }
        let text = f == .notes ? notesView.string : (field(f)?.stringValue ?? "")
        switch InspectorEdit.command(f, text: text, cue: cue, model: model) {
        case .failure(let r):
            show(rejection: r)
            render(force: [f])
            return false
        case .success(nil):
            render(force: [f])
        case .success(let command?):
            messageLabel.stringValue = ""
            send(command, cue: cue.id, key: "\(f)", revert: [f])
            // Show what will be stored (`90` becomes `01:30.000`) until the model answers.
            if f == .preWait || f == .postWait, let s = InspectorTime.parse(text) {
                field(f)?.stringValue = InspectorTime.format(s)
            }
        }
        return true
    }

    /// Commits every field typed in and not yet committed, then waits for the sends to be answered.
    /// False when an edit was invalid or refused: the caller (Save, Close, Quit) must not go on.
    func commitPendingEdits() async -> Bool {
        var ok = true
        for f in InspectorField.allCases where dirty.contains(f) { ok = commit(f) && ok }
        let failuresBefore = sendFailures
        for task in Array(inFlight.values) { await task.value }
        return ok && sendFailures == failuresBefore
    }

    /// Esc: drops the edit and shows the model's text. Nothing is left to commit, so the
    /// end-of-editing that follows finds the text unchanged.
    func cancel(_ f: InspectorField) {
        dirty.remove(f)
        render(force: [f])
    }

    private func send(_ command: Command, cue: ModelID, key: String, revert: Set<InspectorField>) {
        let seqKey = "\(cue)/\(key)"
        let n = (editSeq[seqKey] ?? 0) + 1
        editSeq[seqKey] = n
        nextTask += 1
        let taskID = nextTask
        let property = command.json["property"]
        let sentTriggers = property?["type"]?.stringValue == "triggers" ? property?["value"] : nil
        if let sentTriggers { pendingTriggers[cue] = sentTriggers }
        inFlight[taskID] = Task { [weak self] in
            guard let self else { return }
            defer {
                inFlight[taskID] = nil
                if let sentTriggers, pendingTriggers[cue] == sentTriggers { pendingTriggers[cue] = nil }
            }
            if case .failure(let error) = await store.send(command: command) {
                sendFailures += 1
                if case .engine(_, let message) = error {
                    messageLabel.stringValue = String(format: loc("inspector.refused"), message)
                } else {
                    messageLabel.stringValue = loc("inspector.failed")
                }
                // A newer edit of the same thing is in flight or being typed: it decides what shows.
                guard selection == [cue], editSeq[seqKey] == n else { return }
                render(force: revert.subtracting(dirty))
            }
        }
    }

    private func show(rejection: InspectorEdit.Rejection) {
        let key: String
        switch rejection {
        case .invalidTime: key = "inspector.invalid.time"
        case .duplicateNumber: key = "inspector.invalid.duplicateNumber"
        case .unknownTarget: key = "inspector.invalid.unknownTarget"
        case .selfTarget: key = "inspector.invalid.selfTarget"
        case .notApplicable: key = "inspector.invalid.notApplicable"
        case .decimalComma: key = "inspector.invalid.decimalComma"
        case .invalidOSC: key = "inspector.invalid.osc"
        }
        messageLabel.stringValue = loc(key)
    }

    private func inspectorField(of control: Any?) -> InspectorField? {
        InspectorField.allCases.first { field($0) === control as? NSTextField }
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        guard let f = inspectorField(of: obj.object) else { return }
        commit(f)
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard selector == #selector(NSResponder.cancelOperation(_:)), let f = inspectorField(of: control) else {
            return false
        }
        cancel(f)
        view.window?.makeFirstResponder(nil)
        return true
    }

    func controlTextDidChange(_ obj: Notification) {
        if let f = inspectorField(of: obj.object) { dirty.insert(f) }
    }

    func textDidChange(_ notification: Notification) { dirty.insert(.notes) }

    func textDidEndEditing(_ notification: Notification) { commit(.notes) }

    func textView(_ textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard selector == #selector(NSResponder.cancelOperation(_:)) else { return false }
        cancel(.notes)
        view.window?.makeFirstResponder(nil)
        return true
    }

    @objc private func popupChanged(_ sender: NSPopUpButton) {
        guard let cue else { return }
        let i = sender.indexOfSelectedItem
        if sender === colourPopup, Self.colours.indices.contains(i) {
            send(.setProperty(cue.id, .color(Self.colours[i])), cue: cue.id, key: "colour", revert: [])
        } else if sender === secondTriggerPopup, Self.secondTriggers.indices.contains(i) {
            send(
                .setProperty(cue.id, .secondTrigger(Self.secondTriggers[i])), cue: cue.id, key: "second", revert: [])
        } else if sender === continuePopup, Self.continueModes.indices.contains(i) {
            send(.setProperty(cue.id, .continueMode(Self.continueModes[i])), cue: cue.id, key: "continue", revert: [])
        }
    }

    @objc private func checkChanged(_ sender: NSButton) {
        guard let cue else { return }
        let on = sender.state == .on
        if sender === onReleaseCheck {
            send(.setProperty(cue.id, .secondTriggerOnRelease(on)), cue: cue.id, key: "release", revert: [])
            return
        }
        send(
            .setProperty(cue.id, sender === flaggedCheck ? .flagged(on) : .armed(on)), cue: cue.id,
            key: sender === flaggedCheck ? "flagged" : "armed", revert: [])
    }
}
