// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The output route editor (VID-01; QLab 5 manual p. 233, UI map `window.video-route-editor.demo`),
/// a dialog window of its own run modally over the app (not a sheet on the settings window),
/// opened by a route row's Edit: its name, its size with a button that takes the size of the chosen
/// display, its rotation (a quarter turn from upright or back swaps the width and height typed) and
/// scaling (with a line saying what the scaling does), then the display it is shown on on this
/// machine and that display's size, read only. OK sends what changed of the route as one undo step
/// and the display as this machine's choice (no undo step); Cancel and Esc send nothing. A value the
/// route cannot have is refused in the dialog, which stays up saying why.
@MainActor
final class RouteEditorController: NSWindowController, NSTextFieldDelegate {
    let store: WorkspaceStore
    let routeID: ModelID
    let nameField = NSTextField()
    let widthField = NSTextField()
    let heightField = NSTextField()
    let fromDisplayButton = NSButton(title: loc("routeEditor.fromDisplay"), target: nil, action: nil)
    let rotationPopup = NSPopUpButton(frame: .zero, pullsDown: false)
    let scalingPopup = NSPopUpButton(frame: .zero, pullsDown: false)
    /// What the chosen scaling does, in a sentence.
    let scalingNote = NSTextField(wrappingLabelWithString: "")
    let displayPopup = NSPopUpButton(frame: .zero, pullsDown: false)
    let displaySizeLabel = NSTextField(labelWithString: "")
    let messageLabel = NSTextField(wrappingLabelWithString: "")
    let cancelButton = NSButton(title: loc("routeEditor.cancel"), target: nil, action: nil)
    let okButton = NSButton(title: loc("routeEditor.ok"), target: nil, action: nil)
    /// The displays this Mac has, as the settings page last read them.
    private let displays: [StageDisplay]
    /// The route as it was when the sheet opened, and its display here.
    private let original: (name: String, width: Int, height: Int, rotation: Int, scaling: String)
    private let originalDisplay: String?
    /// The part of that display it is on (a splitter's output): kept while the display is, and its
    /// size is the device's size for Set from Device.
    private let originalPartial: DisplayPart?
    /// Called when the sheet is done (OK accepted or Cancel), to take it down.
    var onDone: (() -> Void)?
    /// The edit on its way (tests wait for it).
    private(set) var saving: Task<Void, Never>?

    static let rotations = [0, 90, 180, 270]
    /// The sheet's width (its height is what its rows need).
    static let width: CGFloat = 520
    /// A route is 1 to 30000 pixels a side (QLab 5 manual p. 233).
    static let sides = 1...30000

    init?(store: WorkspaceStore, route: ModelID) {
        guard let raw = store.model?.routes.first(where: { $0.id == route })?.raw else { return nil }
        self.store = store
        routeID = route
        displays = store.videoOutputs.displays
        let entry = store.videoOutputs.route(route)
        originalDisplay = entry?.mapping?.display
        originalPartial = entry?.mapping?.partial
        original = (
            raw["name"]?.stringValue ?? "", Int(raw["width"]?.doubleValue ?? 0), Int(raw["height"]?.doubleValue ?? 0),
            Int(raw["rotation"]?.doubleValue ?? 0), raw["scaling"]?.stringValue ?? "fill"
        )
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 460, height: 300), styleMask: [.titled], backing: .buffered,
            defer: false)
        window.title = loc("routeEditor.title")
        window.identifier = NSUserInterfaceItemIdentifier("route-editor")
        window.isReleasedWhenClosed = false
        super.init(window: window)
        let content = build(mappingName: entry?.mapping?.name)
        window.contentView = content
        window.defaultButtonCell = okButton.cell as? NSButtonCell
        fill()
        content.layoutSubtreeIfNeeded()
        window.setContentSize(content.fittingSize)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    private func label(_ key: String) -> NSTextField {
        let l = NSTextField(labelWithString: loc(key))
        l.alignment = .right
        return l
    }

    private func build(mappingName: String?) -> NSView {
        nameField.setAX("route-editor-name", label: loc("routeEditor.name"))
        widthField.setAX("route-editor-width", label: loc("settings.video.width"))
        heightField.setAX("route-editor-height", label: loc("settings.video.height"))
        for f in [nameField, widthField, heightField] { f.delegate = self }
        for f in [widthField, heightField] {
            f.alignment = .right
            f.font = .monospacedDigitSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
            f.widthAnchor.constraint(equalToConstant: 64).isActive = true
        }
        nameField.widthAnchor.constraint(greaterThanOrEqualToConstant: 240).isActive = true
        fromDisplayButton.setAX("route-editor-from-display")
        fromDisplayButton.bezelStyle = .rounded
        fromDisplayButton.target = self
        fromDisplayButton.action = #selector(fromDisplayClicked)
        fromDisplayButton.toolTip = loc("routeEditor.fromDisplay.tip")
        rotationPopup.setAX("route-editor-rotation", label: loc("routeEditor.rotation"))
        for r in Self.rotations {
            rotationPopup.addItem(withTitle: String(format: loc("routeEditor.degrees"), r))
            rotationPopup.lastItem?.representedObject = r
        }
        rotationPopup.target = self
        rotationPopup.action = #selector(rotationChosen)
        scalingPopup.setAX("route-editor-scaling", label: loc("routeEditor.scaling"))
        scalingPopup.target = self
        scalingPopup.action = #selector(scalingChosen)
        scalingNote.setAX("route-editor-scaling-note")
        scalingNote.textColor = .secondaryLabelColor
        scalingNote.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        scalingNote.preferredMaxLayoutWidth = 260
        displayPopup.setAX("route-editor-display", label: loc("routeEditor.display"))
        displayPopup.target = self
        displayPopup.action = #selector(displayChosen)
        VideoSettingsPane.fillDisplays(
            displayPopup, displays: displays, chosen: originalDisplay, chosenName: mappingName)
        displaySizeLabel.setAX("route-editor-display-size")
        messageLabel.setAX("route-editor-message")
        messageLabel.textColor = .systemRed
        messageLabel.preferredMaxLayoutWidth = 420
        cancelButton.setAX("route-editor-cancel")
        cancelButton.bezelStyle = .rounded
        cancelButton.keyEquivalent = "\u{1b}"
        cancelButton.target = self
        cancelButton.action = #selector(cancelClicked)
        okButton.setAX("route-editor-ok")
        okButton.bezelStyle = .rounded
        okButton.keyEquivalent = "\r"
        okButton.target = self
        okButton.action = #selector(okClicked)

        let by = NSTextField(labelWithString: "×")
        by.textColor = .secondaryLabelColor
        let size = NSStackView(views: [widthField, by, heightField, fromDisplayButton])
        size.spacing = 6
        size.setCustomSpacing(16, after: heightField)
        let rotation = NSStackView(views: [rotationPopup])
        let grid = NSGridView(views: [
            [label("routeEditor.name"), nameField],
            [label("routeEditor.size"), size],
            [label("routeEditor.rotation"), rotation],
            [label("routeEditor.scaling"), scalingPopup],
            [NSGridCell.emptyContentView, scalingNote],
        ])
        grid.column(at: 0).xPlacement = .trailing
        grid.rowAlignment = .firstBaseline
        grid.rowSpacing = 10
        grid.columnSpacing = 8
        let lower = NSGridView(views: [
            [label("routeEditor.display"), displayPopup],
            [label("routeEditor.displaySize"), displaySizeLabel],
        ])
        lower.column(at: 0).xPlacement = .trailing
        lower.rowAlignment = .firstBaseline
        lower.rowSpacing = 10
        lower.columnSpacing = 8
        let divider = NSBox()
        divider.boxType = .separator
        let buttons = NSStackView(views: [messageLabel, cancelButton, okButton])
        buttons.spacing = 10
        buttons.distribution = .fill
        messageLabel.setContentHuggingPriority(.defaultLow, for: .horizontal)
        messageLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let stack = NSStackView(views: [grid, divider, lower, buttons])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        stack.edgeInsets = NSEdgeInsets(top: 20, left: 20, bottom: 16, right: 20)
        for v in [divider, buttons] {
            v.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -40).isActive = true
        }
        stack.widthAnchor.constraint(equalToConstant: Self.width).isActive = true
        // The two grids' label columns line up.
        grid.column(at: 0).width = 130
        lower.column(at: 0).width = 130
        return stack
    }

    /// The fields as the route is now.
    private func fill() {
        nameField.stringValue = original.name
        widthField.stringValue = String(original.width)
        heightField.stringValue = String(original.height)
        rotationPopup.selectItem(at: Self.rotations.firstIndex(of: original.rotation) ?? 0)
        shownRotation = original.rotation
        VideoSettingsPane.fillScalings(scalingPopup, chosen: original.scaling)
        scalingChosen()
        displayChosen()
    }

    /// The display chosen in the sheet, if this Mac has it now.
    private var chosenDisplay: StageDisplay? {
        let id = displayPopup.selectedItem?.representedObject as? String
        return displays.first { $0.id == id }
    }

    /// The rotation the size fields were last drawn for.
    private var shownRotation = 0

    /// A quarter turn from upright (or back) swaps the width and height typed, so the route keeps
    /// the shape of what it is shown on turned (as QLab 5 does); a half turn changes nothing.
    @objc private func rotationChosen() {
        let rotation = rotationPopup.selectedItem?.representedObject as? Int ?? 0
        if (rotation % 180 == 0) != (shownRotation % 180 == 0) {
            let w = widthField.stringValue
            widthField.stringValue = heightField.stringValue
            heightField.stringValue = w
        }
        shownRotation = rotation
    }

    @objc private func scalingChosen() {
        let scaling = scalingPopup.selectedItem?.representedObject as? String ?? "fill"
        scalingNote.stringValue = loc("routeEditor.scaling.note.\(scaling)")
    }

    /// The size of what the route is shown on: the chosen display's, or its part's when the display is
    /// the one the route is on, as laid out on the page (QLab 5 manual p. 234).
    private var chosenSize: (width: UInt32, height: UInt32)? {
        guard let d = chosenDisplay else { return nil }
        if d.id == originalDisplay, let part = originalPartial { return part.size(ofWidth: d.width, height: d.height) }
        return (d.width, d.height)
    }

    @objc private func displayChosen() {
        let size = chosenSize
        // With no display chosen the size read is the route's own (as QLab 5 shows it); never typed in.
        displaySizeLabel.stringValue = String(
            format: loc("settings.video.resolution"), String(size?.width ?? UInt32(max(0, original.width))),
            String(size?.height ?? UInt32(max(0, original.height))))
        fromDisplayButton.isEnabled = size != nil
    }

    /// Takes the route's size from the chosen display (or its part), QLab 5 manual pp. 233-234.
    @objc private func fromDisplayClicked() {
        guard let size = chosenSize else { return }
        widthField.stringValue = String(size.width)
        heightField.stringValue = String(size.height)
    }

    /// Runs the dialog modally, centred over `parent` (the stage standing back meanwhile, as for any
    /// question); returns once OK or Cancel took it down (`onDone` stops the modal run).
    func run(over parent: NSWindow?) {
        guard let window else { return }
        if let p = parent?.frame {
            window.setFrameTopLeftPoint(NSPoint(x: p.midX - window.frame.width / 2, y: p.maxY - 80))
        } else {
            window.center()
        }
        _ = StandBack.runModal(window)
        window.orderOut(nil)
    }

    @objc private func cancelClicked() { onDone?() }

    /// Sends what changed: the route's properties as one undo step, then the display on this
    /// machine. A value the route cannot have, or the engine's refusal, keeps the sheet up saying
    /// why.
    @objc private func okClicked() {
        window?.makeFirstResponder(nil)
        let name = nameField.stringValue
        guard !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            messageLabel.stringValue = loc("settings.video.badRouteName")
            return
        }
        guard let width = Int(widthField.stringValue.trimmingCharacters(in: .whitespaces)),
            let height = Int(heightField.stringValue.trimmingCharacters(in: .whitespaces)),
            Self.sides.contains(width), Self.sides.contains(height)
        else {
            messageLabel.stringValue = loc("routeEditor.badSize")
            return
        }
        let rotation = rotationPopup.selectedItem?.representedObject as? Int ?? 0
        let scaling = scalingPopup.selectedItem?.representedObject as? String ?? "fill"
        var changes: [Command] = []
        if name != original.name {
            changes.append(.setRoute(routeID, .object(["type": .string("name"), "value": .string(name)])))
        }
        if (width, height) != (original.width, original.height) {
            changes.append(
                .setRoute(
                    routeID,
                    .object([
                        "type": .string("size"), "width": .int(Int64(width)), "height": .int(Int64(height)),
                    ])))
        }
        if rotation != original.rotation {
            changes.append(.setRoute(routeID, .object(["type": .string("rotation"), "value": .int(Int64(rotation))])))
        }
        if scaling != original.scaling {
            changes.append(.setRoute(routeID, .object(["type": .string("scaling"), "value": .string(scaling)])))
        }
        let display = displayPopup.selectedItem?.representedObject as? String
        let route = routeID
        let original = originalDisplay
        saving = Task { [weak self, store] in
            if let command = Command.transaction(changes), case .failure(let e) = await store.send(command: command) {
                self?.refused(e)
                return
            }
            if display != original, case .failure(let e) = await store.setRouteDisplay(route, display: display) {
                self?.refused(e)
                return
            }
            self?.onDone?()
        }
    }

    /// Shows why OK was refused: a route removed meanwhile (by another client or an undo) is
    /// named as gone, never by its id.
    private func refused(_ error: EngineError) {
        let gone = store.model?.routes.contains { $0.id == routeID } == false
        messageLabel.stringValue = gone ? loc("settings.video.routeGone") : error.detailText
    }

    /// Return in a field is OK, as in any sheet.
    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        if selector == #selector(NSResponder.insertNewline(_:)) {
            okClicked()
            return true
        }
        return false
    }
}
