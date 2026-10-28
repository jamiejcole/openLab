// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

@main
final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuItemValidation, NSMenuDelegate {
    /// `-OpenLabEngine fake` runs the in-process fake engine (UI tests); default is the real one.
    @MainActor private(set) lazy var engine = EngineMode.fromLaunchArguments().make()
    @MainActor var engineSession: EngineSession { engine.0 }
    @MainActor private(set) lazy var appController = AppController(session: engineSession)
    @MainActor private(set) lazy var alarmController = EngineAlarmController(
        session: engineSession, supervisor: engine.1, presenter: EngineAlarmPanel(),
        alerts: { [unowned self] in appController.alerts })

    static func main() {
        let app = NSApplication.shared
        let delegate = AppDelegate()
        app.delegate = delegate
        app.run()
    }

    func applicationWillFinishLaunching(_ notification: Notification) {
        NSApp.appearance = NSAppearance(named: .darkAqua)
        let menu = MainMenu.make()
        menu.transport.delegate = self
        menu.edit.delegate = self
        NSApp.mainMenu = menu.bar
        NSApp.windowsMenu = menu.windows
        NSApp.helpMenu = menu.help
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        // The launcher shows until the engine reports a workspace (fake engine: at once).
        appController.showLauncher()
        appController.installKeyMonitor()
        _ = alarmController
        // Finds the engine (or starts one) and connects; the engine opens nothing by itself.
        engine.1.start()
        NSApp.activate(ignoringOtherApps: true)
    }

    /// A show must survive an accidental window close: never quit, and bring a window back.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        appController.reopen()
        return false
    }

    /// One save prompt per dirty workspace, then `goodbye { shutdown }`; Cancel aborts the quit.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if appController.quitting { return .terminateCancel }
        Task { @MainActor in
            let ok = await appController.prepareToQuit()
            NSApp.reply(toApplicationShouldTerminate: ok)
        }
        return .terminateLater
    }

    @MainActor @objc func newWorkspace(_ sender: Any?) { Task { await appController.newWorkspace() } }
    @MainActor @objc func openWorkspace(_ sender: Any?) { Task { await appController.openWorkspace() } }

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

    @MainActor @objc func validateMenuItem(_ item: NSMenuItem) -> Bool {
        if item.action == #selector(enterShowMode(_:)) || item.action == #selector(enterEditMode(_:)) {
            guard let front = appController.frontController else { return false }
            return front.isShowMode == (item.action == #selector(enterEditMode(_:)))
        }
        if item.action == #selector(newCue(_:)) || item.action == #selector(renumberCues(_:))
            || item.action == #selector(moveCuesUp(_:)) || item.action == #selector(moveCuesDown(_:))
        {
            return appController.frontController.map { !$0.isShowMode } ?? false
        }
        if item.action == #selector(copyCues(_:)) || item.action == #selector(cutCues(_:)) {
            return anyText != nil || (appController.frontController.map { !$0.isShowMode } ?? false)
        }
        if item.action == #selector(pasteCues(_:)) {
            return anyText != nil || (appController.frontController.map { !$0.isShowMode } ?? false)
        }
        if item.action == #selector(deleteNumbers(_:)) {
            return textEditor == nil && (appController.frontController.map { !$0.isShowMode } ?? false)
        }
        if item.action == #selector(deleteCues(_:)) {
            return textEditor == nil && (appController.frontController.map { !$0.isShowMode } ?? false)
        }
        if item.action == #selector(undoEdit(_:)) {
            return textEditor != nil || (editor?.canUndo ?? false)
        }
        if item.action == #selector(redoEdit(_:)) {
            return textEditor != nil || (editor?.canRedo ?? false)
        }
        if item.action == #selector(relinkMedia(_:)) {
            return appController.frontController.map { !$0.isShowMode } ?? false
        }
        if item.action == #selector(showLog(_:)) {
            return appController.frontController != nil
        }
        guard let raw = item.representedObject as? String, let command = TransportCommand(rawValue: raw) else {
            return true
        }
        return appController.canPerform(command)
    }

    /// The Transport menu shows the key window's workspace keys.
    @MainActor func menuNeedsUpdate(_ menu: NSMenu) {
        // Show Mode (SM-01): the editing items go dark in the front window's Show Mode.
        if menu.title == NSLocalizedString("menu.edit", comment: "") {
            MainMenu.lockEditItems(in: menu, locked: appController.frontController?.isShowMode == true)
            return
        }
        MainMenu.applyKeys(to: menu, appController.frontKeys)
    }
}
