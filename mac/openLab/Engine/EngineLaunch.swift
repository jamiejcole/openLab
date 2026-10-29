// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// Which engine the app talks to. `-OpenLabEngine fake` runs the in-process fake so UI tests need
/// no engine (and never start a real one); the default is the real one. With the fake,
/// `-OpenLabFakeEmpty` opens no workspace and `-OpenLabFakeCrashAfter SECONDS` makes it crash that
/// long after launch (the crash-alarm UI test).
enum EngineMode: Equatable {
    case real
    #if DEBUG
        case fake(crashAfter: Double?, empty: Bool = false)
    #endif

    /// Only the process arguments are read (never a persisted default), and only in Debug builds:
    /// a shipping app cannot be switched to the fake engine. Release builds do not contain it at
    /// all: project.yml excludes the fake engine, its shows and its welcome.json from them.
    static func fromLaunchArguments(_ arguments: [String] = ProcessInfo.processInfo.arguments) -> EngineMode {
        #if DEBUG
            guard let i = arguments.firstIndex(of: "-OpenLabEngine"), i + 1 < arguments.count,
                arguments[i + 1] == "fake"
            else { return .real }
            var crashAfter: Double?
            if let c = arguments.firstIndex(of: "-OpenLabFakeCrashAfter"), c + 1 < arguments.count {
                crashAfter = Double(arguments[c + 1])
            }
            return .fake(crashAfter: crashAfter, empty: arguments.contains("-OpenLabFakeEmpty"))
        #else
            return .real
        #endif
    }

    /// The app is the host process of the unit tests (XCTest sets this for it). It must start no
    /// engine: the real engine is detached and relaunches the app when it ends, so on a machine
    /// with no engine running every test run left an engine behind that relaunched the app over
    /// the UI tests (they then could not bring their own instance to the front).
    static func isUnitTestHost(_ environment: [String: String] = ProcessInfo.processInfo.environment) -> Bool {
        environment["XCTestConfigurationFilePath"] != nil || environment["XCTestBundlePath"] != nil
    }

    @MainActor
    func make() -> (EngineSession, EngineSupervisor) {
        switch self {
        case .real:
            let processes = SystemEngineProcesses()
            let session = EngineSession(transport: WebSocketTransport(), infoProvider: { processes.readInfo() })
            return (session, EngineSupervisor(processes: processes, session: session))
        #if DEBUG
            case .fake(let crashAfter, let empty):
                let engine = FakeEngine(snapshot: FakeEngine.demoSnapshot())
                // Two clean workspaces so UI tests start with several windows and no save prompts.
                for doc in engine.openDocs { engine.setDocument(doc, dirty: false) }
                // The demo show has broken cues, so its pre-show check reports them.
                engine.preshowProblems = true
                _ = engine.addDoc(FakeEngine.emptySnapshot(name: "Second"), path: nil)
                if empty { engine.closeAll() }
                let processes = FakeEngineProcesses(engine: engine)
                let session = EngineSession(transport: engine, infoProvider: { processes.readInfo() })
                if let crashAfter {
                    Task { @MainActor in
                        try? await Task.sleep(nanoseconds: UInt64(max(0, crashAfter) * 1_000_000_000))
                        processes.crash()
                    }
                }
                return (session, EngineSupervisor(processes: processes, session: session))
        #endif
        }
    }
}
