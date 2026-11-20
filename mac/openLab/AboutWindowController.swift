// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// What the app ships with that is not ours: the crates the engine and stage host link, with their
/// licences. `docs`' dependency register lists the same names; a test holds the two equal.
enum AboutCredits {
    static let entries: [(names: [String], licence: String)] = [
        (["cpal"], "Apache-2.0"),
        (["symphonia"], "MPL-2.0"),
        (["rubato"], "MIT"),
        (["rtrb"], "MIT/Apache-2.0"),
        (["crossbeam-channel"], "MIT/Apache-2.0"),
        (["assert_no_alloc"], "BSD-1-Clause"),
        (["rosc"], "MIT/Apache-2.0"),
        (["tokio", "tokio-tungstenite"], "MIT"),
        (["socket2"], "MIT/Apache-2.0"),
        (["futures-util"], "MIT/Apache-2.0"),
        (["getrandom"], "MIT/Apache-2.0"),
        (["serde", "serde_json"], "MIT/Apache-2.0"),
        (["uuid"], "MIT/Apache-2.0"),
        (["thiserror"], "MIT/Apache-2.0"),
        (["dirs"], "MIT/Apache-2.0"),
        (["unicode-normalization"], "MIT/Apache-2.0"),
        (["objc2", "objc2-foundation", "objc2-core-foundation", "dispatch2"], "Zlib OR Apache-2.0 OR MIT"),
        (["objc2-app-kit"], "Zlib OR Apache-2.0 OR MIT"),
        (["objc2-core-graphics"], "Zlib OR Apache-2.0 OR MIT"),
        (["objc2-metal"], "Zlib OR Apache-2.0 OR MIT"),
        (["objc2-quartz-core"], "Zlib OR Apache-2.0 OR MIT"),
        (["objc2-av-foundation", "objc2-core-media"], "Zlib OR Apache-2.0 OR MIT"),
        (["objc2-core-video"], "Zlib OR Apache-2.0 OR MIT"),
        (["objc2-image-io"], "Zlib OR Apache-2.0 OR MIT"),
        (["snap"], "BSD-3-Clause"),
    ]

    /// One line per entry: its crates, then the licence.
    static var text: String {
        entries.map { "\($0.names.joined(separator: ", "))  -  \($0.licence)" }.joined(separator: "\n")
    }
}

/// The window closes on Esc (the app's panic has already been sent by the key monitor).
private final class AboutWindow: NSWindow {
    override func cancelOperation(_ sender: Any?) { performClose(sender) }
}

/// The About window (app menu): name, version and build from the bundle, what the app is, its
/// licence and source links and a Credits control that lists the third-party components. Not modal:
/// it never stops the show behind it or takes the cue list's keys away for good.
@MainActor
final class AboutWindowController: NSWindowController {
    static let licenceURL = URL(string: "https://www.gnu.org/licenses/gpl-3.0.html")!
    static let sourceURL = URL(string: "https://github.com/jamiejcole/openLab")!

    static var version: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "dev"
    }
    static var build: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "0"
    }

    /// Opens a link (a test records it instead of leaving the app).
    var openURL: (URL) -> Void = { NSWorkspace.shared.open($0) }
    let versionLabel = NSTextField(labelWithString: "")
    let licenceButton = NSButton(title: loc("about.licenceLink"), target: nil, action: nil)
    let sourceButton = NSButton(title: loc("about.sourceLink"), target: nil, action: nil)
    let creditsButton = NSButton(title: loc("about.credits"), target: nil, action: nil)
    let creditsText = NSTextView(frame: .zero)
    let creditsScroll = NSScrollView(frame: .zero)
    private let column = NSStackView()

    init() {
        let window = AboutWindow(
            contentRect: NSRect(x: 0, y: 0, width: 360, height: 300),
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

        for (button, id, action) in [
            (licenceButton, AX.aboutLicenceLink, #selector(licenceClicked)),
            (sourceButton, AX.aboutSourceLink, #selector(sourceClicked)),
        ] {
            button.target = self
            button.action = action
            button.isBordered = false
            button.attributedTitle = NSAttributedString(
                string: button.title,
                attributes: [
                    .foregroundColor: NSColor.linkColor, .underlineStyle: NSUnderlineStyle.single.rawValue,
                ])
            button.setAX(id)
        }
        let links = NSStackView(views: [licenceButton, sourceButton])
        links.spacing = 16

        creditsButton.bezelStyle = .rounded
        creditsButton.target = self
        creditsButton.action = #selector(toggleCredits)
        creditsButton.setAX(AX.aboutCreditsButton)
        creditsText.isEditable = false
        creditsText.drawsBackground = false
        creditsText.font = .monospacedSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
        creditsText.textColor = .labelColor
        creditsText.textContainerInset = NSSize(width: 6, height: 6)
        creditsText.string = loc("about.credits.heading") + "\n\n" + AboutCredits.text
        creditsText.autoresizingMask = [.width]
        creditsScroll.documentView = creditsText
        creditsScroll.hasVerticalScroller = true
        creditsScroll.borderType = .bezelBorder
        creditsScroll.isHidden = true
        creditsScroll.setAX(AX.aboutCredits)
        creditsScroll.widthAnchor.constraint(equalToConstant: 320).isActive = true
        creditsScroll.heightAnchor.constraint(equalToConstant: 180).isActive = true

        column.orientation = .vertical
        column.alignment = .centerX
        column.spacing = 10
        column.edgeInsets = NSEdgeInsets(top: 24, left: 20, bottom: 20, right: 20)
        for view in [icon, name, versionLabel, blurb, licence, links, creditsButton, creditsScroll] {
            column.addArrangedSubview(view)
        }
        column.translatesAutoresizingMaskIntoConstraints = false
        let content = NSView()
        content.addSubview(column)
        NSLayoutConstraint.activate([
            column.topAnchor.constraint(equalTo: content.topAnchor),
            column.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            column.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            column.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            column.widthAnchor.constraint(equalToConstant: 360),
        ])
        window.contentView = content
        fit()
        window.center()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    private static func wrapping(_ text: String) -> NSTextField {
        let field = NSTextField(wrappingLabelWithString: text)
        field.alignment = .center
        field.preferredMaxLayoutWidth = 320
        return field
    }

    /// The window is as big as its content: no more, so nothing clips and nothing is left to resize.
    private func fit() {
        guard let window else { return }
        column.layoutSubtreeIfNeeded()
        window.setContentSize(column.fittingSize)
    }

    var creditsShown: Bool { !creditsScroll.isHidden }

    @objc func toggleCredits() {
        creditsScroll.isHidden.toggle()
        creditsButton.title = loc(creditsShown ? "about.credits.hide" : "about.credits")
        fit()
    }

    @objc private func licenceClicked() { openURL(Self.licenceURL) }
    @objc private func sourceClicked() { openURL(Self.sourceURL) }
}
