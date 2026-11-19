// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import XCTest

@testable import openLab

/// #708: every question the app asks stands the stage on the display with the menu bar back while it
/// is up (`StandBack`), and only then.
@MainActor
final class StandBackTests: XCTestCase {
    private var told: [Bool] = []
    private var before: (@MainActor (Bool) -> Void)?

    override func setUp() async throws {
        before = StandBack.shared.changed
        StandBack.shared.changed = { [weak self] in self?.told.append($0) }
    }

    override func tearDown() async throws {
        StandBack.shared.changed = before
    }

    /// A real alert's modal run: standing back from before it shows until after it is answered, told
    /// once each way.
    func testARealAlertStandsTheStageBackWhileItIsUp() {
        let alert = NSAlert()
        alert.messageText = "Question"
        alert.addButton(withTitle: "OK")
        var askingWhileUp: Bool?
        DispatchQueue.main.async {
            askingWhileUp = StandBack.shared.isAsking
            NSApp.stopModal(withCode: .alertFirstButtonReturn)
        }
        let answer = StandBack.runModal(alert)
        XCTAssertEqual(answer, .alertFirstButtonReturn)
        XCTAssertEqual(askingWhileUp, true)
        XCTAssertFalse(StandBack.shared.isAsking)
        XCTAssertEqual(told, [true, false])
    }

    /// A sheet: standing back until it ends, Cancel included; questions on top of each other are one
    /// stand-back.
    func testASheetStandsTheStageBackUntilItEndsAndNestedQuestionsAreOne() {
        let window = NSWindow(
            contentRect: NSRect(x: -30000, y: -30000, width: 400, height: 300), styleMask: [.titled],
            backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        defer { window.close() }
        let alert = NSAlert()
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "Cancel")
        var answered: NSApplication.ModalResponse?
        StandBack.beginSheet(alert, for: window) { answered = $0 }
        XCTAssertTrue(StandBack.shared.isAsking)
        StandingBackAlerts(base: ScriptedAlerts()).showError("and another")
        XCTAssertTrue(StandBack.shared.isAsking, "still the sheet")
        window.endSheet(alert.window, returnCode: .alertSecondButtonReturn)
        XCTAssertEqual(answered, .alertSecondButtonReturn)
        XCTAssertFalse(StandBack.shared.isAsking)
        XCTAssertEqual(told, [true, false])
    }

    /// What Esc does on the app's alerts: AppKit gives Esc to a button titled Cancel; an alert with
    /// only OK has no Esc button, so there Esc answers nothing (the app's key monitor still panics
    /// first, as everywhere). Return answers OK.
    func testEscOnAnOKOnlyAlertAnswersNothingAndOnACancelButtonCancels() {
        let ok = NSAlert()
        ok.addButton(withTitle: loc("alert.ok"))
        ok.layout()
        XCTAssertEqual(ok.buttons.map(\.keyEquivalent), ["\r"])
        let cancel = NSAlert()
        cancel.addButton(withTitle: loc("alert.save"))
        cancel.addButton(withTitle: loc("alert.cancel"))
        cancel.layout()
        XCTAssertEqual(cancel.buttons.map(\.keyEquivalent), ["\r", "\u{1b}"])
    }
}
