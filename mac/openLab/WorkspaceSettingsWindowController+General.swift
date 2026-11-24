// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The General page of the workspace settings (QLab 5, manual pp. 99-101): the time between GOs and
/// the key-up rule, the panic fade, auto-numbering, auto-load, the playhead lock and copying media
/// into the show folder as files are added. Each box sends
/// its setting to the engine and the page is drawn from what the engine then holds. The only
/// descriptive note is the one under the copy-media box.
extension WorkspaceSettingsWindowController {
    func buildGeneralTab() -> NSView {
        panicField.setAX(AX.settingsPanicField, label: loc("settings.general.panic"))
        goIntervalField.setAX(AX.settingsGoIntervalField, label: loc("settings.general.goInterval"))
        autoNumberField.setAX("settings-auto-number-step", label: loc("settings.general.autoNumber"))
        autosaveField.setAX("settings-autosave-interval", label: loc("settings.general.autosave"))
        for field in [panicField, goIntervalField, autoNumberField, autosaveField] {
            field.alignment = .right
            field.delegate = self
            field.widthAnchor.constraint(equalToConstant: 70).isActive = true
        }
        let boxes: [(NSButton, String, Selector)] = [
            (keyUpCheck, "settings-key-up", #selector(switchClicked(_:))),
            (autoNumberCheck, "settings-auto-number", #selector(switchClicked(_:))),
            (autoLoadCheck, "settings-new-cues-auto-load", #selector(autoLoadClicked)),
            (lockPlayheadCheck, "settings-lock-playhead", #selector(switchClicked(_:))),
            (copyMediaCheck, AX.settingsCopyMedia, #selector(switchClicked(_:))),
        ]
        for (box, id, action) in boxes {
            box.setAX(id)
            box.target = self
            box.action = action
        }
        // Labels down the left, boxes in a column, units beside them, the one note in its own
        // column on the right; a rule between groups of settings.
        let label = { (key: String) in NSTextField(labelWithString: loc(key)) }
        let unit = { NSTextField(labelWithString: loc("settings.seconds")) }
        let grid = NSGridView()
        grid.columnSpacing = 10
        grid.rowSpacing = 10
        var rules: [NSGridRow] = []
        func rule() {
            let line = NSBox()
            line.boxType = .separator
            let row = grid.addRow(with: [line])
            row.topPadding = 4
            row.bottomPadding = 4
            rules.append(row)
        }
        grid.addRow(with: [label("settings.general.goInterval"), goIntervalField, unit()])
        grid.addRow(with: [keyUpCheck])
        rule()
        grid.addRow(with: [label("settings.general.panic"), panicField, unit()])
        rule()
        grid.addRow(with: [label("settings.general.autosave"), autosaveField, unit()])
        rule()
        grid.addRow(with: [autoNumberCheck, autoNumberField])
        grid.addRow(with: [autoLoadCheck])
        rule()
        grid.addRow(with: [lockPlayheadCheck])
        grid.addRow(with: [copyMediaCheck])
        // The note runs under its box, the width of the page (a column beside it would not fit the
        // smallest window).
        let noteRow = grid.addRow(with: [note("settings.general.copyMedia.note")])
        // A rule and the note run the whole width: their cells are merged once every column exists.
        for row in rules + [noteRow] { row.mergeCells(in: NSRange(location: 0, length: grid.numberOfColumns)) }
        return grid
    }

    private func note(_ key: String) -> NSTextField {
        let label = NSTextField(wrappingLabelWithString: loc(key))
        label.font = .systemFont(ofSize: 11)
        label.textColor = .secondaryLabelColor
        label.preferredMaxLayoutWidth = 340
        return label
    }

    /// The switch a box stands for.
    private func switchOf(_ box: NSButton) -> WorkspaceSwitch? {
        switch box {
        case keyUpCheck: return .goNeedsKeyUp
        case autoNumberCheck: return .autoNumberNewCues
        case lockPlayheadCheck: return .lockPlayhead
        case copyMediaCheck: return .copyMediaOnAdd
        default: return nil
        }
    }

    func renderSwitches() {
        for (box, item) in [
            (keyUpCheck, WorkspaceSwitch.goNeedsKeyUp), (autoNumberCheck, .autoNumberNewCues),
            (lockPlayheadCheck, .lockPlayhead),
        ] {
            let on = model.isOn(item)
            box.state = on == true ? .on : .off
            box.isEnabled = on != nil
        }
        autoNumberField.isEnabled = model.isOn(.autoNumberNewCues) == true
        let copy = model.isOn(.copyMediaOnAdd)
        copyMediaCheck.state = copy == true ? .on : .off
        // Locked in Show Mode, as the rest of the show's editing is.
        copyMediaCheck.isEnabled = copy != nil && !model.store.showMode
        renderAutoLoad()
    }

    func renderAutoLoad() {
        let on = model.newCuesAutoLoad
        autoLoadCheck.state = on == true ? .on : .off
        autoLoadCheck.isEnabled = on != nil
    }

    @objc private func switchClicked(_ sender: NSButton) {
        guard let item = switchOf(sender) else { return }
        let on = sender.state == .on
        Task { [weak self] in
            await self?.model.setSwitch(item, on)
            self?.lastDrawn = nil
            self?.render()
        }
    }

    @objc private func autoLoadClicked() {
        let on = autoLoadCheck.state == .on
        Task { [weak self] in
            await self?.model.setNewCuesAutoLoad(on)
            self?.lastDrawn = nil
            self?.render()
        }
    }
}

extension WorkspaceSettingsModel {
    /// nil until the show is in.
    var newCuesAutoLoad: Bool? {
        guard let settings = store.model?.settings else { return nil }
        return settings["new_cues_auto_load"] == .bool(true)
    }

    func setNewCuesAutoLoad(_ on: Bool) async {
        guard newCuesAutoLoad != on else { return }
        switch await store.send(command: .setNewCuesAutoLoad(on)) {
        case .success: message = nil
        case .failure(let e): message = e.detailText
        }
    }

    /// The switch as the show has it; nil until the show is in.
    func isOn(_ item: WorkspaceSwitch) -> Bool? { store.isOn(item) }

    func setSwitch(_ item: WorkspaceSwitch, _ on: Bool) async {
        guard isOn(item) != on else { return }
        switch await store.send(command: .setSwitch(item, on)) {
        case .success: message = nil
        case .failure(let e): message = e.detailText
        }
    }

    static let autosaveRange = 5.0...3600.0

    var autosaveInterval: Double? { store.model?.settings["autosave_interval"]?.doubleValue }

    @discardableResult
    func setAutosaveInterval(_ text: String) async -> Bool {
        await setNumber("autosave_interval", text, Self.autosaveRange)
    }

    static let autoNumberStepRange = 0.001...1_000_000.0

    var autoNumberStep: Double? { store.model?.settings["auto_number_increment"]?.doubleValue }

    @discardableResult
    func setAutoNumberStep(_ text: String) async -> Bool {
        await setNumber("auto_number_increment", text, Self.autoNumberStepRange)
    }
}
