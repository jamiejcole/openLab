// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The Override Controls panel (PB-10): one small floating panel for every workspace, with the
/// network output switch (on: Network cues send). Only the network output exists, so it is the only
/// row. The switch shows the engine's own state, never a state the app holds: a click sends the
/// request and the next `status` redraws the box. The panel never becomes key on its own, so GO and
/// Esc keep working in the workspace window behind it, and it is never modal.
@MainActor
final class OutputOverridesPanel: NSObject {
    private(set) var panel: NSPanel?
    /// The switch: checked means network output is on.
    let networkSwitch = NSButton(checkboxWithTitle: loc("overrides.network"), target: nil, action: nil)
    private let note = NSTextField(wrappingLabelWithString: loc("overrides.note"))
    /// The user flipped the switch: the new `network_disabled` value.
    var onChange: ((Bool) -> Void)?

    var isVisible: Bool { panel?.isVisible == true }

    override init() {
        super.init()
        networkSwitch.target = self
        networkSwitch.action = #selector(switched)
        networkSwitch.setAX(AX.overrideNetwork, label: loc("overrides.network"))
        networkSwitch.state = .on
        note.font = .systemFont(ofSize: 11)
        note.textColor = .secondaryLabelColor
        note.setAX(AX.overrideNote)
    }

    func show() {
        let panel = self.panel ?? makePanel()
        panel.orderFrontRegardless()
    }

    /// Draws the engine's state. nil: no workspace is open, so the engine's state cannot be seen
    /// and the switch is not offered.
    func sync(networkDisabled: Bool?) {
        networkSwitch.isEnabled = networkDisabled != nil
        networkSwitch.state = networkDisabled == true ? .off : .on
        note.stringValue = loc(networkDisabled == nil ? "overrides.noWorkspace" : "overrides.note")
    }

    @objc private func switched() { onChange?(networkSwitch.state == .off) }

    private func makePanel() -> NSPanel {
        let panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 280, height: 90),
            styleMask: [.titled, .closable, .utilityWindow, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.title = loc("overrides.window")
        panel.identifier = NSUserInterfaceItemIdentifier(AX.overridePanel)
        panel.level = .floating
        panel.isFloatingPanel = true
        panel.becomesKeyOnlyIfNeeded = true
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        let column = NSStackView(views: [networkSwitch, note])
        column.orientation = .vertical
        column.alignment = .leading
        column.spacing = 8
        column.edgeInsets = NSEdgeInsets(top: 14, left: 16, bottom: 14, right: 16)
        panel.contentView = column
        panel.center()
        self.panel = panel
        return panel
    }
}
