// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// A fader that tells its owner when a mouse gesture starts and ends, so a drag is one undo step.
final class LevelFader: NSSlider {
    var onGestureBegin: (() -> Void)?
    var onGestureEnd: (() -> Void)?

    override func mouseDown(with event: NSEvent) {
        onGestureBegin?()
        super.mouseDown(with: event)  // tracks until the button is released
        onGestureEnd?()
    }
}

/// The matrix mixer and the strip that edits its selected level: the body of an Audio cue's Levels
/// tab (AUD-06, AUD-08) and of a Fade cue's levels (CT-10). Edits are applied to the panel's own
/// copy at once and reported as a whole new `LevelSet`; the inspector sends them and calls `show`
/// again with the model, which also takes back an edit the engine refused.
@MainActor
final class LevelsPanel: NSObject, NSTextFieldDelegate {
    let isFade: Bool
    let view = NSStackView()
    let matrix = LevelMatrixView()
    let scroll = NSScrollView()
    let cellLabel = NSTextField(labelWithString: "")
    let levelField = NSTextField()
    let fader = LevelFader(
        value: LevelText.floorDb, minValue: LevelText.floorDb, maxValue: 0, target: nil, action: nil)
    let muteCheck = NSButton(checkboxWithTitle: loc("levels.mute"), target: nil, action: nil)
    let activeCheck = NSButton(checkboxWithTitle: loc("levels.active"), target: nil, action: nil)
    let gangField = NSTextField()
    let gangLabel = NSTextField(labelWithString: loc("levels.gang"))
    /// Peaks of the cue's output channels beside the fader (an Audio cue's Levels tab only).
    let meter = MeterView()
    let silentButton = NSButton(title: loc("levels.setSilent"), target: nil, action: nil)
    let fromTargetButton = NSButton(title: loc("levels.fromTarget"), target: nil, action: nil)

    /// A whole new level set, and the gesture it belongs to (nil for a one-off edit such as a typed value).
    var onEdit: ((LevelSet, Int?) -> Void)?
    /// A gesture ended: the model may be shown again.
    var onGestureEnd: (() -> Void)?
    var onSetFromTarget: (() -> Void)?

    private(set) var levels: LevelSet?
    private(set) var selection: LevelSet.Slot = .master
    /// The gesture a drag in progress belongs to; model updates wait until it ends.
    private(set) var gesture: Int?
    private static var gestureSeq = 0
    var isLocked = false {
        didSet {
            matrix.isLocked = isLocked
            refreshStrip()
        }
    }

    init(isFade: Bool) {
        self.isFade = isFade
        super.init()
        let prefix = isFade ? "fade" : "levels"
        matrix.setAX("\(prefix)-matrix", label: loc("levels.matrix"))
        cellLabel.setAX("\(prefix)-cell")
        levelField.setAX("\(prefix)-field")
        fader.setAX("\(prefix)-fader")
        meter.setAX("\(prefix)-meter")
        muteCheck.setAX("\(prefix)-mute")
        activeCheck.setAX("\(prefix)-active")
        gangField.setAX("\(prefix)-gang")
        silentButton.setAX("\(prefix)-silent")
        fromTargetButton.setAX("\(prefix)-from-target")
        levelField.delegate = self
        gangField.delegate = self
        levelField.alignment = .right
        levelField.widthAnchor.constraint(equalToConstant: 64).isActive = true
        gangField.widthAnchor.constraint(equalToConstant: 40).isActive = true
        fader.isContinuous = true
        fader.widthAnchor.constraint(equalToConstant: 180).isActive = true
        for c in [fader as NSControl, muteCheck, activeCheck, silentButton, fromTargetButton] { c.target = self }
        fader.action = #selector(faderMoved)
        fader.onGestureBegin = { [weak self] in self?.beginGesture() }
        fader.onGestureEnd = { [weak self] in self?.endGesture() }
        muteCheck.action = #selector(muteToggled)
        activeCheck.action = #selector(activeToggled)
        silentButton.action = #selector(silenceAll)
        fromTargetButton.action = #selector(setFromTarget)
        matrix.onSelect = { [weak self] slot in self?.select(slot) }
        matrix.onDrag = { [weak self] slot, db, first in
            guard let self else { return }
            if first { self.beginGesture() }
            self.edit(gesture: self.gesture) { $0.set(slot, db: db, drag: true) }
        }
        matrix.onDragEnd = { [weak self] in self?.endGesture() }

        scroll.documentView = matrix
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .bezelBorder
        scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 60).isActive = true

        let buttons: [NSView] = isFade ? [silentButton, fromTargetButton] : [silentButton]
        var strip: [NSView] = [cellLabel, levelField, fader]
        if !isFade { strip.append(meter) }
        strip += isFade ? [activeCheck] : [muteCheck, gangLabel, gangField]
        let stripRow = NSStackView(views: strip)
        stripRow.spacing = 8
        let buttonRow = NSStackView(views: buttons)
        buttonRow.spacing = 8
        view.orientation = .vertical
        view.alignment = .leading
        view.spacing = 6
        for v in [scroll, stripRow, buttonRow] {
            view.addArrangedSubview(v)
        }
        scroll.widthAnchor.constraint(equalTo: view.widthAnchor).isActive = true
        fromTargetButton.isHidden = !isFade
        refreshStrip()
    }

    // MARK: Showing the model

    /// Shows `levels` over `inputs` file channels and `outputs` cue outputs. Ignored while a
    /// gesture is under way, so the fader does not jump under the mouse.
    func show(_ levels: LevelSet?, inputs: Int, outputs: Int) {
        guard gesture == nil else { return }
        self.levels = levels
        matrix.levels = levels
        matrix.resize(inputs: levels == nil ? 0 : inputs, outputs: levels == nil ? 0 : outputs)
        if !isValid(selection) { selection = .master }
        matrix.selection = selection
        refreshStrip()
    }

    private func isValid(_ slot: LevelSet.Slot) -> Bool {
        switch slot {
        case .master: return true
        case .output(let o): return o < matrix.outputCount
        case .cross(let i, let o): return i < matrix.inputs && o < matrix.outputCount
        }
    }

    func select(_ slot: LevelSet.Slot) {
        guard isValid(slot) else { return }
        selection = slot
        matrix.selection = slot
        refreshStrip()
    }

    private func refreshStrip() {
        guard let levels else {
            for c in [levelField, fader, muteCheck, activeCheck, gangField, silentButton, fromTargetButton]
                as [NSControl]
            {
                c.isEnabled = false
            }
            cellLabel.stringValue = ""
            return
        }
        let slot = selection
        switch slot {
        case .master: cellLabel.stringValue = loc("levels.master")
        case .output(let o): cellLabel.stringValue = String(format: loc("levels.output"), o + 1)
        case .cross(let i, let o): cellLabel.stringValue = String(format: loc("levels.cross"), i + 1, o + 1)
        }
        let active = levels.isActive(slot)
        let db = levels.db(slot)
        if levelField.currentEditor() == nil { levelField.stringValue = db.map(LevelText.format) ?? "" }
        // The fader stops at 0 dB (QLab manual p. 181); a level above it shows pinned there.
        fader.doubleValue = min(max(db ?? LevelText.floorDb, LevelText.floorDb), 0)
        activeCheck.state = active ? .on : .off
        muteCheck.state = levels.isMuted(slot) ? .on : .off
        var isOutput = false
        var gangText = ""
        if case .output(let o) = slot {
            isOutput = true
            gangText = levels.gang(of: o).map(LevelSet.gangLetter) ?? ""
        }
        if gangField.currentEditor() == nil { gangField.stringValue = gangText }
        let free = !isLocked
        levelField.isEnabled = free
        fader.isEnabled = free
        activeCheck.isEnabled = free
        // A mute belongs to a cue output or a route that exists (a route not listed is silent already).
        var hasMute = false
        switch slot {
        case .master: hasMute = false
        case .output: hasMute = true
        case .cross(let i, let o): hasMute = levels.crosspoints[.init(input: i, output: o)] != nil
        }
        muteCheck.isEnabled = free && hasMute && !levels.isFade
        gangField.isEnabled = free && isOutput && !levels.isFade
        silentButton.isEnabled = free
        fromTargetButton.isEnabled = free && canSetFromTarget
    }

    /// Set by the inspector: whether there is a target cue whose levels can be copied (Fade only).
    var canSetFromTarget = false {
        didSet { fromTargetButton.isEnabled = !isLocked && canSetFromTarget }
    }

    // MARK: Editing

    func beginGesture() {
        guard !isLocked else { return }
        Self.gestureSeq += 1
        gesture = Self.gestureSeq
    }

    func endGesture() {
        guard gesture != nil else { return }
        gesture = nil
        onGestureEnd?()
    }

    /// Applies `change` to the panel's levels and reports the result, unless it changes nothing.
    private func edit(gesture: Int? = nil, _ change: (inout LevelSet) -> Void) {
        guard !isLocked, var new = levels else { return }
        change(&new)
        guard new != levels else { return }
        levels = new
        matrix.levels = new
        refreshStrip()
        onEdit?(new, gesture)
    }

    @objc private func faderMoved() {
        let v = fader.doubleValue
        let db = v <= LevelText.floorDb ? LevelText.silence : (v * 10).rounded() / 10
        let slot = selection
        edit(gesture: gesture) { $0.set(slot, db: db, drag: true) }
    }

    @objc private func muteToggled() {
        let slot = selection
        let mute = muteCheck.state == .on
        edit { $0.setMute(slot, mute) }
    }

    @objc private func activeToggled() {
        let slot = selection
        if activeCheck.state == .on {
            // Activating takes the level it shows; a silent default is -inf, as in QLab.
            edit { $0.set(slot, db: $0.db(slot) ?? LevelText.silence) }
        } else {
            edit { $0.deactivate(slot) }
        }
        refreshStrip()
    }

    @objc private func silenceAll() { edit { $0.silenceAll() } }

    @objc private func setFromTarget() { onSetFromTarget?() }

    /// Replaces the levels with `new` as one edit (Set Levels from Target).
    func replace(with new: LevelSet) { edit { $0 = new } }

    func controlTextDidEndEditing(_ obj: Notification) {
        let slot = selection
        if obj.object as? NSTextField === levelField {
            let text = levelField.stringValue
            // An empty field on a level a Fade is not moving leaves it alone.
            if text.trimmingCharacters(in: .whitespaces).isEmpty, levels?.isActive(slot) == false {
                refreshStrip()
                return
            }
            let db = LevelText.parse(text)
            edit { $0.set(slot, db: db) }
            levelField.stringValue = levels?.db(slot).map(LevelText.format) ?? ""
        } else if obj.object as? NSTextField === gangField, case .output(let o) = slot {
            let text = gangField.stringValue
            edit { $0.setGang(of: o, letter: text) }
            refreshStrip()
        }
    }
}
