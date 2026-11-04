// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// A panel that never takes the keys: Space, Return and every hotkey go on to the workspace window.
final class NonKeyPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

/// The notice for typed notes that were not saved: the cue, the reason, OK and Copy Text. A small
/// floating panel that belongs to no window, never becomes key and blocks nothing, so Close, Quit,
/// Show Mode and GO go on while it is up. It stays when its workspace closes, so Copy Text still works,
/// and goes when the app quits. A second notice opens a second panel, stepped down from the last.
@MainActor
final class NotesNoticePanel: NSObject {
    /// The panels showing now.
    private(set) static var open: [NotesNoticePanel] = []

    let panel: NSPanel
    private let notice: NotesNotice
    private let pasteboard: NSPasteboard

    static func show(_ notice: NotesNotice, near window: NSWindow?, pasteboard: NSPasteboard) {
        let shown = NotesNoticePanel(notice, pasteboard: pasteboard)
        let step = CGFloat(open.count) * 28
        if let frame = window?.frame {
            let size = shown.panel.frame.size
            shown.panel.setFrameOrigin(
                NSPoint(x: frame.midX - size.width / 2 + step, y: frame.maxY - size.height - 60 - step))
        } else {
            shown.panel.center()
        }
        open.append(shown)
        shown.panel.orderFrontRegardless()
    }

    static func closeAll() {
        for shown in open { shown.panel.close() }
        open = []
    }

    private init(_ notice: NotesNotice, pasteboard: NSPasteboard) {
        self.notice = notice
        self.pasteboard = pasteboard
        panel = NonKeyPanel(
            contentRect: NSRect(x: 0, y: 0, width: 380, height: 120),
            styleMask: [.titled, .closable, .utilityWindow, .nonactivatingPanel], backing: .buffered, defer: false)
        super.init()
        panel.title = loc("notes.notice.title")
        panel.level = .floating
        panel.isFloatingPanel = true
        panel.becomesKeyOnlyIfNeeded = true
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        let message = NSTextField(wrappingLabelWithString: notice.message)
        message.preferredMaxLayoutWidth = 348
        let ok = NSButton(title: loc("alert.ok"), target: self, action: #selector(dismiss))
        let copy = NSButton(title: loc("alert.copyText"), target: self, action: #selector(copyText))
        let buttons = NSStackView(views: [copy, ok])
        buttons.spacing = 8
        let column = NSStackView(views: [message, buttons])
        column.orientation = .vertical
        column.alignment = .trailing
        column.spacing = 12
        column.edgeInsets = NSEdgeInsets(top: 14, left: 16, bottom: 14, right: 16)
        panel.contentView = column
        panel.setContentSize(column.fittingSize)
        NotificationCenter.default.addObserver(
            self, selector: #selector(closed), name: NSWindow.willCloseNotification, object: panel)
    }

    @objc private func copyText() {
        notice.copyText(to: pasteboard)
        panel.close()
    }

    @objc private func dismiss() { panel.close() }

    @objc private func closed() {
        NotificationCenter.default.removeObserver(self)
        Self.open.removeAll { $0 === self }
    }
}
