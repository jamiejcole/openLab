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
    /// Why something did not open at launch (shown above the recents; empty: hidden).
    let noticeLabel = NSTextField(wrappingLabelWithString: "")

    /// `helpURL`: where help is (the build's page by default); nil shows no link.
    init(helpURL: URL? = LauncherWindowController.helpURL) {
        self.helpURL = helpURL
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

        // What this is: its version, and a link to help where the build says one (the Info.plist
        // key `OpenLabHelpURL`; a build with none shows no link).
        let version = NSTextField(labelWithString: String(format: loc("launcher.version"), Self.version))
        version.font = .systemFont(ofSize: 11)
        version.textColor = .secondaryLabelColor
        version.setAX(AX.launcherVersion)
        var about: [NSView] = [version]
        if helpURL != nil {
            let help = NSButton(title: loc("launcher.help"), target: self, action: #selector(helpClicked))
            help.bezelStyle = .inline
            help.setAX(AX.launcherHelp)
            about.append(help)
        }
        let aboutRow = NSStackView(views: about)
        aboutRow.orientation = .horizontal
        aboutRow.spacing = 8
        noticeLabel.textColor = .systemRed
        noticeLabel.isHidden = true
        noticeLabel.setAX(AX.launcherNotice)
        let column = NSStackView(views: [noticeLabel, new, open, heading, recentStack, aboutRow])
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

    func showNotice(_ text: String?) {
        noticeLabel.stringValue = text ?? ""
        noticeLabel.isHidden = text == nil
    }

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

    /// The app's version as the bundle says it (`dev` for a bundle that says none, as a test's).
    static var version: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "dev"
    }

    /// Where help is, if the build names a page (an http or https URL).
    static var helpURL: URL? {
        guard let text = Bundle.main.object(forInfoDictionaryKey: "OpenLabHelpURL") as? String,
            let url = URL(string: text), ["http", "https"].contains(url.scheme)
        else { return nil }
        return url
    }

    let helpURL: URL?
    /// Opens a URL (a test gives its own).
    var openURL: (URL) -> Void = { NSWorkspace.shared.open($0) }
    @objc private func helpClicked() { helpURL.map(openURL) }

    @objc private func newClicked() { onNew?() }
    @objc private func openClicked() { onOpen?() }
    @objc private func recentClicked(_ sender: NSButton) {
        if recents.indices.contains(sender.tag) { onOpenRecent?(recents[sender.tag]) }
    }
}
