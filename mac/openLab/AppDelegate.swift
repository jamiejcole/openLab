// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

@main
final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuItemValidation, NSMenuDelegate {
    /// `-OpenLabEngine fake` runs the in-process fake engine (UI tests); default is the real one.
    @MainActor private(set) lazy var engine = launch.makeEngine()
    @MainActor var engineSession: EngineSession { engine.0 }
    @MainActor private(set) lazy var appController = AppController(session: engineSession, supervisor: engine.1)
    @MainActor private(set) lazy var alarmController = EngineAlarmController(
        session: engineSession, supervisor: engine.1, presenter: EngineAlarmPanel(),
        alerts: { [unowned self] in appController.alerts })

    /// The system pieces launching uses (tests inject them).
    private let launch: AppLaunch

    @MainActor override init() {
        launch = .live
        super.init()
    }

    @MainActor init(launch: AppLaunch) {
        self.launch = launch
        super.init()
    }

    static func main() {
        let app = NSApplication.shared
        let delegate = AppDelegate()
        app.delegate = delegate
        app.run()
    }

    /// Another copy of the app (same bundle id) was already running: this one hands over to it
    /// and exits, touching neither `engine` nor `appController`.
    @MainActor private var firstCopy: RunningCopy?
    @MainActor var isSecondInstance: Bool { firstCopy != nil }

    func applicationWillFinishLaunching(_ notification: Notification) {
        // A second copy (another download, `open -n`) would attach to the same engine and its
        // Quit would shut down every show the first one runs: it fronts the first and exits.
        if let first = launch.runningFirst() {
            firstCopy = first
            engineLog.notice("openLab is already running (pid \(first.processIdentifier, privacy: .public)): exiting")
            return
        }
        launch.installMenu(self)
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        if let first = firstCopy {
            // Now this copy is the active app: it can let the first one in (macOS 14+).
            launch.activation.bringForward(first)
            return launch.terminate()
        }
        // The launcher shows until the engine reports a workspace (fake engine: at once).
        appController.showLauncher()
        appController.installKeyMonitor()
        _ = alarmController
        // Finds the engine (or starts one) and connects; the engine opens nothing by itself.
        if !EngineMode.isUnitTestHost() { engine.1.start() }
        NSApp.activate(ignoringOtherApps: true)
    }

    /// A show must survive an accidental window close: never quit, and bring a window back.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if isSecondInstance { return false }  // exiting: never builds the session
        appController.reopen()
        return false
    }

    /// One save prompt per dirty workspace, then `goodbye { shutdown }`; Cancel aborts the quit.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if isSecondInstance { return .terminateNow }  // it never started or reached an engine
        if appController.quitting { return .terminateCancel }
        Task { @MainActor in
            let ok = await appController.prepareToQuit()
            NSApp.reply(toApplicationShouldTerminate: ok)
        }
        return .terminateLater
    }

    @MainActor @objc func newWorkspace(_ sender: Any?) { Task { await appController.newWorkspace() } }
    @MainActor @objc func openWorkspace(_ sender: Any?) { Task { await appController.openWorkspace() } }
    @MainActor @objc func saveWorkspace(_ sender: Any?) {
        guard let store = appController.frontController?.store else { return }
        Task { await appController.saveWorkspace(store) }
    }
    @MainActor @objc func saveWorkspaceAs(_ sender: Any?) {
        guard let store = appController.frontController?.store else { return }
        Task { await appController.saveWorkspace(store, asNew: true) }
    }
    @MainActor @objc func openRecent(_ sender: NSMenuItem) {
        guard let url = sender.representedObject as? URL else { return }
        Task { await appController.openWorkspace(url: url) }
    }
    @MainActor @objc func clearRecents(_ sender: Any?) { appController.clearRecents() }
    @MainActor @objc func recoverAutosaves(_ sender: Any?) { Task { await alarmController.recoverAutosaves() } }

    @MainActor @objc func enterShowMode(_ sender: Any?) {
        Task { await appController.frontController?.requestShowMode(true) }
    }
    @MainActor @objc func enterEditMode(_ sender: Any?) {
        Task { await appController.frontController?.requestShowMode(false) }
    }

    // MARK: Cue editing (all refuse in Show Mode; `validateMenuItem` greys them)

    /// The editor of the key window's workspace.
    @MainActor private var editor: CueEditor? { appController.frontController?.editor }

    /// Text being edited (a field editor) keeps its own Undo and Cmd-Delete.
    @MainActor private var textEditor: NSText? {
        (NSApp.keyWindow?.firstResponder as? NSText).flatMap { $0.isEditable ? $0 : nil }
    }

    /// Any text in focus, editable or not, owns Cut, Copy and Paste.
    @MainActor private var anyText: NSText? { NSApp.keyWindow?.firstResponder as? NSText }

    @MainActor @objc func newCue(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String, let kind = NewCueKind(rawValue: raw) else { return }
        Task { await editor?.add(kind) }
    }
    @MainActor @objc func deleteCues(_ sender: Any?) { Task { await editor?.deleteSelection() } }
    @MainActor @objc func deleteNumbers(_ sender: Any?) { Task { await editor?.deleteNumbers() } }
    /// Cut, Copy and Paste: text being edited keeps them; otherwise they act on the cues.
    @MainActor @objc func copyCues(_ sender: Any?) {
        if let text = anyText { return text.copy(sender) }
        Task { await editor?.copySelection() }
    }
    @MainActor @objc func cutCues(_ sender: Any?) {
        if let text = anyText { return text.cut(sender) }
        Task { await editor?.cutSelection() }
    }
    @MainActor @objc func pasteCues(_ sender: Any?) {
        if let text = anyText { return text.paste(sender) }
        Task { await editor?.paste() }
    }
    @MainActor @objc func renumberCues(_ sender: Any?) { appController.frontController?.promptRenumber() }
    @MainActor @objc func moveCuesUp(_ sender: Any?) { Task { await editor?.moveSelection(by: -1) } }
    @MainActor @objc func moveCuesDown(_ sender: Any?) { Task { await editor?.moveSelection(by: 1) } }
    @MainActor @objc func selectNextCue(_ sender: Any?) { appController.selectAdjacentCue(by: 1) }
    @MainActor @objc func selectPreviousCue(_ sender: Any?) { appController.selectAdjacentCue(by: -1) }
    @MainActor @objc func undoEdit(_ sender: Any?) {
        switch CueEditor.textUndo(textEditor, redo: false) {
        case nil: Task { await editor?.undo() }
        case false?: NSSound.beep()
        case true?: break
        }
    }
    @MainActor @objc func redoEdit(_ sender: Any?) {
        switch CueEditor.textUndo(textEditor, redo: true) {
        case nil: Task { await editor?.redo() }
        case false?: NSSound.beep()
        case true?: break
        }
    }

    @MainActor @objc func showLog(_ sender: Any?) { appController.showLog() }
    @MainActor @objc func revealCrashReports(_ sender: Any?) { appController.revealCrashReports() }
    @MainActor @objc func showWorkspaceSettings(_ sender: Any?) { appController.frontController?.showSettings() }
    @MainActor @objc func showPreshowCheck(_ sender: Any?) { appController.frontController?.showPreshowCheck() }

    @MainActor @objc func relinkMedia(_ sender: Any?) {
        guard let store = appController.frontController?.store else { return }
        Task { await appController.relinkMedia(on: store) }
    }

    @MainActor @objc func transportCommand(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String, let command = TransportCommand(rawValue: raw) else {
            return
        }
        // isARepeat raises on non-key events (a menu click).
        let event = NSApp.currentEvent
        let isRepeat = event?.type == .keyDown && event?.isARepeat == true
        Task { await appController.perform(command, isRepeat: isRepeat) }
    }

    /// A workspace window is in front and not in Show Mode: the cue-editing items work.
    @MainActor private var canEditFront: Bool { appController.frontController.map { !$0.isShowMode } ?? false }

    @MainActor @objc func validateMenuItem(_ item: NSMenuItem) -> Bool {
        switch item.action {
        case #selector(enterShowMode(_:))?, #selector(enterEditMode(_:))?:
            guard let front = appController.frontController else { return false }
            return front.isShowMode == (item.action == #selector(enterEditMode(_:)))
        case #selector(newCue(_:))?, #selector(renumberCues(_:))?, #selector(moveCuesUp(_:))?,
            #selector(moveCuesDown(_:))?, #selector(relinkMedia(_:))?:
            return canEditFront
        case #selector(copyCues(_:))?, #selector(cutCues(_:))?, #selector(pasteCues(_:))?:
            return anyText != nil || canEditFront
        case #selector(selectNextCue(_:))?, #selector(selectPreviousCue(_:))?:
            // Not while a text field is edited: Cmd-Up / Down move its insertion point.
            return appController.frontController != nil && !appController.isTextEditing()
        case #selector(deleteNumbers(_:))?, #selector(deleteCues(_:))?:
            return textEditor == nil && canEditFront
        case #selector(undoEdit(_:))?: return textEditor != nil || (editor?.canUndo ?? false)
        case #selector(redoEdit(_:))?: return textEditor != nil || (editor?.canRedo ?? false)
        case #selector(saveWorkspace(_:))?, #selector(saveWorkspaceAs(_:))?:
            return appController.canSaveFront
        case #selector(recoverAutosaves(_:))?: return engineSession.connectionState == .connected
        case #selector(showLog(_:))?: return appController.frontController != nil
        case #selector(showWorkspaceSettings(_:))?: return appController.frontController != nil
        case #selector(showPreshowCheck(_:))?: return appController.frontController?.canRunPreshowCheck ?? false
        default: break
        }
        guard let raw = item.representedObject as? String, let command = TransportCommand(rawValue: raw) else {
            return true
        }
        return appController.canPerform(command)
    }

    /// The Transport menu shows the key window's workspace keys.
    @MainActor func menuNeedsUpdate(_ menu: NSMenu) {
        if menu.title == NSLocalizedString("menu.file.openRecent", comment: "") {
            MainMenu.fillRecent(menu, appController.existingRecentURLs())
            return
        }
        // Show Mode (SM-01): the editing items go dark in the front window's Show Mode.
        if menu.title == NSLocalizedString("menu.edit", comment: "") {
            MainMenu.lockEditItems(in: menu, locked: appController.frontController?.isShowMode == true)
            return
        }
        MainMenu.applyKeys(to: menu, appController.frontKeys)
    }
}

/// Which copy of the app runs: the one launched first. Every copy decides alike, so two
/// launched at once never both stay (or both exit).
enum SingleInstance {
    struct Instance: Equatable {
        var pid: pid_t
        var launched: Date?
    }

    /// The instance among `others` this one (`me`) must hand over to, if any: the earliest
    /// launched, a lower pid breaking a tie (or standing in for an unknown launch date).
    static func handOver(me: Instance, others: [Instance]) -> Instance? {
        func first(_ a: Instance, _ b: Instance) -> Bool {
            if let x = a.launched, let y = b.launched, x != y { return x < y }
            return a.pid < b.pid
        }
        return others.filter { $0.pid != me.pid && first($0, me) }.min(by: first)
    }

    /// The running copy of this app that was there first, or nil when this one is it.
    @MainActor
    static func runningFirst() -> NSRunningApplication? {
        let current = NSRunningApplication.current
        guard let id = Bundle.main.bundleIdentifier else { return nil }
        let apps = NSRunningApplication.runningApplications(withBundleIdentifier: id).filter { !$0.isTerminated }
        let me = Instance(pid: current.processIdentifier, launched: current.launchDate)
        let others = apps.map { Instance(pid: $0.processIdentifier, launched: $0.launchDate) }
        guard let pick = handOver(me: me, others: others) else { return nil }
        return apps.first { $0.processIdentifier == pick.pid }
    }
}
