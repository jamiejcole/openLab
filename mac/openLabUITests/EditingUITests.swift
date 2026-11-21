// SPDX-License-Identifier: GPL-3.0-or-later

import XCTest

/// Add, rename, delete and undo on the real engine, and the same keys doing nothing in Show Mode.
@MainActor
final class EditingUITests: XCTestCase {
    func testAddRenameDeleteAndUndo() {
        let app = launchDemo()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        app.staticTexts["Intro"].click()

        app.typeKey("0", modifierFlags: .command)  // New Group Cue (QLab's key)
        let name = app.textFields["inspector-name"]
        XCTAssertTrue(name.waitForExistence(timeout: 5))
        // The new cue is selected and shown in the inspector: its name is empty.
        XCTAssertEqual(name.value as? String ?? "", "")
        name.click()
        name.typeText("Storm\r")
        let storm = app.staticTexts["Storm"]
        XCTAssertTrue(storm.waitForExistence(timeout: 5))

        storm.click()  // focus back in the list
        app.typeKey(XCUIKeyboardKey.delete.rawValue, modifierFlags: .command)
        XCTAssertTrue(waitFor("exists == false", storm), "Cmd-Delete removes the selected cue")

        app.typeKey("z", modifierFlags: .command)
        XCTAssertTrue(app.staticTexts["Storm"].waitForExistence(timeout: 10), "undo brings the cue back")
    }

    func testCopyAndPasteDuplicatesACue() {
        let app = launchDemo()
        // Rows of the list only: the standby display above it names the playhead's cue too.
        let intro = app.outlines.matching(identifier: "cue-list").outlineRows.containing(
            NSPredicate(format: "label == 'Intro' OR value == 'Intro'"))
        XCTAssertTrue(waitFor("count == 1", intro))
        app.staticTexts["Intro"].firstMatch.click()
        app.typeKey("c", modifierFlags: .command)
        let copied = NSPredicate { _, _ in NSPasteboard.general.types?.contains(.init("dev.openlab.cue-data")) == true }
        XCTAssertEqual(
            XCTWaiter().wait(for: [XCTNSPredicateExpectation(predicate: copied, object: nil)], timeout: 10), .completed)
        app.typeKey("v", modifierFlags: .command)
        XCTAssertTrue(waitFor("count == 2", intro), "the pasted copy shows in the list")
    }

    func testEditingKeysDoNothingInShowMode() {
        let app = launchDemo()
        XCTAssertTrue(app.staticTexts["Intro"].waitForExistence(timeout: 10))
        app.staticTexts["Intro"].click()
        app.radioGroups["mode-switch"].radioButtons["Show"].click()
        XCTAssertTrue(
            waitFor("exists == false", app.textFields["inspector-name"], timeout: 5), "Show Mode hides the inspector")
        app.typeKey("0", modifierFlags: .command)
        app.typeKey(XCUIKeyboardKey.delete.rawValue, modifierFlags: .command)
        app.typeKey("z", modifierFlags: .command)
        XCTAssertFalse(app.staticTexts["Group"].waitForExistence(timeout: 2), "no cue is added in Show Mode")
        XCTAssertTrue(app.staticTexts["Intro"].exists, "nothing was deleted in Show Mode")
    }

    /// Tools > Find Missing Files: the operator points it at one folder, and every missing file
    /// there is found with it and relinked together (QLab 5 manual, missing files). Door slam's and
    /// Rain's files were moved into another folder, which also holds Radio's.
    func testFindMissingFilesLocatesOneFolderAndTheOtherFilesThereFollow() {
        var folder = URL(fileURLWithPath: "/")
        let app = launchApp(opening: ["Demo"]) { copies in
            folder = copies.appendingPathComponent("Moved media", isDirectory: true)
            let fm = FileManager.default
            try fm.createDirectory(at: folder, withIntermediateDirectories: true)
            for name in ["d.wav", "r.wav"] {
                try fm.moveItem(
                    at: copies.appendingPathComponent("Demo/media/\(name)"), to: folder.appendingPathComponent(name))
            }
            try writeWAV(folder.appendingPathComponent("o.wav"), seconds: 1)
        }
        XCTAssertTrue(waitFor("isHittable == true", app.staticTexts["Intro"]), "the demo is ready")
        let cues = ["Door slam", "Rain", "Radio"]
        for cue in cues {
            XCTAssertTrue(cueState("Broken", of: cue, in: app).waitForExistence(timeout: 10), "\(cue) misses its file")
        }

        app.menuBars.menuBarItems["Tools"].click()
        app.menuItems["Find Missing Files\u{2026}"].click()
        let panel = app.dialogs.firstMatch
        XCTAssertTrue(panel.waitForExistence(timeout: 10), "a folder is asked for")
        app.typeKey("g", modifierFlags: [.command, .shift])
        app.typeText(folder.path + "\r")
        app.typeKey(XCUIKeyboardKey.return.rawValue, modifierFlags: [])  // Search
        XCTAssertTrue(waitFor("exists == false", panel), "the panel closes")

        let found = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH 'missing-files-use-'"))
        XCTAssertTrue(waitFor("count == 3", found), "the three files are found in the one folder")
        for i in 0..<3 { XCTAssertEqual(found.element(boundBy: i).value as? Int, 1, "row \(i) is ticked") }
        app.buttons["missing-files-relink"].click()
        for cue in cues {
            XCTAssertTrue(waitFor("exists == false", cueState("Broken", of: cue, in: app)), "\(cue) has its file again")
        }
        XCTAssertEqual(cueStates("Broken", in: app).count, 0, "nothing is broken")
        XCTAssertTrue(waitFor("count == 0", found), "and the tool lists none")
    }
}
