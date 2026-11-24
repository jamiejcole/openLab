// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The output route editor (VID-01; QLab 5 manual p. 233, UI map `window.video-route-editor`), a
/// sheet on the workspace settings window opened by a route row's Edit: its name, its size with a
/// button that takes the size of the chosen display, its rotation and scaling (with a line saying
/// what the scaling does), then the display it is shown on on this machine and that display's
/// size. OK sends what changed of the route as one undo step and the display as this machine's
/// choice (no undo step); Cancel sends nothing. A value the route cannot have is refused in the
/// sheet, which stays up saying why.
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
    /// Called when the sheet is done (OK accepted or Cancel), to take it down.
    var onDone: (() -> Void)?
    /// The edit on its way (tests wait for it).
    private(set) var saving: Task<Void, Never>?

    static let rotations = [0, 90, 180, 270]
    /// A route is 1 to 30000 pixels a side (QLab 5 manual p. 233).
    static let sides = 1...30000

    init?(store: WorkspaceStore, route: ModelID) {
        guard let raw = store.model?.routes.first(where: { $0.id == route })?.raw else { return nil }
        self.store = store
        routeID = route
        displays = store.videoOutputs.displays
        let entry = store.videoOutputs.route(route)
        originalDisplay = entry?.mapping?.display
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
        window.contentView = build(mappingName: entry?.mapping?.name)
        fill()
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
        VideoSettingsPane.fillDisplays(displayPopup, displays: displays, chosen: originalDisplay, chosenName: mappingName)
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
        messageLabel.setContentHuggingPriority(.defaultLow, for: .horizontal)
        let stack = NSStackView(views: [grid, divider, lower, buttons])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        stack.edgeInsets = NSEdgeInsets(top: 20, left: 20, bottom: 16, right: 20)
        divider.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -40).isActive = true
        buttons.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -40).isActive = true
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
        VideoSettingsPane.fillScalings(scalingPopup, chosen: original.scaling)
        scalingChosen()
        displayChosen()
    }

    /// The display chosen in the sheet, if this Mac has it now.
    private var chosenDisplay: StageDisplay? {
        let id = displayPopup.selectedItem?.representedObject as? String
        return displays.first { $0.id == id }
    }

    @objc private func scalingChosen() {
        let scaling = scalingPopup.selectedItem?.representedObject as? String ?? "fill"
        scalingNote.stringValue = loc("routeEditor.scaling.note.\(scaling)")
    }

    @objc private func displayChosen() {
        let d = chosenDisplay
        displaySizeLabel.stringValue =
            d.map { String(format: loc("settings.video.resolution"), String($0.width), String($0.height)) }
            ?? loc("routeEditor.noDisplaySize")
        fromDisplayButton.isEnabled = d != nil
    }

    /// Takes the route's size from the chosen display (QLab 5 manual p. 233).
    @objc private func fromDisplayClicked() {
        guard let d = chosenDisplay else { return }
        widthField.stringValue = String(d.width)
        heightField.stringValue = String(d.height)
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
                self?.messageLabel.stringValue = e.detailText
                return
            }
            if display != original, case .failure(let e) = await store.setRouteDisplay(route, display: display) {
                self?.messageLabel.stringValue = e.detailText
                return
            }
            self?.onDone?()
        }
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
