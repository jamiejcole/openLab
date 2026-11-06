// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// The patch editor (PB-11), opened from the Edit button on a patch in the workspace settings
/// (QLab 5 opens its patch editor the same way). Its routing tab starts with the patch's main fader,
/// then one fader per output, each with a dB box beneath. It holds nothing the model does not: every
/// control is drawn from the patch in the store's model, and an edit goes to the engine, which puts
/// the level within the workspace volume limits; what comes back in the model is what is shown.
/// The cue outputs and device outputs tabs belong to the rest of the patch editor.
@MainActor
final class PatchEditorWindowController: NSWindowController, NSTextFieldDelegate {
    let store: WorkspaceStore
    let patchID: ModelID
    /// Sized before its tabs are added: a zero-sized tab view lays its tab strip out at a negative width.
    let tabs = NSTabView(frame: NSRect(x: 0, y: 0, width: 320, height: 240))
    let titleLabel = NSTextField(labelWithString: "")
    let mainFader = LevelFader(value: 0, minValue: LevelText.floorDb, maxValue: 0, target: nil, action: nil)
    let mainField = NSTextField()
    private(set) var outputFaders: [LevelFader] = []
    private(set) var outputFields: [NSTextField] = []
    let doneButton = NSButton(title: loc("patchEditor.done"), target: nil, action: nil)
    /// Why the last level edit did not take.
    let messageLabel = NSTextField(wrappingLabelWithString: "")
    private let columns = NSStackView()
    private var subs: [AnyCancellable] = []
    /// The field whose text was typed and not yet committed: a model update leaves it alone.
    private var typedField: NSTextField?
    /// A fader drag in progress: the edit belongs to one undo step and the model waits for its end.
    private var dragging = false
    /// Each drag's own undo step: its edits coalesce with each other and no other drag's.
    private var gesture = 0
    /// A coalesce key lives as long as the engine's history, which outlives the app.
    private static let launchToken = UUID().uuidString

    private struct Drawn: Equatable {
        var limits: VolumeLimits
        var main: Double
        var outputs: [Double]
        var name: String
    }
    private var drawn: Drawn?

    init(store: WorkspaceStore, patch: ModelID) {
        self.store = store
        patchID = patch
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 420, height: 320),
            styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.identifier = NSUserInterfaceItemIdentifier(AX.patchEditorWindow)
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.contentView = buildContent()
        window.center()
        subs.append(store.$model.sink { [weak self] _ in DispatchQueue.main.async { self?.render() } })
        render()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Opens with no field focused: AppKit would put the caret in the first box, where the next Space
    /// (the GO key, in the hands of an operator mid-show) would replace the level.
    func present() {
        showWindow(nil)
        window?.makeFirstResponder(nil)
    }

    // MARK: Building

    private func buildContent() -> NSView {
        titleLabel.font = .systemFont(ofSize: 13, weight: .semibold)
        columns.orientation = .horizontal
        columns.alignment = .top
        columns.spacing = 18
        configure(mainFader, mainField, fader: AX.patchEditorMainFader, field: AX.patchEditorMainField)
        columns.addArrangedSubview(column(title: loc("patchEditor.main"), fader: mainFader, field: mainField))

        let item = NSTabViewItem(identifier: "routing")
        item.label = loc("patchEditor.tab.routing")
        let holder = NSView()
        columns.translatesAutoresizingMaskIntoConstraints = false
        holder.addSubview(columns)
        NSLayoutConstraint.activate([
            columns.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: 16),
            columns.topAnchor.constraint(equalTo: holder.topAnchor, constant: 16),
        ])
        item.view = holder
        tabs.addTabViewItem(item)
        tabs.setAX(AX.patchEditorTabs)

        doneButton.setAX(AX.patchEditorDone, label: loc("patchEditor.done"))
        doneButton.keyEquivalent = "\r"
        doneButton.target = self
        doneButton.action = #selector(doneClicked)
        let bottom = NSStackView(views: [doneButton])
        bottom.alignment = .trailing
        messageLabel.textColor = .systemRed
        let root = NSStackView(views: [titleLabel, tabs, messageLabel, bottom])
        root.orientation = .vertical
        root.alignment = .width
        root.spacing = 10
        root.edgeInsets = NSEdgeInsets(top: 12, left: 12, bottom: 12, right: 12)
        return root
    }

    private func configure(_ fader: LevelFader, _ field: NSTextField, fader fid: String, field ffid: String) {
        fader.isVertical = true
        fader.isContinuous = true
        fader.setAX(fid)
        field.setAX(ffid)
        field.alignment = .right
        field.delegate = self
        field.widthAnchor.constraint(equalToConstant: 56).isActive = true
        fader.heightAnchor.constraint(equalToConstant: 150).isActive = true
        fader.target = self
        fader.action = #selector(faderMoved(_:))
        fader.onGestureBegin = { [weak self] in
            self?.dragging = true
            self?.gesture += 1
        }
        fader.onGestureEnd = { [weak self] in
            self?.dragging = false
            self?.render()
        }
    }

    private func column(title: String, fader: NSSlider, field: NSTextField) -> NSStackView {
        let label = NSTextField(labelWithString: title)
        label.alignment = .center
        let stack = NSStackView(views: [label, fader, field])
        stack.orientation = .vertical
        stack.alignment = .centerX
        stack.spacing = 6
        return stack
    }

    // MARK: Drawing the model

    private var patch: ModelObject? { store.model?.patches.first { $0.id == patchID } }

    /// The patch's levels as the model has them, `-120` for silence.
    private func levels(of patch: ModelObject) -> (main: Double, outputs: [Double]) {
        (
            patch.raw["master_db"]?.doubleValue ?? 0,
            (patch.raw["outputs"]?.arrayValue ?? []).map { $0["level_db"]?.doubleValue ?? 0 }
        )
    }

    private var limits: VolumeLimits { store.volumeLimits }

    /// Redraws every control from the model, except a field being typed in or a fader being dragged.
    func render() {
        guard let patch else {
            window?.close()  // the patch is gone
            return
        }
        let now = (limits: limits, levels: levels(of: patch), name: patch.raw["name"]?.stringValue ?? "")
        let state = Drawn(limits: now.limits, main: now.levels.main, outputs: now.levels.outputs, name: now.name)
        if let drawn, drawn == state, typedField == nil, !dragging { return }
        guard !dragging else { return }
        drawn = state
        window?.title = String(format: loc("patchEditor.title"), now.name)
        titleLabel.stringValue = now.name
        // One column per output, after the main one.
        if outputFaders.count != now.levels.outputs.count {
            while columns.arrangedSubviews.count > 1, let last = columns.arrangedSubviews.last {
                columns.removeArrangedSubview(last)
                last.removeFromSuperview()
            }
            outputFaders = []
            outputFields = []
            for i in 0..<now.levels.outputs.count {
                let fader = LevelFader(value: 0, minValue: LevelText.floorDb, maxValue: 0, target: nil, action: nil)
                let field = NSTextField()
                configure(
                    fader, field, fader: "\(AX.patchEditorOutputFader)-\(i + 1)",
                    field: "\(AX.patchEditorOutputField)-\(i + 1)")
                fader.tag = i + 1
                field.tag = i + 1
                outputFaders.append(fader)
                outputFields.append(field)
                columns.addArrangedSubview(
                    column(title: String(format: loc("patchEditor.output"), i + 1), fader: fader, field: field))
            }
        }
        mainField.tag = 0
        mainFader.tag = 0
        show(now.levels.main, fader: mainFader, field: mainField, limits: now.limits)
        for (i, db) in now.levels.outputs.enumerated() {
            show(db, fader: outputFaders[i], field: outputFields[i], limits: now.limits)
        }
    }

    private func show(_ db: Double, fader: LevelFader, field: NSTextField, limits: VolumeLimits) {
        fader.minValue = limits.min
        fader.maxValue = limits.max
        fader.doubleValue = min(max(db, limits.min), limits.max)
        if field !== typedField { field.stringValue = LevelText.format(db) }
    }

    // MARK: Editing

    /// Sends one fader's level. `coalesce` joins a drag's edits into one undo step.
    private func send(_ db: Double, tag: Int, coalesce: String?) {
        let command = Command.setPatchLevel(patchID, output: tag == 0 ? nil : tag - 1, db: db)
        Task {
            switch await store.send(command: command, coalesce: coalesce) {
            case .success: messageLabel.stringValue = ""
            case .failure(let error):
                messageLabel.stringValue = String(format: loc("patchEditor.refused"), error.detailText)
            }
            drawn = nil
            render()
        }
    }

    @objc private func faderMoved(_ sender: LevelFader) {
        let db = LevelText.clamp((sender.doubleValue * 10).rounded() / 10, limits: limits)
        let key = dragging ? "patch-fader:\(patchID.raw):\(Self.launchToken):\(gesture):\(sender.tag)" : nil
        send(db, tag: sender.tag, coalesce: key)
        // The field follows the fader as it moves; the model confirms it.
        (sender.tag == 0 ? mainField : outputFields[sender.tag - 1]).stringValue = LevelText.format(db)
    }

    func controlTextDidChange(_ obj: Notification) { typedField = obj.object as? NSTextField }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        // Esc aborts the editor, which posts no end of editing: the typing ends here.
        if selector == #selector(NSResponder.cancelOperation(_:)) { typedField = nil }
        return control.endTyping(by: selector)
    }

    /// Every way out of typing: Return, Tab or a click elsewhere commits the text that was typed, as
    /// the cue levels do; a field only clicked into writes nothing.
    func controlTextDidEndEditing(_ obj: Notification) {
        let typed = typedField
        typedField = nil
        guard let field = obj.object as? NSTextField, field === typed else {
            drawn = nil
            render()
            return
        }
        // Emptied is -inf (QLab 5 manual p. 204). Only white space is no change: a stray Space, the
        // GO key, leaves that (ours; docs/todo/open-qlab-questions.md row 103).
        if LevelText.isStraySpace(field.stringValue) {
            drawn = nil
            render()
            return
        }
        let db = LevelText.parse(field.stringValue, limits: limits)
        send(db, tag: field.tag, coalesce: nil)
    }

    @objc private func doneClicked() { window?.close() }
}
