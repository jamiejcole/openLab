// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

// Debug only: project.yml excludes this file (and the fake engine) from Release builds.

/// The fake engine as a process: `crash()` ends it leaving its `engine.json` behind, and
/// `spawnEngine` starts a new run of it (new token and pid, nothing open).
@MainActor
final class FakeEngineProcesses: EngineProcesses {
    let engine: FakeEngine
    private(set) var running = true
    private(set) var spawns = 0
    /// What `engine.json` holds: the last run's, also after a crash.
    private var file: EngineConnectionInfo?

    init(engine: FakeEngine) {
        self.engine = engine
        file = engine.info
    }

    func crash() {
        running = false
        engine.dropConnections()
        engine.connectFailure = true
    }

    func readInfo() -> Result<EngineConnectionInfo, EngineConnectionInfo.LoadError> {
        file.map { .success($0) } ?? .failure(.missing)
    }

    func isEngine(_ pid: Int32) -> Bool { running && pid == engine.pid }

    func signal(_ pid: Int32, _ signal: EngineSignal) {
        if isEngine(pid) { crash() }
    }

    func spawnEngine() throws -> Int32 {
        spawns += 1
        if !running {
            engine.restart()
            engine.connectFailure = false
            running = true
        }
        file = engine.info
        return engine.pid
    }
}
