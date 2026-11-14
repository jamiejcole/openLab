// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// The workspace settings window (UI-01, AUD-07, WS-07, PB-02, PB-03). QLab 5 has it in the
/// File menu (Cmd-,) and behind the footer gear: General (the panic fade and the time between GOs),
/// Keys, Audio (the output patch table, `+Audio`, and the volume limits), OSC and Network. It reads
/// and writes through its `WorkspaceSettingsModel`.
@MainActor
final class WorkspaceSettingsWindowController: NSWindowController, NSTextFieldDelegate, NSWindowDelegate,
    NSTabViewDelegate
{
    let model: WorkspaceSettingsModel
    /// Sized before its tabs are added: a zero-sized tab view lays its tab strip out at a negative width.
    let tabs = NSTabView(frame: NSRect(x: 0, y: 0, width: 320, height: 240))

    // General
    let panicField = NSTextField()
    let goIntervalField = NSTextField()
    let autoLoadCheck = NSButton(checkboxWithTitle: loc("settings.general.autoLoad"), target: nil, action: nil)
    // Audio: the patch table (`+Audio`) and the volume limits beside it.
    let maxLimitField = NSTextField()
    let minLimitField = NSTextField()
    let patchAddButton = NSButton(title: loc("settings.audio.newPatch"), target: nil, action: nil)
    /// The table (made again for each drawing) and the view it sits in, in a scroll view.
    var patchGrid = NSGridView()
    let patchGridHolder = WidthWatchingView()
    /// One per patch row, in table order; rebuilt whenever the table is drawn.
    var patchNameFields: [NSTextField] = []
    var patchDevicePopups: [NSPopUpButton] = []
    var patchEditButtons: [NSButton] = []
    var patchRemoveButtons: [NSButton] = []
    var patchProblemLabels: [NSTextField] = []
    var shownPatchTable: PatchTableState?
    /// The patch editor last opened; each patch's editor lives as long as this window does.
    var patchEditor: PatchEditorWindowController?
    var patchEditors: [ModelID: PatchEditorWindowController] = [:]
    /// Whether an opened patch editor is put on screen (a scenario run draws it off screen).
    var presentsEditors = true

    // OSC passcode table (NET-02); built and drawn in `+Osc`.
    let oscAddButton = NSButton(title: loc("settings.osc.add"), target: nil, action: nil)
    let oscGrid = NSGridView()
    let oscNoneLabel = NSTextField(labelWithString: loc("settings.osc.noPasscode"))
    /// The No Passcode row's View, Edit and Control boxes.
    var oscNoneRights: [NSButton] = []
    /// One per passcode row, in table order; rebuilt whenever the table is drawn.
    var oscCodeFields: [NSTextField] = []
    var oscViewBoxes: [NSButton] = []
    var oscEditBoxes: [NSButton] = []
    var oscControlBoxes: [NSButton] = []
    var oscDeleteButtons: [NSButton] = []
    /// The table as last drawn: the rows' controls find their passcode here by position.
    var shownOsc: OscAccessTable?
    /// The last passcode edit on its way to the engine; the next waits for it.
    var oscCommits: Task<Void, Never>?

    let networkAddButton = NSButton(title: loc("settings.network.add"), target: nil, action: nil)
    let networkGrid = NSGridView()
    /// The name, host and port fields of every row, in row order.
    var networkFields: [NSTextField] = []
    var networkTransportPopups: [NSPopUpButton] = []
    var networkDeleteButtons: [NSButton] = []
    var shownNetwork: [NetworkPatchRow]?
    var networkCommits: Task<Void, Never>?

    // Key map (`+Keys`): one recorder, clear button and reason line per action.
    var keyFields: [WorkspaceKey: HotkeyRecorder] = [:]
    var keyClearButtons: [WorkspaceKey: NSButton] = [:]
    var keyReasons: [WorkspaceKey: NSTextField] = [:]

    /// The Video page (stages), in its own file.
    let videoPane: VideoSettingsPane

    let messageLabel = NSTextField(wrappingLabelWithString: "")
    let doneButton = DoneButton(title: loc("settings.done"), target: nil, action: nil)

    private var subs: [AnyCancellable] = []
    // Menu-tracking observers: held only while the window is open, removed when it closes.
    private var observers: [NSObjectProtocol] = []
    private var fitQueued = false
    /// How many times the controls were rebuilt (tests: an unchanged status rebuilds nothing).
    private(set) var renderCount = 0
    var lastDrawn: WorkspaceSettingsModel.DisplayState?
    /// A popup's menu is open: rebuilding its items would close it under the operator's hand.
    private var menuOpen = false
    /// A read of the engine's patch resolutions under way (a device came or went).
    private var rereading: Task<Void, Never>?
    private var rereadAgain = false

    init(store: WorkspaceStore) {
        model = WorkspaceSettingsModel(store: store)
        videoPane = VideoSettingsPane(store: store)
        let window = NSWindow(
            // Wide enough by default for a device's whole name in the patch table.
            contentRect: NSRect(x: 0, y: 0, width: 640, height: 540),
            styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.title = loc("settings.title")
        window.identifier = NSUserInterfaceItemIdentifier(AX.settingsWindow)
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.contentView = buildContent()
        window.center()
        // Published before they change: the sinks use the delivered value and render after it.
        subs.append(store.$status.sink { [weak self] _ in self?.renderSoon() })
        subs.append(store.$model.sink { [weak self] _ in self?.renderSoon() })
        // The table is drawn from what the model last read from the engine, whoever asked for it.
        subs.append(model.$patches.sink { [weak self] _ in self?.renderSoon() })
        subs.append(model.$devices.sink { [weak self] _ in self?.renderSoon() })
        // A device lost, back or opened changes where patches resolve; the engine's patch warnings
        // and device states say so, and the table reads the resolutions again.
        subs.append(store.$warnings.removeDuplicates().sink { [weak self] _ in self?.rereadSoon() })
        subs.append(
            store.$status.map { $0?.outputs.map(\.state) }.removeDuplicates().sink { [weak self] _ in
                self?.rereadSoon()
            })
        // Taller than the pages need by the message row's two lines, which never take the pages' room.
        window.contentMinSize = NSSize(width: 520, height: 552)
        window.delegate = self
        tabs.delegate = self
        // The table takes its width only once its tab is laid out, after the lines were wrapped:
        // they are wrapped again then (on the next turn, not inside the layout pass).
        patchGridHolder.onWidthChange = { [weak self] in
            guard let self, !self.fitQueued else { return }
            self.fitQueued = true
            DispatchQueue.main.async { [weak self] in
                MainActor.assumeIsolated {
                    self?.fitQueued = false
                    self?.fitProblemLines()
                }
            }
        }
        render()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    // MARK: Menu tracking

    private func trackMenus() {
        guard observers.isEmpty else { return }
        let center = NotificationCenter.default
        observers.append(
            center.addObserver(forName: NSMenu.didBeginTrackingNotification, object: nil, queue: .main) {
                [weak self] _ in MainActor.assumeIsolated { self?.menuOpen = true }
            })
        observers.append(
            center.addObserver(forName: NSMenu.didEndTrackingNotification, object: nil, queue: .main) {
                [weak self] _ in
                MainActor.assumeIsolated {
                    self?.menuOpen = false
                    self?.renderSoon()
                }
            })
    }

    /// The observers live from showing to closing.
    override func showWindow(_ sender: Any?) {
        trackMenus()
        super.showWindow(sender)
    }

    /// A refusal line is about the tab it came from.
    func tabView(_ tabView: NSTabView, didSelect tabViewItem: NSTabViewItem?) {
        model.message = nil
        model.keyRefusal = nil
        render()
    }

    /// A problem line under a patch wraps at the table's width, which follows the window's.
    func windowDidResize(_ notification: Notification) { fitProblemLines() }

    /// Each problem line wraps at the width its cell gives it as laid out now, and its row is as
    /// tall as the text there: the grid sizes a merged row from a guess that left a blank band
    /// under a two-line warning.
    func fitProblemLines() {
        window?.contentView?.layoutSubtreeIfNeeded()
        var changed = false
        for label in patchProblemLabels where label.frame.width > 100 {
            let width = label.frame.width
            let needs = ceil(
                label.cell?.cellSize(forBounds: NSRect(x: 0, y: 0, width: width, height: 10_000)).height ?? 0)
            guard let row = patchGrid.cell(for: label)?.row else { continue }
            if abs(label.preferredMaxLayoutWidth - width) > 0.5 || row.height != needs {
                label.preferredMaxLayoutWidth = width
                label.invalidateIntrinsicContentSize()
                row.height = needs
                changed = true
            }
        }
        if changed { window?.contentView?.layoutSubtreeIfNeeded() }
    }

    func windowWillClose(_ notification: Notification) {
        model.message = nil
        model.keyRefusal = nil
        messageLabel.stringValue = ""
        observers.forEach(NotificationCenter.default.removeObserver)
        observers = []
        menuOpen = false
    }

    // MARK: Showing

    /// Brings the window up and reads the engine's state. The task's value tells tests it is done.
    @discardableResult
    func present() -> Task<Void, Never> {
        showWindow(nil)
        // No caret in a field: the next Space (GO, in an operator's hands) would replace its text.
        window?.makeFirstResponder(nil)
        return Task {
            await model.refresh()
            await videoPane.refresh()
            render()
        }
    }

    private func renderSoon() {
        DispatchQueue.main.async { [weak self] in MainActor.assumeIsolated { self?.render() } }
    }

    /// Reads devices and resolutions again once (only while the window shows: closed, it reads
    /// them when presented).
    private func rereadSoon() {
        guard window?.isVisible == true else { return }
        // One read at a time; a change during it is read again after it.
        guard rereading == nil else {
            rereadAgain = true
            return
        }
        rereading = Task {
            repeat {
                rereadAgain = false
                await model.refresh()
            } while rereadAgain
            rereading = nil
            render()
        }
    }

    // MARK: Building

    private func buildContent() -> NSView {
        panicField.setAX(AX.settingsPanicField, label: loc("settings.general.panic"))
        goIntervalField.setAX(AX.settingsGoIntervalField, label: loc("settings.general.goInterval"))
        for field in [panicField, goIntervalField] {
            field.alignment = .right
            field.delegate = self
            field.widthAnchor.constraint(equalToConstant: 70).isActive = true
        }

        let general = NSStackView(views: [
            row(loc("settings.general.panic"), panicField, loc("settings.seconds")),
            row(loc("settings.general.goInterval"), goIntervalField, loc("settings.seconds")),
            autoLoadBox(),
        ])
        general.orientation = .vertical
        general.alignment = .leading
        general.spacing = 12

        for (label, view, id) in [
            (loc("settings.tab.general"), general, "general"), (loc("settings.tab.keys"), buildKeysTab(), "keys"),
            (loc("settings.tab.audio"), buildAudioTab(), "audio"),
            (loc("settings.tab.osc"), buildOscTab(), "osc"),
            (loc("settings.tab.network"), buildNetworkTab(), "network"),
            (loc("settings.tab.video"), videoPane.view, "video"),
        ] {
            let item = NSTabViewItem(identifier: id)
            item.label = label
            let holder = NSView()
            view.translatesAutoresizingMaskIntoConstraints = false
            holder.addSubview(view)
            // The tables take the pane's height.
            if id == "osc" || id == "network" || id == "audio" || id == "video" {
                view.bottomAnchor.constraint(equalTo: holder.bottomAnchor, constant: -16).isActive = true
            }
            // The key map's two columns and the patch and stage tables share the pane's width, so a long name wraps or is cut in its own.
            if id == "keys" || id == "audio" || id == "video" {
                view.trailingAnchor.constraint(equalTo: holder.trailingAnchor, constant: -16).isActive = true
            }
            NSLayoutConstraint.activate([
                view.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: 16),
                view.trailingAnchor.constraint(lessThanOrEqualTo: holder.trailingAnchor, constant: -16),
                view.topAnchor.constraint(equalTo: holder.topAnchor, constant: 16),
            ])
            item.view = holder
            tabs.addTabViewItem(item)
        }
        tabs.setAX(AX.settingsTabs)

        messageLabel.setAX(AX.settingsMessage)
        messageLabel.textColor = .systemRed
        doneButton.setAX(AX.settingsDone, label: loc("settings.done"))
        doneButton.keyEquivalent = "\r"
        doneButton.target = self
        doneButton.action = #selector(doneClicked)

        // The line takes two lines at most, cut after that, in a row of fixed height: a long
        // refusal never moves the page above it.
        messageLabel.maximumNumberOfLines = 2
        messageLabel.font = .systemFont(ofSize: 11)
        messageLabel.lineBreakMode = .byWordWrapping
        messageLabel.cell?.wraps = true
        messageLabel.cell?.truncatesLastVisibleLine = true
        let bottom = NSStackView(views: [messageLabel, doneButton])
        bottom.heightAnchor.constraint(equalToConstant: 32).isActive = true
        bottom.orientation = .horizontal
        // The line takes the free width, so Done stays at the right edge with or without a message.
        bottom.distribution = .fill
        bottom.alignment = .centerY
        messageLabel.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        doneButton.setContentHuggingPriority(.required, for: .horizontal)
        messageLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let root = NSStackView(views: [tabs, bottom])
        root.orientation = .vertical
        root.alignment = .width
        root.spacing = 10
        // The tabs take any height the window is given; the Done row keeps its own.
        root.distribution = .fill
        tabs.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        bottom.setContentHuggingPriority(.required, for: .vertical)
        root.edgeInsets = NSEdgeInsets(top: 12, left: 12, bottom: 12, right: 12)
        // Every tab fills the window's width, whatever its page would choose for itself (the Keys
        // page's wrapping labels would otherwise size the tab view to them).
        tabs.widthAnchor.constraint(equalTo: root.widthAnchor, constant: -24).isActive = true
        return root
    }

    private func row(_ title: String?, _ control: NSView, _ suffix: Any?) -> NSStackView {
        var views: [NSView] = title.map { [NSTextField(labelWithString: $0), control] } ?? [control]
        if let suffix = suffix as? String {
            views.append(NSTextField(labelWithString: suffix))
        } else if let suffix = suffix as? NSView {
            views.append(suffix)
        }
        let stack = NSStackView(views: views)
        stack.orientation = .horizontal
        stack.spacing = 8
        return stack
    }

    // MARK: Drawing the model

    /// Redraws every control from the model. Called after each answer and each status change.
    func render() {
        let state = model.displayState
        guard state != lastDrawn, !menuOpen else { return }
        // A cleared `lastDrawn` asks for a full redraw (a refused edit puts the engine's text back).
        let forced = lastDrawn == nil
        lastDrawn = state
        renderCount += 1
        renderOsc(state.osc, forced: forced)
        renderNetwork(state.network, forced: forced)
        renderKeys(state.keys, refusal: state.keyRefusal)
        renderGeneral()
        renderPatches(forced: forced)
        messageLabel.stringValue = model.message ?? ""
    }

    private func renderGeneral() {
        let editing = window?.firstResponder
        let limits = model.limits
        for (field, value) in [
            (panicField, model.panicDuration), (goIntervalField, model.minGoInterval),
            (maxLimitField, model.store.model == nil ? nil : limits.max),
            (minLimitField, model.store.model == nil ? nil : limits.min),
        ] {
            // A field being typed in keeps its text.
            if let editor = editing as? NSTextView, editor.delegate === field { continue }
            field.stringValue = value.map(Self.format) ?? ""
            field.isEnabled = value != nil
        }
        renderAutoLoad()
    }

    static func format(_ seconds: Double) -> String {
        seconds == seconds.rounded() ? String(Int(seconds)) : String(seconds)
    }

    // MARK: Actions

    @objc private func doneClicked() { window?.close() }

    /// Goes with the workspace (and takes its patch editors along, which otherwise outlive them both).
    override func close() {
        for editor in patchEditors.values { editor.close() }
        super.close()
    }

    /// Return in a field commits it and leaves the window open (the Done button's Return key
    /// would otherwise close the window with the typed text never sent).
    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        // Esc drops what was typed (the app has already panicked, which Esc does from any window):
        // the field shows the engine's value again and nothing is sent.
        if selector == #selector(NSResponder.cancelOperation(_:)) {
            control.abortEditing()
            shownPatchTable = nil
            lastDrawn = nil
            render()
            return true
        }
        guard selector == #selector(NSResponder.insertNewline(_:)) else { return false }
        window?.makeFirstResponder(nil)
        return true
    }

    /// A number field was committed (Return or leaving the field): sent when valid, else the
    /// engine's value comes back.
    func controlTextDidEndEditing(_ notification: Notification) {
        guard let field = notification.object as? NSTextField else { return }
        if networkFields.contains(field) {
            networkFieldCommitted(field)
            return
        }
        if oscCodeFields.contains(field) {
            oscCodeCommitted(field)
            return
        }
        if patchNameFields.contains(field) {
            patchNameCommitted(field)
            return
        }
        let text = field.stringValue
        Task {
            if field === panicField {
                await model.setPanicDuration(text)
            } else if field === goIntervalField {
                await model.setMinGoInterval(text)
            } else if field === maxLimitField {
                await model.setOutputMax(text)
            } else if field === minLimitField {
                await model.setOutputMin(text)
            }
            // A refused or unchanged value must not leave the typed text in the field.
            lastDrawn = nil
            render()
        }
    }
}

/// The window's default button. While a field is being typed in, Return belongs to the field
/// (it commits the text); only with no text being edited does Return press Done.
final class DoneButton: NSButton {
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if window?.firstResponder is NSText { return false }
        return super.performKeyEquivalent(with: event)
    }
}

/// A flipped view that says when its width changes (the patch table's holder: its problem lines
/// wrap at that width).
@MainActor
final class WidthWatchingView: NSView {
    var onWidthChange: (() -> Void)?
    override var isFlipped: Bool { true }

    override func setFrameSize(_ newSize: NSSize) {
        let before = frame.width
        super.setFrameSize(newSize)
        if frame.width != before { onWidthChange?() }
    }
}
