// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// A standard alert window (`NSAlert`: the app icon, a bold message, informative text, buttons in
/// the system's order) shown without running it modally. It floats above every window, never
/// becomes key on its own and blocks nothing, so GO, Esc and Close keep reaching the workspace
/// windows while it is up. The buttons run their own actions; the alert's modal machinery is not
/// used.
@MainActor
final class FloatingAlert: NSObject {
    struct Button {
        let title: String
        let id: String?
        let action: () -> Void
    }

    let alert = NSAlert()
    private let buttons: [Button]
    private var accessory: NSTextField?

    /// The alert's window (an `NSPanel`).
    var window: NSWindow { alert.window }

    /// `buttons` run first to last as the system lays them out: the first is the default, on the right.
    init(
        critical: Bool, title: String, message: String, detail: String = "", buttons: [Button],
        messageIDs: (title: String, message: String, detail: String)? = nil
    ) {
        self.buttons = buttons
        super.init()
        alert.alertStyle = critical ? .critical : .warning
        alert.messageText = title
        alert.informativeText = message
        if !detail.isEmpty {
            let label = NSTextField(wrappingLabelWithString: detail)
            label.font = .systemFont(ofSize: 11)
            label.textColor = .secondaryLabelColor
            label.preferredMaxLayoutWidth = 240
            label.isSelectable = false
            label.frame = NSRect(origin: .zero, size: label.fittingSize)
            alert.accessoryView = label
            accessory = label
        }
        for button in buttons { alert.addButton(withTitle: button.title) }
        // The alert is never run, so its buttons act through ours rather than ending a modal session.
        for (index, control) in alert.buttons.enumerated() {
            control.tag = index
            control.target = self
            control.action = #selector(clicked(_:))
            if let id = buttons[index].id { control.setAX(id) }
        }
        alert.layout()
        if let ids = messageIDs {
            for case let field as NSTextField in Self.walk(window.contentView) {
                if field.stringValue == title {
                    field.setAX(ids.title)
                } else if field.stringValue == message {
                    field.setAX(ids.message)
                }
            }
            accessory?.setAX(ids.detail)
        }
        // Selectable text needs its panel to be key: a click on the message would take the keys
        // from the workspace window.
        for case let field as NSTextField in Self.walk(window.contentView) { field.isSelectable = false }
        let panel = window as? NSPanel
        panel?.isFloatingPanel = true
        panel?.becomesKeyOnlyIfNeeded = true
        window.level = .floating
        window.hidesOnDeactivate = false
        window.isReleasedWhenClosed = false
        window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
    }

    private static func walk(_ view: NSView?) -> [NSView] {
        guard let view else { return [] }
        return [view] + view.subviews.flatMap(walk)
    }

    @objc private func clicked(_ sender: NSButton) {
        guard sender.tag >= 0, sender.tag < buttons.count else { return }
        buttons[sender.tag].action()
    }
}
