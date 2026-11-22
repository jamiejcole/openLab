// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// A panel that never takes the keys: Space, Return and every hotkey go on to the workspace window.
final class NonKeyPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

/// The notice for typed notes that were not saved: the cue, the reason, OK and Copy Text. A standard
/// alert window that belongs to no window, never becomes key and blocks nothing, so Close, Quit,
/// Show Mode and GO go on while it is up. It stays when its workspace closes, so Copy Text still works,
/// and goes when the app quits. A second notice opens a second alert, stepped down from the last.
@MainActor
final class NotesNoticePanel: NSObject {
    /// The alerts showing now.
    private(set) static var open: [NotesNoticePanel] = []

    private let alert: FloatingAlert
    var panel: NSWindow { alert.window }
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
        var close: () -> Void = {}
        var copy: () -> Void = {}
        alert = FloatingAlert(
            critical: false, title: loc("notes.notice.title"), message: notice.message,
            buttons: [
                .init(title: loc("alert.ok"), id: nil, action: { close() }),
                .init(title: loc("alert.copyText"), id: nil, action: { copy() }),
            ])
        super.init()
        close = { [weak self] in self?.panel.close() }
        copy = { [weak self] in self?.copyText() }
        NotificationCenter.default.addObserver(
            self, selector: #selector(closed), name: NSWindow.willCloseNotification, object: panel)
    }

    private func copyText() {
        notice.copyText(to: pasteboard)
        panel.close()
    }

    @objc private func closed() {
        NotificationCenter.default.removeObserver(self)
        Self.open.removeAll { $0 === self }
    }
}
