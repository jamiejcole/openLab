// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// Which engine the app talks to. `-OpenLabEngine fake` (or `fake-single`) runs the in-process
/// fake so UI tests need no engine; the default is the real one.
enum EngineMode: Equatable {
    case real
    case fake(FakeEngine.Mode)

    /// Only the process arguments are read (never a persisted default), and only in Debug builds:
    /// a shipping app cannot be switched to the fake engine. (The fake and its welcome.json stay
    /// in the Release bundle: gating the XcodeGen sources per configuration is not worth it.)
    static func fromLaunchArguments(_ arguments: [String] = ProcessInfo.processInfo.arguments) -> EngineMode {
        #if DEBUG
            guard let i = arguments.firstIndex(of: "-OpenLabEngine"), i + 1 < arguments.count else { return .real }
            switch arguments[i + 1] {
            case "fake": return .fake(.multi)
            case "fake-single": return .fake(.single)
            default: return .real
            }
        #else
            return .real
        #endif
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
