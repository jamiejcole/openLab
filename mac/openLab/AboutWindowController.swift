// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The window closes on Esc (the app's panic has already been sent by the key monitor).
private final class AboutWindow: NSWindow {
    override func cancelOperation(_ sender: Any?) { performClose(sender) }
}

/// The About window (app menu): icon, name, version and build from the bundle, what the app is and
/// its licence. Not modal: it never stops the show behind it, and it is as big as its content.
@MainActor
final class AboutWindowController: NSWindowController {
    static var version: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "dev"
    }
    static var build: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "0"
    }

    let versionLabel = NSTextField(labelWithString: "")

    init() {
        let window = AboutWindow(
            contentRect: NSRect(x: 0, y: 0, width: 320, height: 240),
            styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = loc("menu.app.about")
        window.identifier = NSUserInterfaceItemIdentifier(AX.aboutWindow)
        window.isReleasedWhenClosed = false
        super.init(window: window)

        let icon = NSImageView(image: NSApp?.applicationIconImage ?? NSImage())
        icon.widthAnchor.constraint(equalToConstant: 64).isActive = true
        icon.heightAnchor.constraint(equalToConstant: 64).isActive = true
        let name = NSTextField(labelWithString: loc("about.name"))
        name.font = .boldSystemFont(ofSize: 18)
        name.setAX(AX.aboutName)
        versionLabel.stringValue = String(format: loc("about.version"), Self.version, Self.build)
        versionLabel.textColor = .secondaryLabelColor
        versionLabel.setAX(AX.aboutVersion)
        let blurb = Self.wrapping(loc("about.description"))
        blurb.setAX(AX.aboutDescription)
        let licence = Self.wrapping(loc("about.licence"))
        licence.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        licence.textColor = .secondaryLabelColor
        licence.setAX(AX.aboutLicence)

        let column = NSStackView(views: [icon, name, versionLabel, blurb, licence])
        column.orientation = .vertical
        column.alignment = .centerX
        column.spacing = 10
        column.edgeInsets = NSEdgeInsets(top: 24, left: 20, bottom: 24, right: 20)
        column.translatesAutoresizingMaskIntoConstraints = false
        let content = NSView()
        content.addSubview(column)
        NSLayoutConstraint.activate([
            column.topAnchor.constraint(equalTo: content.topAnchor),
            column.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            column.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            column.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            column.widthAnchor.constraint(equalToConstant: 320),
        ])
        window.contentView = content
        column.layoutSubtreeIfNeeded()
        window.setContentSize(column.fittingSize)
        window.center()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    private static func wrapping(_ text: String) -> NSTextField {
        let field = NSTextField(wrappingLabelWithString: text)
        field.alignment = .center
        field.preferredMaxLayoutWidth = 280
        return field
    }
}
