// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// What one look at a recent workspace's file found (made off the main thread).
struct RecentProbe: Sendable, Equatable {
    /// The file is where it was.
    var exists: Bool
    /// The file is not there but its bookmark found it here (moved or renamed, or in the Trash).
    var movedTo: URL?
    /// A bookmark made while the file existed and none was held.
    var newBookmark: Data?
}

/// Checks recent workspaces' files, and resolves their bookmarks, off the main thread, for the
/// launcher and the Open Recent menu alike. A file on a volume that has stopped answering can hold
/// a check for many seconds: nothing waits for one; an unanswered check is marked unreachable after
/// `waitSeconds`, and a late answer still replaces that.
@MainActor
final class RecentChecker {
    enum State: Equatable {
        case checking, found, missing, unreachable
    }

    /// How long a check may take before its row says the file is not reachable.
    static let waitSeconds = 4.0

    /// Whether a file is there (a seam: tests list files that do not exist). Runs off the main thread.
    var exists: @Sendable (URL) -> Bool = { FileManager.default.fileExists(atPath: $0.path) }
    /// Runs `work`, the look at one file, away from the main thread and delivers its result on the
    /// main actor (a seam: a test holds one file's work back, or runs it at once).
    var run: (URL, @escaping @Sendable () -> RecentProbe, @escaping @MainActor (RecentProbe) -> Void) -> Void = {
        _, work, deliver in
        DispatchQueue.global(qos: .utility).async {
            let result = work()
            DispatchQueue.main.async { MainActor.assumeIsolated { deliver(result) } }
        }
    }
    var scheduler: Scheduler = SystemScheduler()
    /// Called when an answer came or a wait ran out.
    var onChange: (@MainActor () -> Void)?

    private(set) var states: [URL: State] = [:]
    private(set) var probes: [URL: RecentProbe] = [:]
    private var generation: [URL: Int] = [:]
    /// Files whose look has not answered yet: a dead volume holds one thread per file, however often
    /// the launcher or the menu asks again, and that first look's wait still runs out.
    private var inFlight: Set<URL> = []

    /// What is known of `url`: a file not answered for yet is `checking`, which callers treat as usable.
    func state(of url: URL) -> State { states[url] ?? .checking }

    /// Looks at each file again. Nothing waits: rows keep what they showed until an answer comes.
    func refresh(_ urls: [URL], bookmarks: [String: Data]) {
        let exists = exists
        // A file last found is not known to be there now: it reads as unchecked until answered.
        for url in urls where states[url] == .found { states[url] = nil }
        for url in urls where !inFlight.contains(url) {
            inFlight.insert(url)
            let token = (generation[url] ?? 0) + 1
            generation[url] = token
            let held = bookmarks[url.path]
            run(
                url,
                { Self.probe(url, bookmark: held, exists: exists) },
                { [weak self] result in
                    guard let self else { return }
                    inFlight.remove(url)
                    guard generation[url] == token else { return }
                    probes[url] = result
                    states[url] = result.exists ? .found : .missing
                    onChange?()
                })
            let scheduler = scheduler
            Task { @MainActor [weak self] in
                try? await scheduler.sleep(seconds: Self.waitSeconds)
                guard let self, generation[url] == token, states[url] == nil else { return }
                states[url] = .unreachable
                onChange?()
            }
        }
    }

    nonisolated static func probe(_ url: URL, bookmark: Data?, exists: @Sendable (URL) -> Bool) -> RecentProbe {
        if exists(url) {
            let made =
                bookmark == nil
                ? try? url.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil) : nil
            return RecentProbe(exists: true, movedTo: nil, newBookmark: made)
        }
        var moved: URL?
        if let bookmark {
            var stale = false
            if let now = try? URL(
                resolvingBookmarkData: bookmark, options: [.withoutUI, .withoutMounting], relativeTo: nil,
                bookmarkDataIsStale: &stale), exists(now)
            {
                moved = now
            }
        }
        return RecentProbe(exists: false, movedTo: moved, newBookmark: nil)
    }
}
