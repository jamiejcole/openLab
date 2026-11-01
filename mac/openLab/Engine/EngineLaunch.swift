// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// Which engine the app starts. The default is the shipping one: detached, relaunching the app
/// when it ends, its `engine.json` in the user's Application Support folder. Debug builds also have
/// the test engine (`-OpenLabEngine test`): the same bundled engine on its virtual audio device,
/// with an Application Support folder of its own, ending with the app (`--watch-stdin`) and never
/// relaunching it. The app's tests run against it.
enum EngineMode: Equatable {
    case real
    #if DEBUG
        case test(TestEngineOptions)
    #endif

    /// Only the process arguments are read (never a persisted default), and only in Debug builds:
    /// a shipping app cannot be switched to the test engine (and its engine has no virtual device).
    ///
    /// The unit-test host always runs the test engine on the stepped clock, whatever its arguments:
    /// a shipping engine started from a test run would outlive it and relaunch the app over the
    /// UI tests.
    static func fromLaunchArguments(
        _ arguments: [String] = ProcessInfo.processInfo.arguments,
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> EngineMode {
        #if DEBUG
            if isUnitTestHost(environment) {
                return .test(TestEngineOptions(clock: .stepped, appSupport: TestEngineOptions.freshFolder()))
            }
            guard value(of: "-OpenLabEngine", in: arguments) == "test" else { return .real }
            let clock = value(of: "-OpenLabVirtualDevice", in: arguments).flatMap(TestEngineOptions.Clock.init)
            let support = value(of: "-OpenLabAppSupport", in: arguments).map { URL(fileURLWithPath: $0) }
            var open: [String] = []
            for (i, a) in arguments.enumerated() where a == "-OpenLabOpen" && i + 1 < arguments.count {
                open.append(arguments[i + 1])
            }
            return .test(
                TestEngineOptions(
                    clock: clock ?? .realtime, appSupport: support ?? TestEngineOptions.freshFolder(), open: open))
        #else
            return .real
        #endif
    }

    private static func value(of flag: String, in arguments: [String]) -> String? {
        guard let i = arguments.firstIndex(of: flag), i + 1 < arguments.count else { return nil }
        return arguments[i + 1]
    }

    /// The app is the host process of the unit tests (XCTest sets this for it).
    static func isUnitTestHost(_ environment: [String: String] = ProcessInfo.processInfo.environment) -> Bool {
        environment["XCTestConfigurationFilePath"] != nil || environment["XCTestBundlePath"] != nil
    }

    @MainActor
    func make() -> (EngineSession, EngineSupervisor) {
        let processes: SystemEngineProcesses
        switch self {
        case .real:
            processes = SystemEngineProcesses()
        #if DEBUG
            case .test(let options):
                processes = SystemEngineProcesses(
                    infoURL: options.appSupport.appendingPathComponent("openLab/engine.json"),
                    arguments: options.engineArguments, firstSpawnArguments: options.openArguments, watchStdin: true)
        #endif
        }
        let session = EngineSession(transport: WebSocketTransport(), infoProvider: { processes.readInfo() })
        return (session, EngineSupervisor(processes: processes, session: session))
    }
}

#if DEBUG
    /// The test engine's launch: its clock, its Application Support folder and the shows it opens
    /// when first started (not when restarted after a crash: the app reopens its workspaces then).
    struct TestEngineOptions: Equatable {
        enum Clock: String {
            /// Device time moves only on an `advance` request (unit tests).
            case stepped
            /// Device time follows the system clock (UI tests).
            case realtime
        }

        var clock: Clock
        var appSupport: URL
        var open: [String] = []

        var engineArguments: [String] {
            ["--virtual-device", clock.rawValue, "--app-support", appSupport.path, "--watch-stdin"]
        }

        var openArguments: [String] { open.flatMap { ["--open", $0] } }

        /// A new, empty folder under the temporary directory.
        static func freshFolder() -> URL {
            let url = FileManager.default.temporaryDirectory.appendingPathComponent(
                "openlab-test-engine-\(UUID().uuidString)", isDirectory: true)
            try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
            return url
        }
    }
#endif
