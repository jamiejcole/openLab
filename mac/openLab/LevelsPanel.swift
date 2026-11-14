// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// A fader that tells its owner when a mouse gesture starts and ends, so a drag is one undo step.
final class LevelFader: NSSlider {
    var onGestureBegin: (() -> Void)?
    var onGestureEnd: (() -> Void)?

    override class var cellClass: AnyClass? {
        get { LevelFaderCell.self }
        set {}
    }

    /// The level differs between the cues shown: no knob is drawn (no position is theirs); a press on
    /// the track still sets it on all of them.
    var isMixed: Bool {
        get { (cell as? LevelFaderCell)?.isMixed ?? false }
        set {
            guard newValue != isMixed else { return }
            (cell as? LevelFaderCell)?.isMixed = newValue
            needsDisplay = true
        }
    }

    override func mouseDown(with event: NSEvent) {
        onGestureBegin?()
        super.mouseDown(with: event)  // tracks until the button is released
        onGestureEnd?()
    }
}

final class LevelFaderCell: NSSliderCell {
    var isMixed = false

    override func drawKnob(_ knobRect: NSRect) {
        if !isMixed { super.drawKnob(knobRect) }
    }
}

/// The scroll view around the levels: it says when it has been laid out, so the faders fill its height.
final class LevelsScrollView: NSScrollView {
    var onLayout: (() -> Void)?

    override func layout() {
        super.layout()
        onLayout?()
    }
}

/// What the levels scroll over: the faders, the value boxes under them and the field that types in a box.
final class LevelsBoard: NSView {
    override var isFlipped: Bool { true }
}

/// An Audio cue's Levels tab (AUD-06, AUD-08) and a Fade cue's levels (CT-10), laid out as QLab's
/// are: buttons in a column on the left; on the right a fader for the main level and for each cue
/// output, each over its value box, and the crosspoints below. Edits are applied to the panel's own
/// copy at once and reported as a whole new `LevelSet`; the inspector sends them and calls `show`
/// again with the model, which also takes back an edit the engine refused.
@MainActor
final class LevelsPanel: NSObject, NSTextFieldDelegate {
    let isFade: Bool
    let view = NSStackView()
    /// The left column; the inspector adds a Fade's own controls at its end.
    let side = NSStackView()
    let matrix = LevelMatrixView()
    let board = LevelsBoard()
    let scroll = LevelsScrollView()
    /// The name of the chosen level, over the controls that act on it.
    let cellLabel = NSTextField(labelWithString: "")
    /// Types in the chosen level's box: laid over it while typing, hidden otherwise.
    let levelField = NSTextField()
    /// The main level's fader, then one per cue output.
    private(set) var faders: [LevelFader] = []
    /// The chosen level's fader (the main one while a crosspoint is chosen: those have none).
    var fader: LevelFader {
        if case .output(let o) = selection, faders.indices.contains(o + 1) { return faders[o + 1] }
        return faders[0]
    }
    let muteCheck = NSButton(checkboxWithTitle: loc("levels.mute"), target: nil, action: nil)
    let activeCheck = NSButton(checkboxWithTitle: loc("levels.active"), target: nil, action: nil)
    let gangField = NSTextField()
    let gangLabel = NSTextField(labelWithString: loc("levels.gang"))
    /// Peaks of the cue's output channels (an Audio cue's Levels tab only).
    let meter = MeterView()
    let defaultButton = NSButton(title: loc("levels.setDefault"), target: nil, action: nil)
    let silentButton = NSButton(title: loc("levels.setSilent"), target: nil, action: nil)
    let fromTargetButton = NSButton(title: loc("levels.fromTarget"), target: nil, action: nil)
    /// Opens the patch editor of the cue's patch (an Audio cue's Levels tab; QLab 5 manual p. 189).
    let editPatchButton = NSButton(title: loc("levels.editPatch"), target: nil, action: nil)
    var onEditPatch: (() -> Void)?
    static let sideWidth: CGFloat = 170
    /// The least height a fader has; a shorter tab scrolls.
    static let minFaderHeight: CGFloat = 70

    /// A whole new level set, and the gesture it belongs to (nil for a one-off edit such as a typed value).
    var onEdit: ((LevelSet, Int?) -> Void)?
    /// A gesture ended: the model may be shown again.
    var onGestureEnd: (() -> Void)?
    var onSetFromTarget: (() -> Void)?
    /// A drag starts or ends, or the mouse goes down or up on the matrix: the edit must stay on this
    /// cue until it is over.
    var onHold: ((Bool) -> Void)?

    /// The workspace volume limits the levels live in (the store's).
    var limits = VolumeLimits() {
        didSet {
            guard limits != oldValue else { return }
            matrix.limits = limits
            refreshStrip()
        }
    }
    private(set) var levels: LevelSet?
    /// Levels that differ between the cues shown (several selected): their boxes and field show no value.
    var mixed: Set<LevelSet.Slot> = [] {
        didSet {
            guard mixed != oldValue else { return }
            matrix.mixed = mixed
            refreshStrip()
        }
    }
    /// The cues shown are ganged differently: no gang is drawn and none can be set.
    var gangsMixed = false {
        didSet {
            guard gangsMixed != oldValue else { return }
            matrix.hidesGangs = gangsMixed
            refreshStrip()
        }
    }
    /// What the last edit did, for applying it to several cues each on its own levels.
    private(set) var lastEdit: LevelSet.Edit?
    /// Several cues are shown: an action is reported even when the shown (first) cue already has its
    /// result, since another cue may not.
    var editsSeveral = false
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
        meter.setAX("\(prefix)-meter")
        muteCheck.setAX("\(prefix)-mute")
        activeCheck.setAX("\(prefix)-active")
        gangField.setAX("\(prefix)-gang")
        defaultButton.setAX("\(prefix)-default")
        silentButton.setAX("\(prefix)-silent")
        fromTargetButton.setAX("\(prefix)-from-target")
        editPatchButton.setAX(AX.levelsEditPatch, label: loc("levels.editPatch"))
        editPatchButton.target = self
        editPatchButton.action = #selector(editPatchClicked)
        levelField.delegate = self
        gangField.delegate = self
        levelField.alignment = .center
        levelField.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        levelField.isHidden = true
        gangField.widthAnchor.constraint(equalToConstant: 40).isActive = true
        for c in [muteCheck, activeCheck, defaultButton, silentButton, fromTargetButton] as [NSControl] {
            c.target = self
        }
        muteCheck.action = #selector(muteToggled)
        activeCheck.action = #selector(activeToggled)
        defaultButton.action = #selector(setDefaults)
        silentButton.action = #selector(silenceAll)
        fromTargetButton.action = #selector(setFromTarget)
        matrix.onSelect = { [weak self] slot in self?.select(slot) }
        matrix.onDrag = { [weak self] slot, db, first in
            guard let self else { return }
            if first, self.gesture == nil { self.beginGesture() }
            let limits = self.limits
            self.edit(gesture: self.gesture, as: .level(slot)) { $0.set(slot, db: db, drag: true, limits: limits) }
        }
        matrix.onDragEnd = { [weak self] in self?.endGesture() }
        // From the press: a drag can start at any moment, and must start from this cue's values.
        matrix.onPress = { [weak self] on in
            if on { self?.beginGesture() } else { self?.endGesture() }
        }
        matrix.onType = { [weak self] slot in self?.beginTyping(slot) }

        board.addSubview(matrix)
        board.addSubview(levelField)
        syncFaders(outputs: 0)
        scroll.documentView = board
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        scroll.borderType = .noBorder
        scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 60).isActive = true
        scroll.onLayout = { [weak self] in self?.layoutBoard() }

        let buttons: [NSButton] =
            isFade ? [fromTargetButton, silentButton] : [defaultButton, silentButton, editPatchButton]
        let gangRow = NSStackView(views: [gangLabel, gangField])
        gangRow.spacing = 6
        let chosen: [NSView] = isFade ? [cellLabel, activeCheck] : [cellLabel, muteCheck, gangRow, meter]
        cellLabel.font = .systemFont(ofSize: 11)
        cellLabel.textColor = .secondaryLabelColor
        cellLabel.lineBreakMode = .byTruncatingTail
        cellLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        side.orientation = .vertical
        side.alignment = .leading
        side.spacing = 8
        for b in buttons {
            b.controlSize = .small
            b.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            side.addArrangedSubview(b)
            b.widthAnchor.constraint(equalTo: side.widthAnchor).isActive = true
        }
        side.setCustomSpacing(16, after: buttons[buttons.count - 1])
        for v in chosen { side.addArrangedSubview(v) }
        if !isFade { meter.widthAnchor.constraint(equalTo: side.widthAnchor).isActive = true }
        side.widthAnchor.constraint(equalToConstant: Self.sideWidth).isActive = true
        side.setHuggingPriority(.defaultHigh, for: .vertical)
        view.orientation = .horizontal
        view.alignment = .top
        view.spacing = 12
        view.addArrangedSubview(side)
        view.addArrangedSubview(scroll)
        scroll.heightAnchor.constraint(equalTo: view.heightAnchor).isActive = true
        scroll.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        refreshStrip()
    }

    // MARK: Faders and the board

    /// One fader for the main level and one per cue output, made or dropped to match `outputs`.
    private func syncFaders(outputs: Int) {
        let wanted = outputs + 1
        while faders.count > wanted { faders.removeLast().removeFromSuperview() }
        while faders.count < wanted {
            let n = faders.count
            let f = LevelFader(
                value: LevelText.floorDb, minValue: LevelText.floorDb, maxValue: 0, target: self,
                action: #selector(faderMoved(_:)))
            f.isVertical = true
            f.isContinuous = true
            f.controlSize = .small
            f.tag = n
            let prefix = isFade ? "fade" : "levels"
            // The main fader keeps the name the single fader had.
            f.setAX(n == 0 ? "\(prefix)-fader" : "\(prefix)-fader-\(n)", label: Self.name(of: Self.slot(ofFader: n)))
            f.onGestureBegin = { [weak self] in
                self?.select(Self.slot(ofFader: n))
                self?.beginGesture()
            }
            f.onGestureEnd = { [weak self] in self?.endGesture() }
            faders.append(f)
            board.addSubview(f)
        }
    }

    private static func slot(ofFader n: Int) -> LevelSet.Slot { n == 0 ? .master : .output(n - 1) }

    static func name(of slot: LevelSet.Slot) -> String {
        switch slot {
        case .master: return loc("levels.master")
        case .output(let o): return String(format: loc("levels.output"), o + 1)
        case .cross(let i, let o): return String(format: loc("levels.cross"), i + 1, o + 1)
        }
    }

    /// Sizes the board to the scroll view and puts the faders over their boxes: the boxes sit at the
    /// bottom and the faders take the height above them. Worked out from the scroll view's frame,
    /// never its clip view (whose size changes as scroll bars come and go).
    func layoutBoard() {
        let bar = NSScroller.scrollerWidth(for: .regular, scrollerStyle: .legacy)
        let top: CGFloat = 4
        let gap: CGFloat = 6
        let least = top + Self.minFaderHeight + gap + matrix.frame.height
        let size = NSSize(
            width: max(matrix.frame.width, 1), height: max(least, (scroll.frame.height - bar).rounded(.down)))
        if board.frame.size != size { board.setFrameSize(size) }
        let boxesTop = size.height - matrix.frame.height
        if matrix.frame.origin != NSPoint(x: 0, y: boxesTop) { matrix.setFrameOrigin(NSPoint(x: 0, y: boxesTop)) }
        for (n, f) in faders.enumerated() {
            let box = matrix.rect(of: Self.slot(ofFader: n))
            let frame = NSRect(x: box.midX - 11, y: top, width: 22, height: boxesTop - gap - top)
            if f.frame != frame { f.frame = frame }
        }
        if !levelField.isHidden { levelField.frame = fieldFrame(selection) }
    }

    private func fieldFrame(_ slot: LevelSet.Slot) -> NSRect {
        board.convert(matrix.rect(of: slot), from: matrix).insetBy(dx: 1, dy: 0)
    }

    /// Opens the chosen level's box for typing (a double-click on it, or Return).
    func beginTyping(_ slot: LevelSet.Slot) {
        guard !isLocked, levels != nil else { return }
        select(slot)
        levelField.frame = fieldFrame(slot)
        levelField.isHidden = false
        levelField.window?.makeFirstResponder(levelField)
        levelField.currentEditor()?.selectAll(nil)
    }

    // MARK: Showing the model

    /// The grid size a `show` asked for while a level was being typed; applied when editing ends, so
    /// the text never lands on a slot that moved under the cursor.
    private var heldSize: (inputs: Int, outputs: Int)?

    /// Text has been typed in a field and not yet committed or dropped (a field that was only
    /// clicked into is not: nothing of it is held or written).
    private var typedField: NSTextField?
    private var isDirty: Bool { typedField != nil }

    /// Shows `levels` over `inputs` file channels and `outputs` cue outputs. Ignored while a
    /// gesture is under way, so the fader does not jump under the mouse. While text is being typed
    /// the levels are taken (a peer's change must not be written back by the commit) but the grid
    /// size is held, so the selection does not move under the text.
    func show(_ levels: LevelSet?, inputs: Int, outputs: Int) {
        guard gesture == nil else { return }
        if isDirty, levels != nil {
            heldSize = (inputs, outputs)
            self.levels = levels
            matrix.levels = levels
            refreshStrip()
            return
        }
        place(levels, inputs: inputs, outputs: outputs)
    }

    private func place(_ levels: LevelSet?, inputs: Int, outputs: Int) {
        heldSize = nil
        self.levels = levels
        matrix.levels = levels
        matrix.resize(inputs: levels == nil ? 0 : inputs, outputs: levels == nil ? 0 : outputs)
        syncFaders(outputs: matrix.outputCount)
        layoutBoard()
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

    @objc private func editPatchClicked() { onEditPatch?() }

    private func refreshStrip() {
        // Opening the editor changes nothing; Show Mode keeps it shut like every other edit.
        editPatchButton.isEnabled = !isLocked && levels != nil
        guard let levels else {
            for c in [levelField, muteCheck, activeCheck, gangField, defaultButton, silentButton, fromTargetButton]
                as [NSControl]
            {
                c.isEnabled = false
            }
            for f in faders { f.isEnabled = false }
            cellLabel.stringValue = ""
            return
        }
        let slot = selection
        cellLabel.stringValue = Self.name(of: slot)
        let active = levels.isActive(slot)
        let db = levels.db(slot)
        if !isDirty { levelField.stringValue = mixed.contains(slot) ? "" : db.map(LevelText.format) ?? "" }
        let free = !isLocked
        // A fader runs from the minimum limit and stops at 0 dB (QLab manual p. 181), or at the
        // maximum if that is lower; a level above it shows pinned there.
        for (n, f) in faders.enumerated() {
            f.minValue = limits.min
            f.maxValue = min(limits.max, 0)
            f.doubleValue = min(max(levels.db(Self.slot(ofFader: n)) ?? limits.min, limits.min), f.maxValue)
            f.isMixed = mixed.contains(Self.slot(ofFader: n))
            f.isEnabled = free
        }
        activeCheck.state = active ? .on : .off
        muteCheck.state = levels.isMuted(slot) ? .on : .off
        var isOutput = false
        var gangText = ""
        if case .output(let o) = slot {
            isOutput = true
            gangText = levels.gang(of: o).map(LevelSet.gangLetter) ?? ""
        }
        if !isDirty { gangField.stringValue = gangsMixed ? "" : gangText }
        levelField.isEnabled = free
        activeCheck.isEnabled = free
        // A mute belongs to a cue output or a route that exists (a route not listed is silent already).
        var hasMute = false
        switch slot {
        case .master: hasMute = false
        case .output: hasMute = true
        case .cross(let i, let o): hasMute = levels.crosspoints[.init(input: i, output: o)] != nil
        }
        muteCheck.isEnabled = free && hasMute && !levels.isFade
        gangField.isEnabled = free && isOutput && !levels.isFade && !gangsMixed
        defaultButton.isEnabled = free
        silentButton.isEnabled = free
        fromTargetButton.isEnabled = free && canSetFromTarget
    }

    /// Set by the inspector: whether there is a target cue whose levels can be copied (Fade only).
    var canSetFromTarget = false {
        didSet { fromTargetButton.isEnabled = !isLocked && canSetFromTarget }
    }

    // MARK: Editing

    /// Commits text typed in a field and closes its editor: before Save, Close, Quit or Show Mode, and
    /// before a drag starts (a typed value must not be written over the drag when the field is left).
    func commitTyping() {
        guard let typed = typedField else { return }
        finishTyping(commit: typed)
        typed.window?.makeFirstResponder(nil)
    }

    func beginGesture() {
        guard !isLocked else { return }
        commitTyping()
        Self.gestureSeq += 1
        gesture = Self.gestureSeq
        onHold?(true)
    }

    func endGesture() {
        guard gesture != nil else { return }
        gesture = nil
        matrix.forgetPress()
        onGestureEnd?()
        onHold?(false)
    }

    /// Applies `change` to the panel's levels and reports the result, unless it changes nothing.
    private func edit(gesture: Int? = nil, as action: LevelSet.Edit?, _ change: (inout LevelSet) -> Void) {
        guard !isLocked, var new = levels else { return }
        change(&new)
        guard new != levels || (editsSeveral && action != nil) else { return }
        lastEdit = action
        levels = new
        matrix.levels = new
        refreshStrip()
        onEdit?(new, gesture)
    }

    @objc private func faderMoved(_ sender: LevelFader) {
        let v = sender.doubleValue
        let limits = limits
        let db = v <= limits.min ? LevelText.silence : (v * 10).rounded() / 10
        let slot = Self.slot(ofFader: sender.tag)
        if selection != slot { select(slot) }
        edit(gesture: gesture, as: .level(slot)) { $0.set(slot, db: db, drag: true, limits: limits) }
    }

    @objc private func muteToggled() {
        let slot = selection
        let mute = muteCheck.state == .on
        edit(as: .mute(slot)) { $0.setMute(slot, mute) }
    }

    @objc private func activeToggled() {
        let slot = selection
        if activeCheck.state == .on {
            // Activating takes the level it shows; a silent default is -inf, as in QLab.
            let limits = limits
            edit(as: nil) { $0.set(slot, db: $0.db(slot) ?? LevelText.silence, limits: limits) }
        } else {
            edit(as: nil) { $0.deactivate(slot) }
        }
        refreshStrip()
    }

    @objc private func silenceAll() { edit(as: .silenceAll) { $0.silenceAll() } }

    /// A new Audio cue's levels: everything at 0 dB, file channel n to cue output n.
    @objc private func setDefaults() { edit(as: .defaults) { $0.setDefaults() } }

    @objc private func setFromTarget() { onSetFromTarget?() }

    /// Replaces the levels with `new` as one edit (Set Levels from Target).
    func replace(with new: LevelSet) { edit(as: nil) { $0 = new } }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        // Esc aborts the editor, which posts no end of editing: the typing ends here.
        if selector == #selector(NSResponder.cancelOperation(_:)) { finishTyping(commit: nil) }
        return control.endTyping(by: selector)
    }

    func controlTextDidChange(_ obj: Notification) { typedField = obj.object as? NSTextField }

    func controlTextDidEndEditing(_ obj: Notification) { finishTyping(commit: obj.object as? NSTextField) }

    /// Every way out of typing ends here: Return, Tab and a click elsewhere (`commit` is the field),
    /// Esc (nil). Only text that was typed is committed, so leaving a field untouched writes nothing
    /// (a level that does not round-trip through its text, -6.25 shown as -6.2, stays as it is). The
    /// grid size a `show` held meanwhile is applied, and the strip shows the levels as they now are.
    private func finishTyping(commit field: NSTextField?) {
        let typed = typedField
        typedField = nil
        if let typed, field != nil { commitText(of: typed) }
        if field == nil || field === levelField { levelField.isHidden = true }
        if let held = heldSize, gesture == nil {
            place(levels, inputs: held.inputs, outputs: held.outputs)
        } else {
            refreshStrip()
        }
    }

    private func commitText(of field: NSTextField) {
        let slot = selection
        if field === levelField {
            let text = levelField.stringValue
            // Emptied is -inf (QLab 5 manual p. 204), except on a level a Fade is not moving, which
            // it leaves alone. Only white space is no change: a stray Space, the GO key, leaves that
            // (ours).
            if LevelText.isStraySpace(text) || (text.isEmpty && levels?.isActive(slot) == false) { return }
            let limits = limits
            let db = LevelText.parse(text, limits: limits)
            edit(as: .level(slot)) { $0.set(slot, db: db, limits: limits) }
        } else if field === gangField, case .output(let o) = slot {
            let text = gangField.stringValue
            edit(as: .gang(output: o, letter: text)) { $0.setGang(of: o, letter: text) }
        }
    }
}
