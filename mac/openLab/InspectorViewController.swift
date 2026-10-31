// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// Bottom inspector: the Basics tab for the selected cue (ED-01, CUE-01..CUE-03). Edits go to the
/// store as model commands; fields show the store's model, so a refused or invalid edit simply
/// puts the model's value back. A field being edited is never overwritten by a model change.
@MainActor
final class InspectorViewController: NSViewController, NSTextFieldDelegate, NSTextViewDelegate, NSTabViewDelegate {
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
    let groupModePopup = NSPopUpButton()
    let groupLoopCheck = NSButton(checkboxWithTitle: loc("inspector.groupLoop"), target: nil, action: nil)
    let groupShuffleCheck = NSButton(checkboxWithTitle: loc("inspector.groupShuffle"), target: nil, action: nil)
    /// The Mode tab, in the tab view only while a Group cue is selected.
    private var modeItem: NSTabViewItem?
    /// The Levels tab of an Audio cue (AUD-06, AUD-08) and the Fade tab of a Fade cue (CT-10), each in
    /// the tab view only while such a cue is selected.
    let audioLevels = LevelsPanel(isFade: false)
    let fadeLevels = LevelsPanel(isFade: true)
    let fadeSettings = FadeSettingsPanel()
    var levelsItem: NSTabViewItem?
    var fadeItem: NSTabViewItem?
    /// The timing and loops tab of an Audio cue (AUD-11, AUD-12, AUD-14), in the tab view only while
    /// one is selected.
    let timeLoops = TimeLoopsPanel()
    var timeLoopsItem: NSTabViewItem?
    /// A levels or fade edit sends only its changed `kind_fields`, but is built from the kind as the
    /// previous unanswered edit left it, so those fields start from what was sent (as `pendingTriggers`).
    var pendingKinds: [ModelID: JSONValue] = [:]
    static let groupModes = [
        "timeline", "playlist", "start_first_and_enter", "start_first_and_go_to_next", "start_random",
    ]
    /// A Wait or Fade cue's duration (QLab's Basics tab); empty and disabled for other cues.
    let durationField = NSTextField()
    /// The Load Time tab's one field, in the tab view only while a Load cue is selected (CT-14).
    let loadTimeField = NSTextField()
    var loadTimeItem: NSTabViewItem?
    let kindIcon = NSImageView()
    let notesView = NSTextView()
    let colourPopup = NSPopUpButton()
    let continuePopup = NSPopUpButton()
    let flaggedCheck = NSButton(checkboxWithTitle: loc("inspector.flagged"), target: nil, action: nil)
    let armedCheck = NSButton(checkboxWithTitle: loc("inspector.armed"), target: nil, action: nil)
    let messageLabel = NSTextField(labelWithString: "")
    /// Preview / Stop Preview of the selected audio cue. M5.8 (Time & Loops tab) should host it.
    let previewButton = NSButton(title: loc("inspector.preview"), target: nil, action: nil)
    /// Choose... next to an Audio cue's file: opens the file chooser (set by the window controller).
    let chooseButton = NSButton(title: loc("inspector.target.choose"), target: nil, action: nil)
    var onChooseFile: ((ModelID) -> Void)?
    /// Sends a transport action (set by the window controller).
    var onTransport: ((TransportAction) -> Void)?
    private var connectionSub: AnyCancellable?

    /// Show Mode (SM-01): every control is read-only and every edit path refuses, so a stray key
    /// or click during a performance cannot change the show.
    var isLocked = false {
        didSet {
            guard isLocked != oldValue else { return }
            applyLock()
            if !isLocked { render() }
        }
    }

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
    var cueForEdit: Cue? {
        guard var c = cue else { return nil }
        if let t = pendingTriggers[c.id] { c.raw.set("triggers", t) }
        if let k = pendingKinds[c.id] { c.raw.set("kind", k) }
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
        case .duration: return durationField
        case .loadTime: return loadTimeField
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
            (durationField, AX.inspectorDuration), (loadTimeField, AX.inspectorLoadTime),
        ] {
            f.setAX(id)
            f.delegate = self
            f.lineBreakMode = .byTruncatingTail
            f.usesSingleLineMode = true
        }
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
        previewButton.setAX(AX.inspectorPreview)
        previewButton.target = self
        previewButton.action = #selector(previewClicked)
        chooseButton.setAX(AX.inspectorChoose)
        chooseButton.target = self
        chooseButton.action = #selector(chooseClicked)
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
        // Number and Duration share a row, as do the two waits, so the five fields fit the inspector's
        // 240 pt height: stacked singly they overflowed and the top rows overlapped (#445).
        for f in [numberField, preWaitField, postWaitField, durationField] {
            f.widthAnchor.constraint(equalToConstant: 80).isActive = true
        }
        loadTimeField.widthAnchor.constraint(equalToConstant: 130).isActive = true
        func pair(_ a: NSView, _ b: NSView) -> NSStackView {
            let s = NSStackView(views: [a, b])
            s.spacing = 8
            s.alignment = .top
            return s
        }
        kindIcon.widthAnchor.constraint(equalToConstant: 20).isActive = true
        let nameRow = NSStackView(views: [kindIcon, nameField])
        nameRow.spacing = 4
        nameField.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let left = column(
            [
                pair(
                    row(loc("cuelist.col.number"), numberField), row(loc("cuelist.col.duration"), durationField)),
                pair(
                    row(loc("cuelist.col.preWait"), preWaitField), row(loc("cuelist.col.postWait"), postWaitField)),
                row(loc("inspector.continue"), continuePopup),
            ], width: 168)
        let targetRow = NSStackView(views: [targetField, chooseButton])
        targetRow.spacing = 6
        targetField.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        targetField.lineBreakMode = .byTruncatingMiddle  // the file name at the end is the part that matters
        targetField.widthAnchor.constraint(greaterThanOrEqualToConstant: 90).isActive = true
        chooseButton.setContentHuggingPriority(.required, for: .horizontal)
        chooseButton.setContentCompressionResistancePriority(.required, for: .horizontal)
        let centre = column([
            row(loc("cuelist.col.name"), nameRow), row(loc("cuelist.col.target"), targetRow),
            row(loc("inspector.notes"), scroll),
        ])
        for v in centre.arrangedSubviews { v.widthAnchor.constraint(equalTo: centre.widthAnchor).isActive = true }
        nameRow.widthAnchor.constraint(equalTo: centre.widthAnchor).isActive = true
        targetRow.widthAnchor.constraint(equalTo: centre.widthAnchor).isActive = true
        scroll.widthAnchor.constraint(equalTo: centre.widthAnchor).isActive = true
        let right = column(
            [previewButton, row(loc("inspector.colour"), colourPopup), flaggedCheck, armedCheck], width: 110)
        let columns = NSStackView(views: [left, centre, right])
        columns.orientation = .horizontal
        columns.alignment = .top
        columns.distribution = .fill
        centre.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        columns.spacing = 12
        columns.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 8, right: 12)

        let item = NSTabViewItem(identifier: "basics")
        item.label = loc("inspector.tab.basics")
        item.view = columns
        tabView.addTabViewItem(item)
        tabView.addTabViewItem(triggersTab(row: row, column: column))
        modeItem = modeTab(row: row, column: column)
        loadTimeItem = loadTimeTab(row: row, column: column)
        buildLevelsTabs()
        buildTimeLoopsTab()
        tabView.setAX(AX.inspectorTabs)
        tabView.delegate = self

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
            case .runtime(let cues, _):
                MainActor.assumeIsolated {
                    self?.updatePreviewButton()
                    // The file's length may have come (or changed): the waveform follows.
                    if let self, let cue = self.cueForEdit, cues.contains(cue.id) { self.renderTimeLoops(cue) }
                }
            case .progress(let ids):
                MainActor.assumeIsolated {
                    if let self, let id = self.cue?.id, ids.contains(id) { self.updateTimeLoopsProgress() }
                }
            case .waits, .notices: break
            }
        }
        connectionSub = store.$connectionState.sink { [weak self] state in  // willSet: use the emitted value
            MainActor.assumeIsolated { self?.updatePreviewButton(connected: state == .connected) }
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

    /// The Mode tab of a Group cue (CT-22): how starting the group starts its children. Loop and
    /// shuffle belong to a playlist only (QLab keeps them on its Playlist tab).
    private func modeTab(
        row: (String, NSView) -> NSView, column: ([NSView], CGFloat?) -> NSStackView
    ) -> NSTabViewItem {
        groupModePopup.setAX(AX.inspectorGroupMode)
        groupLoopCheck.setAX(AX.inspectorGroupLoop)
        groupShuffleCheck.setAX(AX.inspectorGroupShuffle)
        for key in Self.groupModes {
            groupModePopup.menu?.addItem(
                NSMenuItem(title: loc("inspector.groupMode.\(key)"), action: nil, keyEquivalent: ""))
        }
        for c in [groupModePopup as NSControl, groupLoopCheck, groupShuffleCheck] {
            c.target = self
        }
        groupModePopup.action = #selector(popupChanged(_:))
        groupLoopCheck.action = #selector(checkChanged(_:))
        groupShuffleCheck.action = #selector(checkChanged(_:))
        let columns = column(
            [row(loc("inspector.groupMode"), groupModePopup), groupLoopCheck, groupShuffleCheck], nil)
        columns.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 8, right: 12)
        let item = NSTabViewItem(identifier: "mode")
        item.label = loc("inspector.tab.mode")
        item.view = columns
        return item
    }

    /// The Load Time tab of a Load cue (QLab 5, manual pp. 680-681): the time its target is loaded to.
    private func loadTimeTab(
        row: (String, NSView) -> NSView, column: ([NSView], CGFloat?) -> NSStackView
    ) -> NSTabViewItem {
        let content = column([row(loc("inspector.loadTime"), loadTimeField)], nil)
        content.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 8, right: 12)
        let item = NSTabViewItem(identifier: "loadTime")
        item.label = loc("inspector.tab.loadTime")
        item.view = content
        return item
    }

    /// Shows the Mode tab for a Group cue and hides it otherwise; the controls follow the model.
    private func renderMode(_ cue: Cue) {
        guard let modeItem else { return }
        let mode = cue.kindType == "group" ? cue.raw["kind"]?["mode"] : nil
        let present = tabView.tabViewItems.contains { $0 === modeItem }
        if mode == nil, present {
            tabView.removeTabViewItem(modeItem)
        } else if mode != nil, !present {
            tabView.insertTabViewItem(modeItem, at: 1)
        }
        guard let mode else { return }
        let type = mode["type"]?.stringValue ?? "timeline"
        groupModePopup.selectItem(at: Self.groupModes.firstIndex(of: type) ?? 0)
        let playlist = type == "playlist"
        groupLoopCheck.state = playlist && mode["loop"] == .bool(true) ? .on : .off
        groupShuffleCheck.state = playlist && mode["shuffle"] == .bool(true) ? .on : .off
        groupLoopCheck.isEnabled = playlist
        groupShuffleCheck.isEnabled = playlist
    }

    /// A recorded chord: refused with a message, or sent as the cue's new hotkey. Returns whether
    /// it was sent. The other trigger fields are kept.
    @discardableResult
    func record(_ chord: KeyChord) -> Bool {
        guard !isLocked, let cue = cueForEdit, let model = store.model else { return false }
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
        guard !isLocked, let cue = cueForEdit, cue.raw["triggers"]?["hotkey"].map({ $0 != .null }) == true else {
            return
        }
        messageLabel.stringValue = ""
        send(InspectorEdit.setTriggers(cue, hotkey: .some(nil)), cue: cue.id, key: "hotkey", revert: [])
    }

    private func fillPopups() {
        for key in Self.colours {
            let style = CueKindStyle.colours[key]
            let item = NSMenuItem(title: loc(style?.labelKey ?? "cue.colour.none"), action: nil, keyEquivalent: "")
            if let style {
                let base = Icon.colourSwatch.image(loc(style.labelKey), compact: true)
                item.image = base.withSymbolConfiguration(.init(paletteColors: [style.tint])) ?? base
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
        // An unnamed cue with no file shows its kind as a hint; otherwise the field holds the shown name.
        nameField.placeholderString = CueKindStyle.kindLabel(kind)
        let style = CueKindStyle.kinds[kind]
        kindIcon.image = style.map { $0.icon.image(CueKindStyle.kindLabel(kind), compact: true) }
        let isTarget = InspectorEdit.targetKinds.contains(kind)
        let noFile = kind == "audio" && (cue.raw["kind"]?["media"]?.stringValue ?? "").isEmpty
        targetField.placeholderString =
            isTarget ? (noFile ? loc("inspector.target.noFile") : nil) : loc("inspector.target.notApplicable")
        chooseButton.isEnabled = kind == "audio" && !isLocked
        if kind == "audio" {
            targetField.stringValue = cue.raw["kind"]?["media"]?.stringValue ?? ""
        } else if !isTarget {
            targetField.stringValue = ""
        }
        targetField.isEnabled = isTarget
        targetField.toolTip = targetField.stringValue.isEmpty ? nil : targetField.stringValue
        // Only Wait and Fade cues have a duration to type; others take theirs from content or have none.
        durationField.isEnabled = InspectorEdit.durationKinds.contains(kind)
        syncTab(loadTimeItem, present: kind == "load")
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
        renderMode(cue)
        renderLevels(cueForEdit ?? cue, model: model)
        renderTimeLoops(cueForEdit ?? cue)
        updatePreviewButton()
        if isLocked { applyLock() }
    }

    /// Show Mode does not lock it: a preview plays outside the show and edits nothing (QLab's docs
    /// do not say; the safe reading is that the operator can still audition).
    func updatePreviewButton(connected: Bool? = nil) {
        let active = cue.map { store.previewing.contains($0.id) } ?? false
        previewButton.title = loc(active ? "inspector.stopPreview" : "inspector.preview")
        previewButton.isEnabled = cue?.kindType == "audio" && (connected ?? (store.connectionState == .connected))
    }

    @objc private func chooseClicked() {
        guard !isLocked, let cue, cue.kindType == "audio" else { return }
        onChooseFile?(cue.id)
    }

    @objc private func previewClicked() {
        guard let id = cue?.id else { return }
        onTransport?(store.previewing.contains(id) ? .stopPreview(cue: id) : .preview(cue: id))
    }

    private var lockedControls: [NSControl] {
        [
            numberField, nameField, preWaitField, postWaitField, durationField, loadTimeField, targetField, oscField,
            hotkeyField, hotkeyClear, chooseButton,
            secondTriggerPopup, onReleaseCheck, groupModePopup, groupLoopCheck, groupShuffleCheck, colourPopup,
            continuePopup, flaggedCheck, armedCheck,
        ]
    }

    private func applyLock() {
        _ = view
        // Callers commit first (`WorkspaceWindowController.requestShowMode`); text still dirty here
        // is dropped and the model's text put back, never left on screen as if saved.
        let stale = dirty
        if isLocked {
            dirty = []
            view.window?.makeFirstResponder(nil)
        }
        for c in lockedControls { c.isEnabled = !isLocked }
        notesView.isEditable = !isLocked
        audioLevels.isLocked = isLocked
        fadeLevels.isLocked = isLocked
        fadeSettings.isLocked = isLocked
        timeLoops.isLocked = isLocked
        if isLocked, !stale.isEmpty { render(force: stale) }
    }

    // MARK: Committing edits

    /// Sends the edit for `f` if the text changed; rejects bad input by putting the model's text back.
    @discardableResult
    func commit(_ f: InspectorField) -> Bool {
        dirty.remove(f)
        guard !isLocked, let cue = cueForEdit, let model = store.model else { return true }
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
            // Show what will be stored (`90` becomes `01:30.00`) until the model answers.
            if [.preWait, .postWait, .duration, .loadTime].contains(f), let s = CueTime.parse(text) {
                field(f)?.stringValue = CueTime.format(s)
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

    func send(
        _ command: Command, cue: ModelID, key: String, revert: Set<InspectorField>, coalesce: String? = nil
    ) {
        guard !isLocked else { return }
        let seqKey = "\(cue)/\(key)"
        let n = (editSeq[seqKey] ?? 0) + 1
        editSeq[seqKey] = n
        nextTask += 1
        let taskID = nextTask
        let property = command.json["property"]
        let sentTriggers = property?["type"]?.stringValue == "triggers" ? property?["value"] : nil
        if let sentTriggers { pendingTriggers[cue] = sentTriggers }
        let sentKind = property.flatMap { pendingKind(cue, after: $0) }
        if let sentKind { pendingKinds[cue] = sentKind }
        inFlight[taskID] = Task { [weak self] in
            guard let self else { return }
            // Cleared on success and on refusal alike. On success the model already holds the change:
            // a `result` with `ok.revision` is held until its `model_changed` is written
            // (docs/network.md), so the next edit reads the new triggers. On refusal nothing
            // changed, so a refused field must not be sent again with the next edit.
            defer {
                inFlight[taskID] = nil
                if let sentTriggers, pendingTriggers[cue] == sentTriggers { pendingTriggers[cue] = nil }
                if let sentKind, pendingKinds[cue] == sentKind { pendingKinds[cue] = nil }
            }
            let outcome = await store.send(command: command, coalesce: coalesce)
            // Answered: the model holds the edit (or refused it), so the unanswered copy must not hide it
            // from the render below.
            if let sentKind, pendingKinds[cue] == sentKind { pendingKinds[cue] = nil }
            if case .failure(let error) = outcome {
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

    /// An edit sent outside the inspector (the cue list's Duration column) that changes the cue's kind:
    /// until it is answered, a kind edit made here must build on it, not on a pendingKinds entry that
    /// still holds the old value (#354). The returned closure ends the tracking once the engine answers.
    func trackPending(_ command: Command, cue: ModelID) -> () -> Void {
        guard let property = command.json["property"], let sentKind = pendingKind(cue, after: property) else {
            return {}
        }
        pendingKinds[cue] = sentKind
        return { [weak self] in
            if self?.pendingKinds[cue] == sentKind { self?.pendingKinds[cue] = nil }
        }
    }

    private func show(rejection: InspectorEdit.Rejection) {
        messageLabel.stringValue = loc(rejection.messageKey)
    }

    /// The kind `cue` will have once `property` lands, when the property replaces it (`kind`) or a
    /// field inside it (`duration`, `load_to_time`): a kind edit sent before the answer must keep it.
    private func pendingKind(_ cue: ModelID, after property: JSONValue) -> JSONValue? {
        let type = property["type"]?.stringValue
        if type == "kind" { return property["value"] }
        if type == "kind_fields", let fields = property["value"]?.objectValue,
            let edited = cueForEdit, edited.id == cue, var kind = edited.raw["kind"]
        {
            for (field, value) in fields { kind.set(field, value) }
            return kind
        }
        guard let type, type == "duration" || type == "load_to_time", let value = property["value"],
            let edited = cueForEdit, edited.id == cue, var kind = edited.raw["kind"]
        else { return nil }
        kind.set(type, value)
        return kind
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
        guard !isLocked, let cue else { return }
        let i = sender.indexOfSelectedItem
        if sender === colourPopup, Self.colours.indices.contains(i) {
            send(.setProperty(cue.id, .color(Self.colours[i])), cue: cue.id, key: "colour", revert: [])
        } else if sender === secondTriggerPopup, Self.secondTriggers.indices.contains(i) {
            send(
                .setProperty(cue.id, .secondTrigger(Self.secondTriggers[i])), cue: cue.id, key: "second", revert: [])
        } else if sender === groupModePopup, Self.groupModes.indices.contains(i) {
            // Choosing the type the cue already has changes nothing, and must not clear the
            // playlist's loop and shuffle. A new type starts from what the model holds (a type
            // that has no loop or shuffle holds none).
            let mode = cue.raw["kind"]?["mode"]
            let type = Self.groupModes[i]
            guard type != (mode?["type"]?.stringValue ?? "timeline") else { return }
            let playlist = type == "playlist"
            sendGroupMode(
                cue, type: type, loop: playlist && mode?["loop"] == .bool(true),
                shuffle: playlist && mode?["shuffle"] == .bool(true))
        } else if sender === continuePopup, Self.continueModes.indices.contains(i) {
            send(.setProperty(cue.id, .continueMode(Self.continueModes[i])), cue: cue.id, key: "continue", revert: [])
        }
    }

    private func sendGroupMode(_ cue: Cue, type: String, loop: Bool, shuffle: Bool) {
        send(
            .setProperty(cue.id, .groupMode(type: type, loop: loop, shuffle: shuffle)), cue: cue.id, key: "groupMode",
            revert: [])
    }

    @objc private func checkChanged(_ sender: NSButton) {
        guard !isLocked, let cue else { return }
        let on = sender.state == .on
        if sender === groupLoopCheck || sender === groupShuffleCheck {
            sendGroupMode(
                cue, type: "playlist", loop: sender === groupLoopCheck ? on : groupLoopCheck.state == .on,
                shuffle: sender === groupShuffleCheck ? on : groupShuffleCheck.state == .on)
            return
        }
        if sender === onReleaseCheck {
            send(.setProperty(cue.id, .secondTriggerOnRelease(on)), cue: cue.id, key: "release", revert: [])
            return
        }
        send(
            .setProperty(cue.id, sender === flaggedCheck ? .flagged(on) : .armed(on)), cue: cue.id,
            key: sender === flaggedCheck ? "flagged" : "armed", revert: [])
    }
}
