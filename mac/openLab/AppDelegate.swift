// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

@main
final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuItemValidation, NSMenuDelegate {
    /// The shipping engine, or in Debug builds the test engine (`EngineMode`).
    @MainActor private(set) lazy var engine = launch.makeEngine()
    @MainActor var engineSession: EngineSession { engine.0 }
    @MainActor lazy var appController = AppController(
        session: engineSession, supervisor: engine.1, launchDefaults: launch.launchDefaults,
        layoutDefaults: Self.layoutDefaults)

    /// Where windows remember their layout: the user's settings, except under the test engine (Debug
    /// builds), which must not fill or reorder the real ones.
    @MainActor private static var layoutDefaults: UserDefaults? {
        #if DEBUG
            if EngineMode.fromLaunchArguments() != .real { return UserDefaults(suiteName: "openlab.test-engine") }
        #endif
        return .standard
    }
    @MainActor private(set) lazy var alarmController = EngineAlarmController(
        session: engineSession, supervisor: engine.1, presenter: EngineAlarmPanel(),
        alerts: { [unowned self] in alarmAlerts })
    /// What the engine alarm raises errors and notices through: quiet in Show Mode (NFR-10).
    @MainActor var alarmAlerts: WorkspaceAlerts { appController.gatedAlerts }

    private var launchSub: AnyCancellable?

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
        ToolTipDelay.register()
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
        // The launcher shows until the engine reports a workspace.
        #if DEBUG
            if let recents = EngineMode.testRecents() { appController.recentURLs = { recents } }
        #endif
        appController.beginLaunch()
        appController.bindLauncher(to: engine.1)
        appController.isDisabledMenuChord = { [weak self] chord in self?.menuItemIsOff(for: chord) ?? false }
        appController.installKeyMonitor()
        _ = alarmController
        // The first time the engine is up: after its recovery check was asked, do what the launch
        // choice says. (Later runs are engine restarts, which reattach the windows.)
        launchSub = engine.1.ready.first().sink { [unowned self] in
            Task { @MainActor in
                await alarmController.offerTask?.value
                await appController.applyLaunchBehavior()
                alarmController.refreshOffer()
            }
        }
        // Finds the engine (or starts one) and connects; the engine opens nothing by itself.
        engine.1.start()
        NSApp.activate(ignoringOtherApps: true)
    }

    func application(_ application: NSApplication, open urls: [URL]) {
        if isSecondInstance { return }
        appController.openFiles(urls)
    }

    /// A show must survive an accidental window close: never quit, and bring a window back.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if isSecondInstance { return false }  // exiting: never builds the session
        appController.reopen()
        return false
    }

    /// A quit ended the modals and is waiting to be asked again.
    @MainActor private var quitPending = false
    /// Asks to quit again after a modal was ended for the quit (tests replace it: the real one
    /// would end the test host).
    @MainActor var terminateAgain: @MainActor () -> Void = { NSApp.terminate(nil) }

    /// One save prompt per dirty workspace, then `goodbye { shutdown }`; Cancel aborts the quit.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if isSecondInstance { return .terminateNow }  // it never started or reached an engine
        if appController.quitting { return .terminateCancel }
        // An open, save or other modal panel or alert is running its own loop, which never runs the
        // main queue the quit walk is queued on: ending it first (it answers as cancelled) lets the
        // walk run. Modals can be nested (the import's notes window, then its Save panel): a timer
        // that runs inside a modal loop ends each in turn until none is left, then the quit is
        // asked again once. A second Command-Q meanwhile only waits for that.
        if quitPending { return .terminateCancel }
        if NSApp.modalWindow != nil {
            quitPending = true
            NSApp.abortModal()
            nonisolated(unsafe) let delegate = self
            let timer = Timer(timeInterval: 0.05, repeats: true) { fired in
                nonisolated(unsafe) let fired = fired
                MainActor.assumeIsolated {
                    if NSApp.modalWindow != nil {
                        NSApp.abortModal()
                        return
                    }
                    fired.invalidate()
                    delegate.quitPending = false
                    delegate.terminateAgain()
                }
            }
            RunLoop.main.add(timer, forMode: .common)
            RunLoop.main.add(timer, forMode: .modalPanel)
            return .terminateCancel
        }
        Task { @MainActor in
            let ok = await appController.prepareToQuit()
            NSApp.reply(toApplicationShouldTerminate: ok)
        }
        return .terminateLater
    }

    @MainActor @objc func newWorkspace(_ sender: Any?) { Task { await appController.newWorkspace() } }
    @MainActor @objc func newWorkspaceFromQlab(_ sender: Any?) { Task { await appController.newWorkspaceFromQlab() } }
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
    @MainActor @objc func closeAllWorkspaces(_ sender: Any?) { Task { await appController.closeAllWorkspaces() } }
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

    /// Runs `edit` on the front window's editor with its selection as of now: the selection can move
    /// with the playhead before the Task runs, and the command is for what the operator saw.
    @MainActor private func onSelection(_ edit: @escaping @MainActor @Sendable (CueEditor, [ModelID]) async -> Void) {
        guard let front = appController.frontController else { return }
        let editor = front.editor
        let cues = front.cueList.selectedIDs()
        Task { await edit(editor, cues) }
    }

    /// Text being edited (a field editor) keeps its own Undo and Cmd-Delete.
    @MainActor private var textEditor: NSText? {
        (appController.keyWindow()?.firstResponder as? NSText).flatMap { $0.isEditable ? $0 : nil }
    }

    /// Any text in focus, editable or not, owns Cut, Copy and Paste.
    @MainActor private var anyText: NSText? { appController.keyWindow()?.firstResponder as? NSText }

    @MainActor @objc func newCue(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String, let kind = NewCueKind(rawValue: raw) else { return }
        onSelection { editor, cues in await editor.add(kind, after: cues) }
    }
    @MainActor @objc func deleteCues(_ sender: Any?) {
        onSelection { editor, cues in await editor.deleteSelection(of: cues) }
    }
    @MainActor @objc func deleteNumbers(_ sender: Any?) {
        onSelection { editor, cues in await editor.deleteNumbers(of: cues) }
    }
    /// Cut, Copy and Paste: text being edited keeps them; otherwise they act on the cues.
    @MainActor @objc func copyCues(_ sender: Any?) {
        if let text = anyText { return text.copy(sender) }
        onSelection { editor, cues in await editor.copySelection(of: cues) }
    }
    @MainActor @objc func cutCues(_ sender: Any?) {
        if let text = anyText { return text.cut(sender) }
        onSelection { editor, cues in await editor.cutSelection(of: cues) }
    }
    @MainActor @objc func pasteCues(_ sender: Any?) {
        if let text = anyText { return text.paste(sender) }
        onSelection { editor, cues in await editor.paste(after: cues) }
    }
    /// Edit > Paste Cue Properties (ED-06): the sheet for the selected cues.
    @MainActor @objc func pasteCueProperties(_ sender: Any?) {
        guard let front = appController.frontController else { return }
        front.promptPasteProperties(onto: front.cueList.selectedIDs())
    }
    /// Edit > Find (ED-09); Show Mode greys all three (`validateMenuItem`).
    @MainActor @objc func showFind(_ sender: Any?) { appController.frontController?.toggleFind() }
    @MainActor @objc func findNextCue(_ sender: Any?) { appController.frontController?.findNext(forward: true) }
    @MainActor @objc func findPreviousCue(_ sender: Any?) { appController.frontController?.findNext(forward: false) }
    @MainActor @objc func renumberCues(_ sender: Any?) { appController.frontController?.promptRenumber() }
    @MainActor @objc func moveCuesUp(_ sender: Any?) {
        onSelection { editor, cues in await editor.moveSelection(by: -1, of: cues) }
    }
    @MainActor @objc func moveCuesDown(_ sender: Any?) {
        onSelection { editor, cues in await editor.moveSelection(by: 1, of: cues) }
    }
    @MainActor @objc func toggleLayoutArea(_ sender: NSMenuItem) {
        guard let area = MainMenu.layoutArea(of: sender) else { return }
        appController.toggleLayout(area)
    }
    @MainActor @objc func selectCue(_ sender: Any?) { appController.frontController?.promptCue(movePlayhead: false) }
    @MainActor @objc func movePlayheadToCue(_ sender: Any?) {
        appController.frontController?.promptCue(movePlayhead: true)
    }
    @MainActor @objc func movePlayheadNext(_ sender: Any?) { appController.movePlayheadAdjacent(by: 1) }
    @MainActor @objc func movePlayheadPrevious(_ sender: Any?) { appController.movePlayheadAdjacent(by: -1) }
    @MainActor @objc func movePlayheadNextSequence(_ sender: Any?) {
        appController.frontController?.cueList.movePlayheadBySequence(1)
    }
    @MainActor @objc func movePlayheadPreviousSequence(_ sender: Any?) {
        appController.frontController?.cueList.movePlayheadBySequence(-1)
    }
    @MainActor @objc func toggleSidebarTab(_ sender: Any?) { appController.frontController?.toggleSidebarTab() }
    @MainActor @objc func toggleWorkspaceFullScreen(_ sender: Any?) {
        appController.frontController?.window?.toggleFullScreen(nil)
    }
    @MainActor @objc func showWorkspaceInFinder(_ sender: Any?) { appController.showFrontWorkspaceInFinder() }
    @MainActor @objc func copyMediaIntoShowFolder(_ sender: Any?) {
        appController.frontController?.copyMediaIntoShowFolder()
    }
    @MainActor @objc func pasteLevels(_ sender: Any?) { appController.frontController?.pasteLevels() }
    @MainActor @objc func selectNextInspectorTab(_ sender: Any?) { appController.selectAdjacentInspectorTab(by: 1) }
    @MainActor @objc func selectPreviousInspectorTab(_ sender: Any?) {
        appController.selectAdjacentInspectorTab(by: -1)
    }
    @MainActor @objc func loadToTime(_ sender: Any?) { appController.frontController?.toggleLoadToTime() }
    @MainActor @objc func revealTargetFile(_ sender: Any?) {
        appController.frontController?.cueList.revealSelectedTarget()
    }
    @MainActor @objc func jumpToTargets(_ sender: Any?) { appController.frontController?.jumpToTargets() }
    @MainActor @objc func showAbout(_ sender: Any?) { appController.aboutWindow.showWindow(nil) }
    @MainActor @objc func showPreferences(_ sender: Any?) {
        appController.preferencesWindow.reload()
        appController.preferencesWindow.showWindow(nil)
    }
    @MainActor @objc func showLauncherWindow(_ sender: Any?) { appController.showLauncher() }
    @MainActor @objc func selectNextCue(_ sender: Any?) { appController.selectAdjacentCue(by: 1) }
    @MainActor @objc func selectPreviousCue(_ sender: Any?) { appController.selectAdjacentCue(by: -1) }
    /// The editor whose workspace owns the key window, its settings window included.
    @MainActor private var undoEditor: CueEditor? { appController.undoController?.editor }

    @MainActor @objc func undoEdit(_ sender: Any?) {
        switch CueEditor.textUndo(textEditor, redo: false) {
        case nil: Task { await undoEditor?.undo() }
        case false?: NSSound.beep()
        case true?: break
        }
    }
    @MainActor @objc func redoEdit(_ sender: Any?) {
        switch CueEditor.textUndo(textEditor, redo: true) {
        case nil: Task { await undoEditor?.redo() }
        case false?: NSSound.beep()
        case true?: break
        }
    }

    @MainActor @objc func revealCrashReports(_ sender: Any?) { appController.revealCrashReports() }
    @MainActor @objc func showWorkspaceSettings(_ sender: Any?) { appController.frontController?.showSettings() }
    @MainActor @objc func showWorkspaceSettingsPage(_ sender: NSMenuItem) {
        let page = (sender.representedObject as? String).flatMap(SettingsPage.init)
        appController.frontController?.showSettings(page: page)
    }
    @MainActor @objc func showOverrideControls(_ sender: Any?) { appController.showOverrideControls() }
    @MainActor @objc func showWorkspaceStatus(_ sender: Any?) { appController.frontController?.showWorkspaceStatus() }

    @MainActor @objc func findMissingFiles(_ sender: Any?) {
        guard let store = appController.frontController?.store else { return }
        Task { await appController.findMissingFiles(on: store) }
    }

    @MainActor @objc func transportCommand(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String, let command = TransportCommand(rawValue: raw) else {
            return
        }
        // isARepeat raises on non-key events (a menu click).
        let event = NSApp.currentEvent
        let isRepeat = event?.type == .keyDown && event?.isARepeat == true
        appController.performNow(command, isRepeat: isRepeat)
    }

    /// A workspace window is in front and not in Show Mode: the cue-editing items work.
    @MainActor private var canEditFront: Bool { appController.frontController.map { !$0.isShowMode } ?? false }

    /// The front workspace's list has at least one cue selected: the items that act on the selection.
    @MainActor private var hasSelection: Bool {
        appController.frontController.map { !$0.cueList.selectedIDs().isEmpty } ?? false
    }

    /// The main menu has an item of this delegate's on `chord` and it is greyed out right now.
    @MainActor func menuItemIsOff(for chord: KeyChord) -> Bool {
        guard let menu = NSApp.mainMenu else { return false }
        func find(_ menu: NSMenu) -> NSMenuItem? {
            for item in menu.items {
                if let sub = item.submenu, let found = find(sub) { return found }
                guard !item.keyEquivalent.isEmpty, let action = item.action, responds(to: action),
                    item.target == nil || item.target === self,
                    !item.keyEquivalentModifierMask.contains(.function)
                else { continue }
                var mods = item.keyEquivalentModifierMask
                if item.keyEquivalent.lowercased() != item.keyEquivalent { mods.insert(.shift) }
                if KeyChord(item.keyEquivalent, mods) == chord { return item }
            }
            return nil
        }
        guard let item = find(menu) else { return false }
        return !validateMenuItem(item)
    }

    @MainActor @objc func validateMenuItem(_ item: NSMenuItem) -> Bool {
        // Show Mode (SM-01): every item of the locked-edit list is off, in whatever menu it sits.
        if MainMenu.LockedEdit(action: item.action) != nil, appController.frontController?.isShowMode == true {
            return false
        }
        switch item.action {
        case #selector(enterShowMode(_:))?, #selector(enterEditMode(_:))?:
            guard let front = appController.frontController else { return false }
            return front.isShowMode == (item.action == #selector(enterEditMode(_:)))
        case #selector(toggleLayoutArea(_:))?:
            guard let front = appController.frontController, let area = MainMenu.layoutArea(of: item) else {
                return false
            }
            item.state = front.isVisible(area) ? .on : .off
            // Show Mode has taken the toolbar and inspector away (SM-01): their items wait.
            return !(front.isShowMode && WorkspaceWindowController.hiddenInShowMode.contains(area))
        case #selector(selectCue(_:))?, #selector(movePlayheadToCue(_:))?, #selector(movePlayheadNext(_:))?,
            #selector(movePlayheadPrevious(_:))?, #selector(movePlayheadNextSequence(_:))?,
            #selector(movePlayheadPreviousSequence(_:))?:
            return appController.frontController != nil && !appController.isTextEditing()
        case #selector(toggleSidebarTab(_:))?: return appController.frontController != nil
        case #selector(toggleWorkspaceFullScreen(_:))?:
            guard let window = appController.frontController?.window else { return false }
            item.title = loc(
                window.styleMask.contains(.fullScreen) ? "menu.view.exitFullScreen" : "menu.view.enterFullScreen")
            return true
        case #selector(newWorkspaceFromQlab(_:))?: return !appController.qlabImportBlocked
        case #selector(showWorkspaceInFinder(_:))?: return appController.canShowFrontWorkspaceInFinder
        case #selector(copyMediaIntoShowFolder(_:))?: return appController.frontController?.canCopyMedia ?? false
        case #selector(pasteLevels(_:))?:
            return textEditor == nil && (appController.frontController?.canPasteLevels ?? false)
        case #selector(selectNextInspectorTab(_:))?, #selector(selectPreviousInspectorTab(_:))?:
            // Cmd-Left / Right move the insertion point of a text field being edited.
            guard let front = appController.frontController else { return false }
            return front.isVisible(.inspector) && !appController.isTextEditing()
        case #selector(loadToTime(_:))?:
            // An operator's tool: on in Show Mode too (QLab manual p. 149); it cycles the bar.
            return appController.frontController != nil
        case #selector(jumpToTargets(_:))?:
            // Not while a text field is edited: the jump would take its focus.
            return (appController.frontController?.canJumpToTargets ?? false) && !appController.isTextEditing()
        case #selector(revealTargetFile(_:))?:
            return appController.frontController?.cueList.canRevealSelectedTarget ?? false
        case #selector(findMissingFiles(_:))?: return appController.frontController?.canFindMissingFiles ?? false
        case #selector(closeAllWorkspaces(_:))?: return !appController.allWindows.isEmpty
        case #selector(showFind(_:))?, #selector(findNextCue(_:))?, #selector(findPreviousCue(_:))?:
            return canEditFront  // off in Show Mode, as in QLab
        case #selector(newCue(_:))?: return canEditFront
        case #selector(renumberCues(_:))?, #selector(moveCuesUp(_:))?, #selector(moveCuesDown(_:))?:
            return canEditFront && hasSelection
        case #selector(copyCues(_:))?, #selector(cutCues(_:))?:
            return anyText != nil || (canEditFront && hasSelection)
        case #selector(pasteCues(_:))?:
            return anyText != nil || canEditFront
        case #selector(selectNextCue(_:))?, #selector(selectPreviousCue(_:))?:
            // Not while a text field is edited: Cmd-Up / Down move its insertion point.
            return appController.frontController != nil && !appController.isTextEditing()
        case #selector(pasteCueProperties(_:))?:
            // A cue copied and cues to paste onto; not while text is typed (the key is the text's then).
            return textEditor == nil && (appController.frontController?.canPasteProperties ?? false)
        case #selector(deleteNumbers(_:))?, #selector(deleteCues(_:))?:
            return textEditor == nil && canEditFront && hasSelection
        case #selector(undoEdit(_:))?:
            let document = textEditor == nil ? appController.undoController?.store.document : nil
            item.title = MainMenu.undoTitle(redo: false, step: document?.canUndo == true ? document?.undoName : nil)
            return textEditor != nil || (undoEditor?.canUndo ?? false)
        case #selector(redoEdit(_:))?:
            let document = textEditor == nil ? appController.undoController?.store.document : nil
            item.title = MainMenu.undoTitle(redo: true, step: document?.canRedo == true ? document?.redoName : nil)
            return textEditor != nil || (undoEditor?.canRedo ?? false)
        case #selector(saveWorkspace(_:))?: return appController.canSaveFrontAgain
        case #selector(saveWorkspaceAs(_:))?: return appController.canSaveFront
        case #selector(recoverAutosaves(_:))?: return engineSession.connectionState == .connected
        case #selector(showWorkspaceSettings(_:))?, #selector(showWorkspaceSettingsPage(_:))?:
            return appController.frontController != nil
        case #selector(showWorkspaceStatus(_:))?: return appController.frontController != nil  // Show Mode too
        default: break
        }
        guard let raw = item.representedObject as? String, let command = TransportCommand(rawValue: raw) else {
            return true
        }
        let store = appController.frontController?.store
        let selected = appController.frontController?.cueList.selectedIDs() ?? []
        guard appController.canPerform(command) || command == .panic else { return false }
        switch command {
        // The key stays live whatever is open (Esc), but the item has nothing to panic with no workspace.
        case .panic: return !appController.allWindows.isEmpty
        // Pause and Resume follow the sidebar's buttons: Pause while something plays, Resume while
        // something is held; Stop and Panic while something is live.
        case .pauseAll: return store?.anyPlaying() ?? false
        case .resumeAll: return store?.anyPaused() ?? false
        case .stopAll: return store?.anyActive() ?? false
        case .pauseResumeSelected: return (store?.anyPlaying(in: selected) ?? false) || (store?.anyPaused(in: selected) ?? false)
        case .stopSelected, .panicSelected: return store?.anyActive(in: selected) ?? false
        case .previewSelected: return appController.frontController?.cueList.anyPreviewableSelected ?? false
        case .loadSelected, .startSelected, .resetSelected: return !selected.isEmpty
        default: return appController.canPerform(command)
        }
    }

    /// The Transport menu shows the key window's workspace keys.
    @MainActor func menuNeedsUpdate(_ menu: NSMenu) {
        if menu.title == NSLocalizedString("menu.file.openRecent", comment: "") {
            appController.recentMenuRefill = { [weak menu, weak self] in
                guard let menu, let self else { return }
                MainMenu.fillRecent(menu, self.appController.openableRecentURLs())
            }
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
