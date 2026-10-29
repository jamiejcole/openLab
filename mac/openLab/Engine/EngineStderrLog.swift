// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// Reads what the engine prints on stderr (why it could not start, a Rust panic message) from a
/// file the engine was started with as its stderr, so the app's log shows it.
///
/// A file, not a pipe: the engine outlives the app (it relaunches it), and a pipe whose reader
/// is gone fails every later write with EPIPE, which Rust's `eprintln!` turns into a panic. A
/// file never blocks or fails the engine whatever the app does. Everything here is bounded: the
/// file is emptied at the start of a run once it is over `fileLimit`, a read takes at most
/// `readLimit` bytes (older output is skipped), a line is cut at `lineLimit` characters and at
/// most `lineBudget` lines are returned in all.
final class EngineStderrTail: @unchecked Sendable {
    static let fileLimit: UInt64 = 1 << 20
    static let readLimit = 64 * 1024
    static let lineLimit = 2000
    static let lineBudget = 200

    let url: URL
    private let lock = NSLock()
    private var offset: UInt64 = 0
    private var partial = Data()
    private var returned = 0
    private var budgetSaid = false

    init(url: URL) { self.url = url }

    /// Before the engine starts: the folder and file exist, an oversized file is emptied, and
    /// reading begins at its end (the engine appends). False: no file could be made.
    func prepare() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        let fm = FileManager.default
        do {
            try fm.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        } catch {
            return false
        }
        let size = (try? fm.attributesOfItem(atPath: url.path))?[.size] as? NSNumber
        if let size, size.uint64Value <= Self.fileLimit {
            offset = size.uint64Value
        } else {
            guard fm.createFile(atPath: url.path, contents: nil, attributes: [.posixPermissions: 0o600]) else {
                return false
            }
            offset = 0
        }
        partial = Data()
        returned = 0
        budgetSaid = false
        return true
    }

    /// The complete lines written since the last call.
    func read() -> [String] {
        lock.lock()
        defer { lock.unlock() }
        guard let handle = try? FileHandle(forReadingFrom: url) else { return [] }
        defer { try? handle.close() }
        guard let end = try? handle.seekToEnd() else { return [] }
        if end < offset {  // emptied under us
            offset = 0
            partial = Data()
        }
        if end - offset > UInt64(Self.readLimit) {  // a flood: keep the newest part only
            offset = end - UInt64(Self.readLimit)
            partial = Data()
        }
        guard end > offset, (try? handle.seek(toOffset: offset)) != nil,
            let chunk = try? handle.read(upToCount: Int(end - offset))
        else { return [] }
        offset += UInt64(chunk.count)
        partial.append(chunk)

        var lines: [String] = []
        while let newline = partial.firstIndex(of: 0x0A) {
            lines.append(Self.text(partial[partial.startIndex..<newline]))
            partial.removeSubrange(partial.startIndex...newline)
        }
        if partial.count > Self.lineLimit * 4 {  // no newline in sight: do not keep growing
            lines.append(Self.text(partial))
            partial = Data()
        }
        return budgeted(lines.filter { !$0.isEmpty })
    }

    private func budgeted(_ lines: [String]) -> [String] {
        var out: [String] = []
        for line in lines {
            if returned < Self.lineBudget {
                returned += 1
                out.append(line)
            } else if !budgetSaid {
                budgetSaid = true
                out.append("(further engine stderr output is not logged)")
            }
        }
        return out
    }

    private static func text(_ bytes: Data) -> String {
        let s = String(decoding: bytes, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        return s.count > lineLimit ? String(s.prefix(lineLimit)) + "..." : s
    }
}

/// Logs the new output of an `EngineStderrTail` to `engineLog` whenever the file grows, on a
/// utility queue (never the main thread).
final class EngineStderrWatcher: @unchecked Sendable {
    private let tail: EngineStderrTail
    private let sink: @Sendable (String) -> Void
    private var source: DispatchSourceFileSystemObject?
    /// Called on the watcher's queue once the file descriptor is closed (after `stop`).
    var onClosed: (@Sendable () -> Void)?

    init(
        tail: EngineStderrTail,
        sink: @escaping @Sendable (String) -> Void = { engineLog.error("engine stderr: \($0, privacy: .public)") }
    ) {
        self.tail = tail
        self.sink = sink
    }

    func start() {
        let fd = open(tail.url.path, O_EVTONLY)
        guard fd >= 0 else {
            engineLog.error("cannot watch the engine's stderr file")
            return
        }
        let source = DispatchSource.makeFileSystemObjectSource(
            fileDescriptor: fd, eventMask: [.extend, .write], queue: DispatchQueue.global(qos: .utility))
        source.setEventHandler { [tail, sink] in
            for line in tail.read() { sink(line) }
        }
        let closed = onClosed
        source.setCancelHandler {
            close(fd)
            closed?()
        }
        self.source = source
        source.resume()
    }

    func stop() {
        source?.cancel()
        source = nil
    }

    deinit { source?.cancel() }
}
