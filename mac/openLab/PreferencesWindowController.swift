// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The app-wide preferences (the app menu's Preferences; Workspace Settings are per workspace).
/// General tab: what happens at launch.
@MainActor
final class PreferencesWindowController: NSWindowController {
    let launchPopup = NSPopUpButton(frame: .zero, pullsDown: false)
    private let preferences: LaunchPreferences

    init(preferences: LaunchPreferences) {
        self.preferences = preferences
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 460, height: 150),
            styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = loc("prefs.title")
        window.identifier = NSUserInterfaceItemIdentifier(AX.preferencesWindow)
        window.isReleasedWhenClosed = false
        super.init(window: window)

        let label = NSTextField(labelWithString: loc("prefs.launch.label"))
        for behavior in LaunchBehavior.allCases {
            launchPopup.addItem(withTitle: loc(behavior.titleKey))
            launchPopup.lastItem?.representedObject = behavior.rawValue
        }
        launchPopup.target = self
        launchPopup.action = #selector(launchChanged)
        launchPopup.setAX(AX.preferencesLaunchPopup)
        let row = NSStackView(views: [label, launchPopup])
        row.orientation = .horizontal
        row.spacing = 8
        row.edgeInsets = NSEdgeInsets(top: 20, left: 20, bottom: 20, right: 20)

        let tab = NSTabViewItem(identifier: "general")
        tab.label = loc("prefs.general")
        tab.view = row
        let tabs = NSTabView()
        tabs.addTabViewItem(tab)
        window.contentView = tabs
        window.center()
        reload()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Shows the stored choice (the window is reused between openings).
    func reload() {
        launchPopup.selectItem(at: LaunchBehavior.allCases.firstIndex(of: preferences.behavior) ?? 0)
    }

    @objc private func launchChanged() {
        guard let raw = launchPopup.selectedItem?.representedObject as? String,
            let behavior = LaunchBehavior(rawValue: raw)
        else { return }
        preferences.behavior = behavior
    }
}
