// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation
import os

/// Typed notes that cannot reach the engine are appended in full to one small local text file, so
/// they outlive the notice panel, Quit and Show Mode. The text can be private: it goes to this file
/// only, never to the OS log, and a failed write is logged without it and never stops anything.
@MainActor
enum NotesKeeper {
    static let fileName = "unsaved-notes.txt"

    /// Where the file lives. Tests point it at their own folder.
    static var folder: URL = {
        let base =
            FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Library/Application Support")
        return base.appendingPathComponent("openLab")
    }()

    /// Appends `text` under a line saying when and why. The file's URL, or nil when it could not be written.
    @discardableResult
    static func keep(_ text: String, reason: String, now: Date = Date()) -> URL? {
        let url = folder.appendingPathComponent(fileName)
        let entry = "--- \(ISO8601DateFormatter().string(from: now)): \(reason)\n\(text)\n\n"
        do {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            if !FileManager.default.fileExists(atPath: url.path) {
                let made = FileManager.default.createFile(
                    atPath: url.path, contents: nil, attributes: [.posixPermissions: 0o600])
                if !made { throw CocoaError(.fileWriteUnknown) }
            }
            let handle = try FileHandle(forWritingTo: url)
            defer { try? handle.close() }
            try handle.seekToEnd()
            try handle.write(contentsOf: Data(entry.utf8))
            return url
        } catch {
            engineLog.error("could not keep unsaved notes: \(String(describing: error), privacy: .public)")
            return nil
        }
    }
}
