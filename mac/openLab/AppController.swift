// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine
import UniformTypeIdentifiers

/// The app-level owner of the windows (ARCHITECTURE section 9): one `WorkspaceWindowController`
/// per workspace the session holds, each given only its own store, plus the launcher shown when
/// none is open. It also runs File New/Open, the close and quit flows and the front-window
/// routing. No NSDocument.
@MainActor
final class AppController {
    let session: EngineSession
    /// One window per store, kept by the store (not its doc id): a window outlives an engine
    /// restart, where its store is rebound to another doc id.
    var controllers: [ObjectIdentifier: WorkspaceWindowController] = [:]
    /// The windows of the bound workspaces, by doc id.
    var windows: [ModelID: WorkspaceWindowController] {
        var out: [ModelID: WorkspaceWindowController] = [:]
        for (doc, store) in session.orderedStores { out[doc] = controllers[ObjectIdentifier(store)] }
        return out
    }
    /// Every workspace window, bound or waiting to be reattached, in open order.
    var allWindows: [WorkspaceWindowController] {
        (session.orderedStores.map(\.store) + session.detached.map(\.store)).compactMap {
            controllers[ObjectIdentifier($0)]
        }
    }
    let launcher = LauncherWindowController()

    // Seams for tests (no modal loops, no panels, no real recents list or key window).
    var alerts: WorkspaceAlerts = AppKitAlerts()
    var pickOpenURL: @MainActor () -> URL? = AppController.openPanel
    /// The folder to search for missing media (nil: cancelled).
    var pickFolderURL: @MainActor () -> URL? = AppController.folderPanel
    var pickSaveURL: @MainActor (String) -> URL? = AppController.savePanel
    var noteRecent: @MainActor (URL) -> Void = { NSDocumentController.shared.noteNewRecentDocumentURL($0) }
    var recentURLs: @MainActor () -> [URL] = { NSDocumentController.shared.recentDocumentURLs }
    /// Whether a recent workspace's file is still there (a seam: tests list files that do not exist).
    var recentExists: @MainActor (URL) -> Bool = { FileManager.default.fileExists(atPath: $0.path) }
    var forgetRecents: @MainActor () -> Void = { NSDocumentController.shared.clearRecentDocuments(nil) }
    var keyWindow: @MainActor () -> NSWindow? = { NSApp.keyWindow }
    var beep: @MainActor () -> Void = { NSSound.beep() }
    /// DX-04: the folders holding crash reports, and the Finder reveal of those that exist.
    var crashFolders: @MainActor () -> [URL] = AppController.defaultCrashFolders
    var revealInFinder: @MainActor ([URL]) -> Void = { NSWorkspace.shared.activateFileViewerSelecting($0) }
    /// The previous-run record is announced once per app run, at the first window.
    var previousRunAnnounced = false
    /// A text field or text view is being edited: keys that type must stay with it.
    var isTextEditing: @MainActor () -> Bool = { (NSApp.keyWindow?.firstResponder as? NSText)?.isEditable == true }
    /// Per workspace: its `settings.hotkeys` as last read (a change re-reads and warns once).
    /// Entries in the per-workspace key cache (tests: evicted when a workspace closes).
    var keyCacheCount: Int { keyCache.count }
    var keySubs: [ObjectIdentifier: AnyCancellable] = [:]
    var keyCache:
        [ObjectIdentifier: (
            settings: JSONValue?, cueChords: Set<KeyChord>, shadowed: Set<KeyChord>, keys: TransportKeys
        )] =
            [:]
    /// A hotkey recorder has the focus: the key goes to it, not to cues or transport (Esc still panics).
    var isRecordingKey: @MainActor () -> Bool = { NSApp.keyWindow?.firstResponder is HotkeyRecorder }
    /// Menu-bar keys outside the Transport menu; a cue hotkey on one never fires (Cmd-Q stays Quit).
    var menuChords: @MainActor () -> Set<KeyChord> = HotkeyRules.currentMenuChords
    /// Cue hotkeys sent down and not yet up, by key code: their `up` goes out on key release.
    private var heldHotkeys: [UInt16: JSONValue] = [:]
    /// A modal panel or alert is running (cue hotkeys must not reach through it).
    var isModalUp: @MainActor () -> Bool = { NSApp.modalWindow != nil }
    private var resignObserver: Any?
    private var keyUpMonitor: Any?

    private var subs: [AnyCancellable] = []
    private var keyMonitor: Any?
    /// How many monitors `installKeyMonitor` has registered (tests: exactly one).
    private(set) var keyMonitorInstalls = 0
    /// Bounds the waits on the quit and save paths (injected so tests never wait in real time).
    let scheduler: Scheduler
    let timeout: Double
    var closing: Set<ModelID> = []
    var quitting = false

    /// Ends, at quit, an engine it started that was never reached (nil in tests of the windows).
    let supervisor: EngineSupervisor?

    init(
        session: EngineSession, supervisor: EngineSupervisor? = nil, scheduler: Scheduler = SystemScheduler(),
        timeout: Double = 5
    ) {
        self.session = session
        self.supervisor = supervisor
        self.scheduler = scheduler
        self.timeout = timeout
        // A key released while the app is inactive never reaches the monitor: forget held keys.
        resignObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didResignActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in  // No trigger_released is sent: a cue is never stopped without an observed key release.
            MainActor.assumeIsolated { self?.heldHotkeys = [:] }
        }
        launcher.onNew = { [weak self] in Task { await self?.newWorkspace() } }
        launcher.onOpen = { [weak self] in Task { await self?.openWorkspace() } }
        launcher.onOpenRecent = { [weak self] url in Task { await self?.openWorkspace(url: url) } }
        subs.append(session.storesChanged.sink { [weak self] in self?.sync() })
        // $workspaces publishes before the property changes: use the delivered value.
        subs.append(session.$workspaces.sink { [weak self] in self?.updateTitles($0) })
    }

    // MARK: Esc

    /// Esc panics every workspace whichever window is key (launcher, alert, a field editor
    /// mid-edit), then the event carries on so the editor or alert still handles it.
    func installKeyMonitor() {
        guard keyMonitor == nil else { return }
        keyMonitorInstalls += 1
        // Deliberately also fires while an alert or panel is up: panic must never be blocked by UI.
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            let consumed = MainActor.assumeIsolated { self?.handleKeyDown(event) == nil }
            return consumed ? nil : event
        }
        keyUpMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyUp) { [weak self] event in
            MainActor.assumeIsolated { self?.handleKeyUp(event) }
            return event
        }
    }

    /// The monitor's body, callable from tests. Esc always goes on to the responder. A key-map
    /// chord is taken here (nil: consumed) because a focused table or outline view would otherwise
    /// swallow Space before the menu's key equivalent sees it; a text field being edited keeps its
    /// typing keys (`canPerform`).
    func handleKeyDown(_ event: NSEvent) -> NSEvent? {
        if Self.isPanicKey(event) {
            firePanic()
            return event
        }
        guard !isRecordingKey(), let chord = KeyChord(event: event) else { return event }
        if let command = frontKeys.command(for: chord), canPerform(command) {
            let isRepeat = event.isARepeat
            Task { await perform(command, isRepeat: isRepeat) }
            return nil
        }
        return fireCueHotkeys(chord, event: event) ? nil : event
    }

    /// A cue hotkey fires in every open workspace, key window or not (ARCHITECTURE section 6): one
    /// app-scope `hotkey` goes to the engine, which does the matching and applies each cue's
    /// second-trigger rule. The app only decides whether the key is a cue hotkey of an open
    /// workspace (to take the event); typing keys stay with a text field being edited, a held key
    /// fires once and a menu key is never taken.
    private func fireCueHotkeys(_ chord: KeyChord, event: NSEvent) -> Bool {
        if chord.typesCharacters, isTextEditing() { return false }
        if menuChords().contains(chord) || cueHotkeysBlocked() { return false }
        let matched = session.orderedStores.contains { entry in
            cueHotkeys(for: entry.store).contains { $0.chord == chord }
        }
        guard matched, let hotkey = chord.hotkeyJSON, let key = hotkey["key"]?.stringValue else { return false }
        // A repeat is the held key; a fresh press always fires (a lost key-up must not mute the key).
        if event.isARepeat { return true }
        heldHotkeys[event.keyCode] = hotkey
        Task { await sendHotkey(key: key, modifiers: Self.modifiers(of: hotkey), up: false) }
        return true
    }

    private static func modifiers(of hotkey: JSONValue) -> HotkeyModifiers {
        let m = hotkey["modifiers"]
        return HotkeyModifiers(
            shift: m?["shift"] == .bool(true), control: m?["control"] == .bool(true),
            alt: m?["alt"] == .bool(true), command: m?["command"] == .bool(true))
    }

    /// A refused or undeliverable hotkey beeps and goes to every show log (it was meant for all).
    private func sendHotkey(key: String, modifiers: HotkeyModifiers, up: Bool) async {
        if case .failure(let e) = await session.hotkey(key: key, modifiers: modifiers, up: up) {
            beep()
            for (_, store) in session.orderedStores { log(e, on: store) }
        }
    }

    /// Keys stay with a modal panel or alert, a sheet, the launcher and a panel that is not ours
    /// (file open/save). Monitoring windows such as the Log window, and no key window at all (app
    /// active, windows behind), still let hotkeys fire.
    private func cueHotkeysBlocked() -> Bool {
        if isModalUp() { return true }
        guard let key = keyWindow() else { return false }
        return key.attachedSheet != nil || key === launcher.window || key is NSPanel
    }

    /// The cue hotkeys of `store` that fire: a chord equal to one of its transport keys belongs to
    /// the transport key (logged once in `keys(for:)`).
    private func cueHotkeys(for store: WorkspaceStore) -> [(cue: ModelID, chord: KeyChord)] {
        let taken = Set(keys(for: store).chords.values)
        return (store.model?.cueHotkeys ?? []).filter { !taken.contains($0.chord) }
    }

    /// Key up: the hotkey's `up`, so the engine releases the cues it fired that use
    /// second-trigger-on-release.
    func handleKeyUp(_ event: NSEvent) {
        guard let hotkey = heldHotkeys.removeValue(forKey: event.keyCode), let key = hotkey["key"]?.stringValue
        else { return }
        Task { await sendHotkey(key: key, modifiers: Self.modifiers(of: hotkey), up: true) }
    }

    private func firePanic() { Task { _ = await panicAll() } }

    private nonisolated static func isPanicKey(_ event: NSEvent) -> Bool {
        let mods = event.modifierFlags.intersection(.deviceIndependentFlagsMask).subtracting([.capsLock, .function])
        return event.keyCode == 53 && mods.isEmpty && !event.isARepeat
    }

    private static func openPanel() -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        if let type = UTType(filenameExtension: "openlab") { panel.allowedContentTypes = [type] }
        return panel.runModal() == .OK ? panel.url : nil
    }

    private static func folderPanel() -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.message = loc("relink.panel.message")
        panel.prompt = loc("relink.panel.prompt")
        return panel.runModal() == .OK ? panel.url : nil
    }

    /// A workspace name as the Save panel's suggested file name: its first line, trimmed, without
    /// path separators, and short enough for a file name with the extension (a name can be 1024
    /// characters and span lines; file names are at most 255 bytes).
    static func suggestedFileName(_ name: String) -> String {
        let line =
            name.split(omittingEmptySubsequences: false, whereSeparator: \.isNewline).first.map(String.init) ?? ""
        var out = line.trimmingCharacters(in: .whitespaces)
            .replacingOccurrences(of: "/", with: "-").replacingOccurrences(of: ":", with: "-")
        while out.utf8.count > 200 { out.removeLast() }
        return out
    }

    private static func savePanel(_ name: String) -> URL? {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = suggestedFileName(name)
        if let type = UTType(filenameExtension: "openlab") { panel.allowedContentTypes = [type] }
        return panel.runModal() == .OK ? panel.url : nil
    }
}
