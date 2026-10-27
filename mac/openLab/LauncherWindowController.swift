// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Shown when no workspace is open: New, Open and the recent workspaces. (The Connect and
/// Templates tabs wait for their backends.)
@MainActor
final class LauncherWindowController: NSWindowController {
    var onNew: (() -> Void)?
    var onOpen: (() -> Void)?
    var onOpenRecent: ((URL) -> Void)?
    private let recentStack = NSStackView()
    private var recents: [URL] = []

    init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 460, height: 420),
            styleMask: [.titled, .closable, .miniaturizable], backing: .buffered, defer: false)
        window.title = loc("launcher.title")
        window.identifier = NSUserInterfaceItemIdentifier(AX.launcherWindow)
        window.isReleasedWhenClosed = false
        super.init(window: window)

        let new = NSButton(title: loc("launcher.new"), target: self, action: #selector(newClicked))
        new.setAX(AX.launcherNew)
        let open = NSButton(title: loc("launcher.open"), target: self, action: #selector(openClicked))
        open.setAX(AX.launcherOpen)
        let heading = NSTextField(labelWithString: loc("launcher.recent"))
        heading.font = .boldSystemFont(ofSize: 13)
        recentStack.orientation = .vertical
        recentStack.alignment = .leading
        recentStack.spacing = 4

        let column = NSStackView(views: [new, open, heading, recentStack])
        column.orientation = .vertical
        column.alignment = .leading
        column.spacing = 12
        column.edgeInsets = NSEdgeInsets(top: 20, left: 20, bottom: 20, right: 20)
        window.contentView = column
        window.center()
        reload(recents: [])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func reload(recents urls: [URL]) {
        recents = Array(urls.prefix(10))
        for v in recentStack.arrangedSubviews { v.removeFromSuperview() }
        if recents.isEmpty {
            recentStack.addArrangedSubview(NSTextField(labelWithString: loc("launcher.recent.none")))
        }
        for (i, url) in recents.enumerated() {
            let b = NSButton(
                title: url.deletingPathExtension().lastPathComponent, target: self,
                action: #selector(recentClicked(_:)))
            b.tag = i
            b.bezelStyle = .inline
            b.toolTip = url.path
            b.setAX(AX.launcherRecentPrefix + String(i))
            recentStack.addArrangedSubview(b)
        }
    }

    @objc private func newClicked() { onNew?() }
    @objc private func openClicked() { onOpen?() }
    @objc private func recentClicked(_ sender: NSButton) {
        if recents.indices.contains(sender.tag) { onOpenRecent?(recents[sender.tag]) }
    }
}
