// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Looks up a user-facing string in Localizable.strings.
func loc(_ key: String) -> String {
    NSLocalizedString(key, comment: "")
}

/// Accessibility identifiers for the major areas (tests and XCUITests find views by these).
/// Hyphenated on purpose: dotted names are reserved for localisation keys.
enum AX {
    static let mainWindow = "main-window"
    static let launcherWindow = "launcher-window"
    static let launcherNew = "launcher-new"
    static let launcherOpen = "launcher-open"
    static let launcherRecentPrefix = "launcher-recent-"
    static let goButton = "go-button"
    static let standbyField = "standby-field"
    static let standbyNotes = "standby-notes"
    static let masthead = "masthead"
    static let cueToolbar = "cue-toolbar"
    static let cueList = "cue-list"
    static let inspector = "inspector"
    static let inspectorEmpty = "inspector-empty"
    static let sidebar = "sidebar"
    static let sidebarTabs = "sidebar-tabs"
    static let sidebarList = "sidebar-list"
    static let newListButton = "new-list-button"
    static let newCartButton = "new-cart-button"
    static let transportReset = "transport-reset"
    static let transportPause = "transport-pause"
    static let transportResume = "transport-resume"
    static let transportPanic = "transport-panic"
    static let footer = "footer"
    static let modeSwitch = "mode-switch"
    static let statusLabel = "status-label"
    static let settingsButton = "settings-button"
    static let toggleToolbar = "toggle-toolbar"
    static let toggleMasthead = "toggle-masthead"
    static let toggleInspector = "toggle-inspector"
    static let toggleSidebar = "toggle-sidebar"
}

extension NSView {
    func setAX(_ id: String, label: String? = nil) {
        setAccessibilityIdentifier(id)
        if let label { setAccessibilityLabel(label) }
    }
}

func symbolImage(_ name: String, _ description: String) -> NSImage? {
    NSImage(systemSymbolName: name, accessibilityDescription: description)
}
