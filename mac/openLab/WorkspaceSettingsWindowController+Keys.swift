// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The Keys tab of the workspace settings (TRG-01, TRG-07): the key map as QLab 5 lays it out
/// (Controls, Keyboard): the workspace actions in one column, the cue-editing actions in the
/// other. Each action has a field that takes the next key pressed in it, a button that takes the
/// key away, and a line beside them saying why a key was not taken. Esc is shown, fixed.
extension WorkspaceSettingsWindowController {
    func buildKeysTab() -> NSView {
        func column(_ title: String, _ keys: [WorkspaceKey], footer: [NSView]) -> NSStackView {
            let heading = NSTextField(labelWithString: title)
            heading.font = .systemFont(ofSize: 13, weight: .semibold)
            let stack = NSStackView(views: [heading] + keys.map(keyRow) + footer)
            stack.orientation = .vertical
            stack.alignment = .leading
            stack.spacing = 6
            return stack
        }
        let escape = [("settings.keys.escOnce", "settings.keys.panicAll"), ("settings.keys.escTwice", "settings.keys.stopAll")]
            .enumerated().map { index, pair -> NSView in
                let key = NSTextField(labelWithString: loc(pair.0))
                key.alignment = .center
                key.widthAnchor.constraint(equalToConstant: Self.keyFieldWidth + 24).isActive = true
                let action = NSTextField(labelWithString: loc(pair.1))
                for label in [key, action] { label.textColor = .secondaryLabelColor }
                let row = NSStackView(views: [key, action])
                row.spacing = 6
                row.setAX("\(AX.settingsKeys)-escape-\(index + 1)", label: "\(loc(pair.0)), \(loc(pair.1))")
                return row
            }
        let workspace = column(
            loc("settings.keys.workspace"), WorkspaceKey.workspaceActions, footer: escape)
        let editing = column(loc("settings.keys.editing"), WorkspaceKey.editingActions, footer: [])
        let columns = NSStackView(views: [workspace, editing])
        columns.orientation = .horizontal
        columns.alignment = .top
        columns.distribution = .fillEqually
        columns.spacing = 16
        columns.setAX(AX.settingsKeys)
        return columns
    }

    static let keyFieldWidth: CGFloat = 78

    private func keyRow(_ key: WorkspaceKey) -> NSView {
        let title = loc(key.titleKey)
        let field = HotkeyRecorder()
        field.isEditable = false
        field.isSelectable = false
        field.alignment = .center
        field.idlePlaceholder = loc("settings.keys.none")
        field.recordingPlaceholder = loc("settings.keys.press")
        field.placeholderString = field.idlePlaceholder
        field.setAX("\(AX.settingsKeys)-\(key.rawValue)", label: title)
        field.widthAnchor.constraint(equalToConstant: Self.keyFieldWidth).isActive = true
        field.onRecord = { [weak self] event in self?.keyTyped(event, for: key) }
        keyFields[key] = field

        let clear = NSButton(title: "", target: self, action: #selector(keyClearClicked(_:)))
        clear.image = NSImage(systemSymbolName: "xmark.circle", accessibilityDescription: loc("settings.keys.clear"))
        clear.isBordered = false
        clear.imagePosition = .imageOnly
        clear.identifier = NSUserInterfaceItemIdentifier(key.rawValue)
        clear.setAX("\(AX.settingsKeys)-clear-\(key.rawValue)", label: String(format: loc("settings.keys.clearKey"), title))
        clear.setContentHuggingPriority(.required, for: .horizontal)
        keyClearButtons[key] = clear

        let name = NSTextField(labelWithString: title)
        name.lineBreakMode = .byTruncatingTail
        name.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let reason = NSTextField(wrappingLabelWithString: "")
        reason.font = .systemFont(ofSize: 11)
        reason.textColor = .systemRed
        reason.isHidden = true
        reason.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        reason.setAX("\(AX.settingsKeys)-reason-\(key.rawValue)")
        keyReasons[key] = reason
        let words = NSStackView(views: [name, reason])
        words.orientation = .vertical
        words.alignment = .leading
        words.spacing = 1
        words.setHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)

        let row = NSStackView(views: [field, clear, words])
        row.orientation = .horizontal
        row.alignment = .firstBaseline
        row.spacing = 6
        return row
    }

    /// Draws each action's key (an empty field shows "none"), the clear buttons and the one reason
    /// line of the last refused key. A field waiting for a key keeps its prompt.
    func renderKeys(_ keys: [WorkspaceKey: KeyChord], refusal: KeyRefusal?) {
        let enabled = model.store.model != nil
        for (key, field) in keyFields {
            if window?.firstResponder !== field { field.stringValue = keys[key]?.display ?? "" }
            field.isEnabled = enabled
            keyClearButtons[key]?.isEnabled = enabled && keys[key] != nil
            let reason = keyReasons[key]
            reason?.stringValue = refusal?.key == key ? refusal?.text ?? "" : ""
            reason?.isHidden = refusal?.key != key
        }
    }

    private func keyTyped(_ event: NSEvent, for key: WorkspaceKey) {
        guard let chord = KeyChord(event: event) else {
            model.keyRefusal = KeyRefusal(key: key, text: loc("settings.keys.refused.unsupported"))
            render()
            return
        }
        let menu = menuChords()
        Task {
            await model.assignKey(key, chord, menuChords: menu)
            lastDrawn = nil
            render()
        }
    }

    @objc private func keyClearClicked(_ sender: NSButton) {
        guard let raw = sender.identifier?.rawValue, let key = WorkspaceKey(rawValue: raw) else { return }
        Task {
            await model.clearKey(key)
            lastDrawn = nil
            render()
        }
    }
}
