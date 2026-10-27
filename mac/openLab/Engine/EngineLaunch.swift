// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// Which engine the app talks to. `-OpenLabEngine fake` (or `fake-single`) runs the in-process
/// fake so UI tests need no engine; the default is the real one.
enum EngineMode: Equatable {
    case real
    case fake(FakeEngine.Mode)

    static func fromLaunchArguments(_ defaults: UserDefaults = .standard) -> EngineMode {
        switch defaults.string(forKey: "OpenLabEngine") {
        case "fake": return .fake(.multi)
        case "fake-single": return .fake(.single)
        default: return .real
        }
    }

    @MainActor
    func makeSession() -> EngineSession {
        switch self {
        case .real:
            return EngineSession(transport: WebSocketTransport(), infoProvider: { EngineConnectionInfo.load() })
        case .fake(let mode):
            let engine = FakeEngine(mode: mode)
            let info = engine.info
            return EngineSession(transport: engine, infoProvider: { .success(info) })
        }
    }
}
