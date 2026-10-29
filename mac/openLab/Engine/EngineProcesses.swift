// SPDX-License-Identifier: GPL-3.0-or-later

import Darwin
import Foundation

enum EngineSignal: Equatable, Sendable {
    /// SIGTERM: the engine writes its recovery files and exits.
    case terminate
    /// SIGKILL: the last resort for an engine that did not end on SIGTERM.
    case kill
}

enum EngineSpawnError: Error, Equatable, Sendable {
    case missingExecutable(String)
    case spawnFailed(Int32)
}

/// What the supervisor does to processes and `engine.json`. A protocol so tests use fakes: no
/// real process is started, signalled or waited for in a unit test.
@MainActor
protocol EngineProcesses: AnyObject {
    func readInfo() -> Result<EngineConnectionInfo, EngineConnectionInfo.LoadError>
    /// Whether `pid` is a live openLab engine (not merely a live process: a stale file's pid may
    /// have been reused by any other program, which must never be signalled).
    func isEngine(_ pid: Int32) -> Bool
    /// Signals `pid` only if it is still an engine.
    func signal(_ pid: Int32, _ signal: EngineSignal)
    /// Starts an engine (detached, its own process group); its pid.
    func spawnEngine() throws -> Int32
}

/// The real thing: `<app-support>/openLab/engine.json` and `Contents/MacOS/openlab-engine`.
@MainActor
final class SystemEngineProcesses: EngineProcesses {
    static let engineName = "openlab-engine"

    let executable: URL?
    let bundleID: String
    let infoURL: URL
    /// What the engine prints on stderr, read back into the app's log.
    let stderrTail: EngineStderrTail
    private let watchStderr: Bool
    private var stderrWatcher: EngineStderrWatcher?

    init(
        executable: URL? = Bundle.main.url(forAuxiliaryExecutable: SystemEngineProcesses.engineName),
        bundleID: String = Bundle.main.bundleIdentifier ?? "dev.openlab.openLab",
        infoURL: URL = EngineConnectionInfo.defaultURL, stderrURL: URL? = nil, watchStderr: Bool = true
    ) {
        self.executable = executable
        self.bundleID = bundleID
        self.infoURL = infoURL
        stderrTail = EngineStderrTail(
            url: stderrURL ?? infoURL.deletingLastPathComponent().appendingPathComponent("engine-stderr.log"))
        self.watchStderr = watchStderr
    }

    func readInfo() -> Result<EngineConnectionInfo, EngineConnectionInfo.LoadError> {
        EngineConnectionInfo.load(from: infoURL)
    }

    func isEngine(_ pid: Int32) -> Bool {
        // 0 and -1 would address a process group or every process: never a pid to look at.
        guard pid > 1 else { return false }
        // An engine this app started and that has exited is a zombie until reaped; reap it here so
        // it reads as gone (a pid that is not our child answers ECHILD and is left alone).
        var status: Int32 = 0
        if waitpid(pid, &status, WNOHANG) == pid { return false }
        guard kill(pid, 0) == 0 || errno == EPERM else { return false }
        return Self.executableName(of: pid) == Self.engineName
    }

    func signal(_ pid: Int32, _ signal: EngineSignal) {
        guard isEngine(pid) else { return }
        let sig = signal == .terminate ? SIGTERM : SIGKILL
        if kill(pid, sig) != 0 {
            engineLog.error("could not signal engine pid \(pid, privacy: .public): errno \(errno, privacy: .public)")
        }
    }

    /// `openlab-engine --relaunch-app BUNDLE_ID`, stdin/stdout on /dev/null, stderr appended to
    /// `engine-stderr.log` (the app logs what lands there; /dev/null if the file cannot be made), in its own
    /// process group (a signal to the app's group does not reach it), no other descriptor
    /// inherited. Not `--watch-stdin`: the engine must outlive the app (it relaunches it).
    func spawnEngine() throws -> Int32 {
        guard let executable, FileManager.default.isExecutableFile(atPath: executable.path) else {
            throw EngineSpawnError.missingExecutable(executable?.path ?? Self.engineName)
        }
        var attr: posix_spawnattr_t?
        posix_spawnattr_init(&attr)
        defer { posix_spawnattr_destroy(&attr) }
        var noSignals = sigset_t()
        sigemptyset(&noSignals)
        posix_spawnattr_setsigmask(&attr, &noSignals)
        posix_spawnattr_setpgroup(&attr, 0)
        let flags = POSIX_SPAWN_SETPGROUP | POSIX_SPAWN_SETSIGMASK | POSIX_SPAWN_CLOEXEC_DEFAULT
        posix_spawnattr_setflags(&attr, Int16(flags))

        var actions: posix_spawn_file_actions_t?
        posix_spawn_file_actions_init(&actions)
        defer { posix_spawn_file_actions_destroy(&actions) }
        posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0)
        posix_spawn_file_actions_addopen(&actions, 1, "/dev/null", O_WRONLY, 0)
        // Opened here, not by posix_spawn: a file that cannot be opened (read-only, a directory, gone
        // since the check) must cost the engine its stderr log, never its start.
        let logFD =
            stderrTail.prepare()
            ? open(stderrTail.url.path, O_WRONLY | O_APPEND | O_CREAT | O_NOFOLLOW | O_CLOEXEC, 0o600) : -1
        defer { if logFD >= 0 { close(logFD) } }
        let logged = logFD >= 0
        if logged {
            posix_spawn_file_actions_adddup2(&actions, logFD, 2)
        } else {
            engineLog.error("no stderr file for the engine: its error output is discarded")
            posix_spawn_file_actions_addopen(&actions, 2, "/dev/null", O_WRONLY, 0)
        }

        let args = [executable.path, "--relaunch-app", bundleID]
        let env = ProcessInfo.processInfo.environment.map { "\($0.key)=\($0.value)" }
        var argv: [UnsafeMutablePointer<CChar>?] = args.map { strdup($0) } + [nil]
        var envp: [UnsafeMutablePointer<CChar>?] = env.map { strdup($0) } + [nil]
        defer {
            for p in argv { free(p) }
            for p in envp { free(p) }
        }
        var pid: pid_t = 0
        let rc = posix_spawn(&pid, executable.path, &actions, &attr, &argv, &envp)
        guard rc == 0 else { throw EngineSpawnError.spawnFailed(rc) }
        if logged, watchStderr {
            stderrWatcher?.stop()
            let watcher = EngineStderrWatcher(tail: stderrTail)
            watcher.start()
            stderrWatcher = watcher
        }
        return pid
    }

    /// The file name of the program running as `pid`, or nil.
    static func executableName(of pid: Int32) -> String? {
        var buffer = [CChar](repeating: 0, count: 4 * Int(MAXPATHLEN))  // PROC_PIDPATHINFO_MAXSIZE
        let n = proc_pidpath(pid, &buffer, UInt32(buffer.count))
        guard n > 0 else { return nil }
        let path = String(decoding: buffer.prefix(Int(n)).map { UInt8(bitPattern: $0) }, as: UTF8.self)
        return (path as NSString).lastPathComponent
    }
}
