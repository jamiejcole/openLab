// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Where a Save puts a show: the file the Save panel chose and whether the show goes into a new
/// show folder of its own with a copy of its media (QLab 5, Managing Workspaces, manual p. 117),
/// or is saved as the file alone.
struct SaveTarget: Equatable {
    var url: URL
    var newFolder = false

    /// Most numbered folder names tried when the show's own is taken (`Show 2` ...).
    static let maxFolderNumber = 1000

    /// The Save panel's choice: a new show folder with the media (first), or the show file only.
    @MainActor
    static func choicePopup(newFolder: Bool) -> NSPopUpButton {
        let popup = NSPopUpButton(frame: .zero, pullsDown: false)
        popup.addItems(withTitles: [loc("save.choice.folder"), loc("save.choice.fileOnly")])
        popup.selectItem(at: newFolder ? 0 : 1)
        popup.setAX(AX.saveChoice, label: loc("save.choice.label"))
        return popup
    }

    /// The popup with its label, as the panel's accessory view.
    @MainActor
    static func accessory(_ popup: NSPopUpButton) -> NSView {
        let label = NSTextField(labelWithString: loc("save.choice.label"))
        let row = NSStackView(views: [label, popup])
        row.orientation = .horizontal
        row.spacing = 8
        row.edgeInsets = NSEdgeInsets(top: 8, left: 16, bottom: 8, right: 16)
        return row
    }

    /// Makes the show folder for a show saved as `url` (`/X/Show.openlab`): `/X/Show`, or the first
    /// free `/X/Show 2` ..., never an existing folder; answers the show file inside it. Made off the
    /// main thread: a slow volume must not hold up the window.
    static func makeShowFolder(for url: URL) async -> Result<URL, Error> {
        let parent = url.deletingLastPathComponent()
        let base = url.deletingPathExtension().lastPathComponent
        let file = url.lastPathComponent
        return await Task.detached(priority: .userInitiated) { () -> Result<URL, Error> in
            var lastError: Error = CocoaError(.fileWriteFileExists)
            for n in 1...maxFolderNumber {
                let name = n == 1 ? base : "\(base) \(n)"
                let folder = parent.appendingPathComponent(name, isDirectory: true)
                do {
                    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false)
                    return .success(folder.appendingPathComponent(file))
                } catch let error as CocoaError where error.code == .fileWriteFileExists {
                    lastError = error
                } catch {
                    return .failure(error)
                }
            }
            return .failure(lastError)
        }.value
    }
}
