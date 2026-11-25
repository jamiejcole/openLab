// SPDX-License-Identifier: GPL-3.0-or-later

import Darwin
import XCTest

@testable import openLab

/// A blocking OSC-over-TCP client for the engine's listener (SLIP framing, OSC 1.1): what a console
/// on the network does, so the tests see what a peer sees of the settings page's switch and port.
final class OscTcpClient {
    private var fd: Int32 = -1

    /// nil when nothing listens on `port` of the loopback.
    init?(port: UInt16) {
        fd = socket(AF_INET, SOCK_STREAM, 0)
        var addr = sockaddr_in()
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = port.bigEndian
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")
        let result = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard result == 0 else {
            close(fd)
            return nil
        }
        var timeout = timeval(tv_sec: 2, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
    }

    deinit { if fd >= 0 { close(fd) } }

    private func padded(_ text: String) -> [UInt8] {
        var bytes = Array(text.utf8) + [0]
        while bytes.count % 4 != 0 { bytes.append(0) }
        return bytes
    }

    /// Sends `address` with no arguments as one SLIP frame.
    func send(_ address: String) {
        let message = padded(address) + padded(",")
        var frame: [UInt8] = [0xC0]
        for byte in message {
            switch byte {
            case 0xC0: frame += [0xDB, 0xDC]
            case 0xDB: frame += [0xDB, 0xDD]
            default: frame.append(byte)
            }
        }
        frame.append(0xC0)
        _ = frame.withUnsafeBytes { write(fd, $0.baseAddress, frame.count) }
    }

    /// The reply's text (its JSON body shows in it), nil when none came within two seconds.
    func reply() -> String? {
        var buffer = [UInt8](repeating: 0, count: 65536)
        let n = read(fd, &buffer, buffer.count)
        return n > 0 ? String(decoding: buffer[0..<n], as: UTF8.self) : nil
    }
}

/// A socket of `kind` bound to `port` of `address` (by default all of this Mac's); nil when another
/// socket has it.
private func boundSocket(_ kind: Int32, _ port: UInt16, address: String = "0.0.0.0") -> Int32? {
    let fd = socket(AF_INET, kind, 0)
    var addr = sockaddr_in()
    addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
    addr.sin_family = sa_family_t(AF_INET)
    addr.sin_port = port.bigEndian
    addr.sin_addr.s_addr = inet_addr(address)
    let bound = withUnsafePointer(to: &addr) {
        $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            Darwin.bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
        }
    }
    if bound == 0 { return fd }
    close(fd)
    return nil
}

/// The last port `freeOscPort` gave out.
@MainActor private var lastOscPort = UInt16(20_000 + Int(getpid()) % 10_000)

/// A port nothing has, for TCP and UDP both, and never one given out before. It is below the range the
/// system gives ports out of on its own (49152 and up; UDP ones at random): a port of that range, free a
/// moment ago, can be another program's by the time the engine opens it.
@MainActor
func freeOscPort() -> UInt16 {
    while true {
        lastOscPort = lastOscPort < 45_000 ? lastOscPort + 1 : 20_000
        guard let udp = boundSocket(SOCK_DGRAM, lastOscPort) else { continue }
        defer { close(udp) }
        guard let tcp = boundSocket(SOCK_STREAM, lastOscPort) else { continue }
        close(tcp)
        return lastOscPort
    }
}

/// NET-01: the OSC page of the workspace settings against the real engine with its OSC listeners
/// open on a port of its own (its machine settings name one). Each test is a flow of the page: what
/// it shows of the listening port, the switch that stops all OSC control, and a port that cannot
/// be opened.
@MainActor
final class SettingsOscTests: XCTestCase {
    typealias H = CueListHarness
    var engine: TestEngine!
    var port: UInt16 = 0

    /// Types `text` in the port field, leaves it and waits for the engine's answer.
    private func commitPort(_ text: String, _ sc: WorkspaceSettingsWindowController) async throws {
        sc.window?.makeFirstResponder(sc.oscPortField)
        try XCTUnwrap(sc.oscPortField.currentEditor()).string = text
        sc.window?.makeFirstResponder(nil)
        await sc.edits.answered()
    }

    override func setUp() async throws {
        port = freeOscPort()
        let settings = Data(#"{"settings_version":1,"osc_port":\#(port)}"#.utf8)
        engine = try await TestEngine.start(settings: settings)
    }

    override func tearDown() async throws { engine?.stop() }

    private func open() async throws -> (Rig, WorkspaceSettingsWindowController) {
        let rig = try await Rig.open("Demo", on: engine)
        let sc = WorkspaceSettingsWindowController(store: rig.store)
        sc.model.localAddresses = { ["192.168.1.20", "10.0.0.7"] }
        addTeardownBlock { @MainActor in sc.close() }
        await sc.present().value
        sc.show(page: .osc)
        return (rig, sc)
    }

    /// Sends a query of cue 1 over `client` until the engine's answer says `status` (the switch's change
    /// reaches the OSC side after the model change), up to a bounded number of tries.
    private func waitForStatus(_ status: String, _ client: OscTcpClient, _ rig: Rig) async throws {
        // Cue 1 exists in the Demo show; the property changes so no probe repeats the one before
        // it (an identical message within 100 ms is dropped unanswered).
        let properties = ["name", "notes", "flagged", "armed", "preWait", "postWait"]
        for n in 0..<200 {
            client.send("/cue/1/\(properties[n % properties.count])")
            if client.reply()?.contains("\"status\":\"\(status)\"") == true { return }
            try await rig.advance(0.01)
        }
        XCTFail("no \(status) answer")
    }

    /// The port the tests give the engine is one it can open: a port another program's UDP socket holds
    /// (TCP free) is not given out.
    func testAFreePortIsFreeForUdpToo() throws {
        let first = freeOscPort()
        let held = ((first + 1)...(first + 8)).compactMap { boundSocket(SOCK_DGRAM, $0, address: "127.0.0.1") }
        defer { for fd in held { close(fd) } }
        let given = freeOscPort()
        XCTAssertFalse(((first + 1)...(first + 8)).contains(given), "\(given) is held for UDP")
    }

    /// Checklist: OSC page → the listening port and whether it is open, this Mac's addresses and a
    /// switch that stops all OSC control.
    func testTheOscPageShowsThePortWhetherItIsOpenThisMacsAddressesAndTheSwitch() async throws {
        let (_, sc) = try await open()
        XCTAssertEqual(sc.oscPortField.stringValue, String(port))
        XCTAssertEqual(sc.oscPortStatus.stringValue, String(format: loc("settings.osc.open"), Int(port)))
        XCTAssertEqual(
            sc.oscAddressesLabel.stringValue, String(format: loc("settings.osc.addresses"), "192.168.1.20, 10.0.0.7"))
        XCTAssertEqual(sc.oscEnabledCheck.state, .on)
        XCTAssertNotNil(OscTcpClient(port: port), "the port is open")
        // Switched off, the page says control is off (the port is still bound), not "listening".
        sc.oscEnabledCheck.performClick(nil)
        await assertEventually("the page says OSC control is off") {
            sc.oscPortStatus.stringValue == String(format: loc("settings.osc.offOpen"), Int(self.port))
        }
    }

    /// Checklist: with the switch off an incoming GO does nothing (and the app's own engine
    /// connection is not affected); switched on again it works.
    func testWithTheSwitchOffAnIncomingGoDoesNothing() async throws {
        let (rig, sc) = try await open()
        let head: () -> ModelID? = { rig.store.playheads[ModelID(H.list)] }
        let client = try XCTUnwrap(OscTcpClient(port: port))
        XCTAssertEqual(head(), H.intro)
        client.send("/go")
        await assertEventually("the GO over OSC moved the playhead") {
            try? await rig.advance(0.02)
            return head() == H.fade
        }
        sc.oscEnabledCheck.performClick(nil)
        await assertEventually { rig.store.isOn(.oscEnabled) == false }
        try await waitForStatus("denied", client, rig)
        client.send("/go")
        client.send("/cue/99/name")
        XCTAssertTrue(client.reply()?.contains("denied") == true)
        try await rig.advance(0.1)
        XCTAssertEqual(head(), H.fade, "an incoming GO did nothing")
        // The app's own connection still works with OSC refused.
        guard case .success = await rig.store.transport(.setPlayhead(list: ModelID(H.list), cue: H.intro)) else {
            return XCTFail("the app's engine connection was affected")
        }
        await assertEventually("the app moved the playhead") { head() == H.intro }
        sc.oscEnabledCheck.performClick(nil)
        await assertEventually { rig.store.isOn(.oscEnabled) == true }
        try await waitForStatus("ok", client, rig)
        client.send("/go")
        await assertEventually("on again: the GO works") {
            try? await rig.advance(0.02)
            return head() == H.fade
        }
    }

    /// The port typed on the page is opened, the old one closed.
    func testChangingThePortOpensTheNewOneAndClosesTheOldOne() async throws {
        let (_, sc) = try await open()
        let new = freeOscPort()
        try await commitPort(String(new), sc)
        XCTAssertEqual(sc.messageLabel.stringValue, "", "not refused")
        XCTAssertEqual(sc.oscPortStatus.stringValue, String(format: loc("settings.osc.open"), Int(new)))
        XCTAssertEqual(sc.oscPortField.stringValue, String(new))
        XCTAssertNotNil(OscTcpClient(port: new))
        await assertEventually("the old port is closed") { OscTcpClient(port: self.port) == nil }
    }

    /// A port typed and left there is opened when the window goes with Done.
    func testAPortTypedIsOpenedWhenTheWindowGoes() async throws {
        let (_, sc) = try await open()
        let new = freeOscPort()
        sc.window?.makeFirstResponder(sc.oscPortField)
        sc.oscPortField.currentEditor()?.string = String(new)
        sc.doneButton.performClick(nil)
        XCTAssertFalse(sc.window?.isVisible == true)
        await sc.edits.answered()
        XCTAssertNil(sc.model.message, "not refused")
        XCTAssertEqual(sc.model.oscListening, OscListening(port: new, open: true, error: nil))
        XCTAssertFalse(sc.window?.isVisible == true)
    }

    /// Checklist: the listening port is changed to one in use → the page says the port could not be
    /// opened and the old one is not left half open (it still serves, and the failed port has no
    /// socket of ours).
    func testAPortInUseIsRefusedWithItsReasonAndTheOldPortKeepsServing() async throws {
        let (_, sc) = try await open()
        // Another program holds a port (TCP only: the UDP bind that comes first must be undone).
        let busy = freeOscPort()
        let tcp = try XCTUnwrap(boundSocket(SOCK_STREAM, busy, address: "127.0.0.1"))
        defer { close(tcp) }
        listen(tcp, 1)
        try await commitPort(String(busy), sc)
        XCTAssertTrue(
            sc.oscPortStatus.stringValue.contains("could not be opened"),
            "the page says it could not be opened: \(sc.oscPortStatus.stringValue)")
        let udp = boundSocket(SOCK_DGRAM, busy, address: "127.0.0.1")
        XCTAssertNotNil(udp, "no UDP socket of the engine's is left on it")
        udp.map { _ = close($0) }
        XCTAssertTrue(sc.messageLabel.stringValue.contains(String(busy)), "the refusal names the port")
        XCTAssertEqual(sc.oscPortField.stringValue, String(port), "the field shows the port that listens")
        XCTAssertNotNil(OscTcpClient(port: port), "the old port keeps serving")
        let client = try XCTUnwrap(OscTcpClient(port: port))
        client.send("/version")
        XCTAssertNotNil(client.reply(), "and answers")
        // A port that is not a number is refused without asking the engine.
        try await commitPort("http", sc)
        XCTAssertEqual(sc.messageLabel.stringValue, loc("settings.osc.badPort"))
        XCTAssertEqual(sc.oscPortField.stringValue, String(port))
    }
}
