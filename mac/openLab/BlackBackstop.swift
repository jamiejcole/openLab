// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// A display a backstop can cover: its id as the stage host names it (ColorSync's UUID), its frame
/// in screen coordinates, and whether it holds the menu bar (the operator's display).
struct BackstopDisplay: Equatable {
    let id: String
    let frame: NSRect
    let hasMenuBar: Bool
}

/// VID-12: which displays the app holds black. A display is held when a stage of an open workspace
/// keeps black and is shown there on this machine, and the display is connected: the one with the
/// menu bar too, as the stage itself covers it (#708). Stage states are not read: the backstop is
/// for the moments the stage host cannot say anything. A stage whose black is down (`panicked`:
/// a panic took it down, or no picture has played on it since its workspace opened) holds
/// nothing, unless its black was up when its engine went away (`carried`: a relaunched engine
/// reopens the workspace with every stage's black down), nor one the operator's Esc took down
/// here (`dismissed`) until the engine has said so.
enum BackstopPlan {
    /// The stages holding a display black, with the display each is shown on.
    static func stages(
        workspaces: [(model: WorkspaceModel?, outputs: VideoOutputs)], dismissed: Set<ModelID> = [],
        carried: [ModelID: StageMappingEntry] = [:]
    ) -> [(stage: ModelID, display: String)] {
        var out: [(stage: ModelID, display: String)] = []
        for (model, outputs) in workspaces {
            for stage in model?.stages ?? [] where stage.raw["keep_black"] == .bool(true) {
                guard !dismissed.contains(stage.id) else { continue }
                let entry = outputs.stages.first { $0.stage == stage.id }
                if let display = entry?.mapping?.display, entry?.panicked != true || carried[stage.id] != nil {
                    out.append((stage.id, display))
                } else if entry == nil, let display = carried[stage.id]?.mapping?.display {
                    // A relaunched engine has not said where the stage is yet.
                    out.append((stage.id, display))
                }
            }
        }
        return out
    }

    /// The frames to hold black, by display id.
    static func covers(
        workspaces: [(model: WorkspaceModel?, outputs: VideoOutputs)], displays: [BackstopDisplay],
        dismissed: Set<ModelID> = [], carried: [ModelID: StageMappingEntry] = [:]
    ) -> [String: NSRect] {
        let held = stages(workspaces: workspaces, dismissed: dismissed, carried: carried)
        let wanted = Set(held.map(\.display))
        var out: [String: NSRect] = [:]
        for display in displays where wanted.contains(display.id) && out[display.id] == nil {
            out[display.id] = display.frame
        }
        return out
    }
}

/// VID-12: the stage host draws the output windows, so between its death and its relaunch (a crash,
/// a kill, the pause after repeated crashes, the engine's own death until a relaunched engine has
/// started a new one) an output display would show the desktop. The app holds a borderless black
/// window on each display a keep-black stage is shown on, one level under the stage host's windows
/// there, so the audience sees black in those gaps. Like the stage, it covers the display with the
/// menu bar too; Esc takes it down at once (`dismiss`), whether or not an engine answers. It never
/// takes key or main, so the operator's keys stay with the app, lets every click through, and
/// follows the workspaces' models and the display choices they last heard (kept while the engine
/// is gone), and this Mac's displays.
@MainActor
final class BlackBackstop {
    /// One under the stage host's output windows (screen-saver level): over every app's windows,
    /// menus, the menu bar and Dock, and under the stages.
    static let level = NSWindow.Level(rawValue: NSWindow.Level.screenSaver.rawValue - 1)

    /// This Mac's displays, read at start and on every display change (a seam: tests give virtual
    /// displays places off every screen).
    var readDisplays: @MainActor () -> [BackstopDisplay] = BlackBackstop.systemDisplays
    /// The windows up, by display id.
    private(set) var windows: [String: NSWindow] = [:]
    private var displays: [BackstopDisplay]
    private var stores: [WorkspaceStore] = []
    /// Stages whose black the operator's Esc took down here, until their engine says it is down
    /// (`panicked`) too: from then on the engine's word is followed again, so the next picture
    /// played on the stage brings the black back.
    private(set) var dismissed: Set<ModelID> = []
    /// Stages whose black was up when their workspace's engine went away: held through its
    /// relaunch, which reopens the workspace with every black down, until Esc or a picture played
    /// on the stage (the engine's word is followed again from then).
    /// With what its engine last said of it then.
    private(set) var carried: [ModelID: StageMappingEntry] = [:]
    /// The workspaces waiting to be reattached to a relaunched engine.
    private var waiting: Set<ObjectIdentifier> = []
    private var storeSubs: [ObjectIdentifier: AnyCancellable] = [:]
    private var screenSub: AnyCancellable?
    private var secondRead: Task<Void, Never>?
    private let scheduler: Scheduler

    /// `scheduler` times the second read of the displays after a change.
    init(scheduler: Scheduler = SystemScheduler()) {
        self.scheduler = scheduler
        displays = Self.systemDisplays()
        screenSub = NotificationCenter.default.publisher(for: NSApplication.didChangeScreenParametersNotification)
            .sink { [weak self] _ in MainActor.assumeIsolated { self?.screensChanged() } }
    }

    /// How long after a display change the displays are read a second time.
    static let secondReadDelay = 0.5

    /// AppKit's display change: the displays are read now and once more a moment later, as the
    /// stage host does. The system moves a window whose display went away onto another, maybe the
    /// operator's; a move that lands after the first read is undone by the second, which puts the
    /// window back over its own display or takes it down.
    func screensChanged() {
        displaysChanged()
        secondRead?.cancel()
        let scheduler = scheduler
        secondRead = Task { [weak self] in
            guard (try? await scheduler.sleep(seconds: Self.secondReadDelay)) != nil, !Task.isCancelled else { return }
            self?.displaysChanged()
        }
    }

    /// The workspaces to follow: bound or waiting to be reattached after an engine restart
    /// (`waiting`).
    func follow(_ live: [WorkspaceStore], waiting: Set<ObjectIdentifier> = []) {
        stores = live
        self.waiting = waiting
        let ids = Set(live.map(ObjectIdentifier.init))
        storeSubs = storeSubs.filter { ids.contains($0.key) }
        for store in live where storeSubs[ObjectIdentifier(store)] == nil {
            storeSubs[ObjectIdentifier(store)] = store.changes.sink { [weak self] change in
                guard Self.mayMove(change) else { return }
                MainActor.assumeIsolated { self?.update() }
            }
        }
        update()
    }

    /// A display was plugged in or out, moved, resized, or the menu bar moved to another.
    func displaysChanged() {
        displays = readDisplays()
        update()
    }

    /// Whether a store change can move a backstop: a new snapshot, a stage edit, or new display
    /// choices (`video_outputs` arrives as `.notices`). Cue edits and runtime ticks cannot.
    private static func mayMove(_ change: StoreChange) -> Bool {
        switch change {
        case .reset, .notices: return true
        case .model(let upserts, let deletes):
            return upserts.contains { if case .stage = $0 { true } else { false } }
                || deletes.contains { if case .stage = $0 { true } else { false } }
        case .runtime, .progress, .waits: return false
        }
    }

    /// Whether a stage of a followed workspace is up (a picture or its black) on the display with
    /// the menu bar, as its engine last said: it covers the operator's windows and every question
    /// the app asks there.
    func stageCoversMenuBarDisplay() -> Bool {
        let menuBar = Set(displays.filter(\.hasMenuBar).map(\.id))
        return stores.contains { store in
            store.videoOutputs.stages.contains { entry in
                guard let display = entry.mapping?.display, menuBar.contains(display) else { return false }
                return entry.state?.state == "shown"
            }
        }
    }

    /// Esc: every black held goes now, whether or not an engine is there to hear the panic (a
    /// relaunching engine, a stage host down), so the operator is never locked out of the display.
    func dismiss() {
        let held = BackstopPlan.stages(workspaces: stores.map { ($0.model, $0.videoOutputs) }, carried: carried)
        dismissed.formUnion(held.map(\.stage))
        carried = [:]
        update()
    }

    private func update() {
        let workspaces: [(model: WorkspaceModel?, outputs: VideoOutputs)] = stores.map { ($0.model, $0.videoOutputs) }
        func keepsBlack(_ id: ModelID) -> Bool {
            stores.contains { $0.model?.stages.contains { $0.id == id && $0.raw["keep_black"] == .bool(true) } == true }
        }
        // A stage whose engine has taken its black down too, or that no longer keeps black, is
        // the engine's again.
        dismissed = dismissed.filter { id in
            keepsBlack(id) && workspaces.contains { $0.outputs.stages.contains { $0.stage == id && !$0.panicked } }
        }
        // A workspace whose engine went away carries the blacks it had up through the relaunch.
        for store in stores where waiting.contains(ObjectIdentifier(store)) {
            for (stage, _) in BackstopPlan.stages(workspaces: [(store.model, store.videoOutputs)], dismissed: dismissed)
            {
                if carried[stage] == nil { carried[stage] = store.videoOutputs.stages.first { $0.stage == stage } }
            }
        }
        // Carried until there is word otherwise from a new engine: the stage no longer keeps black
        // or is gone, or the engine has it without a display or with its black up (a picture played
        // on it there: the engine's again). What the old engine last said is no word.
        carried = carried.filter { id, before in
            let models = stores.compactMap(\.model)
            if models.contains(where: { $0.stages.contains { $0.id == id && $0.raw["keep_black"] != .bool(true) } }) {
                return false
            }
            if models.count == stores.count, !models.contains(where: { $0.stages.contains { $0.id == id } }) {
                return false
            }
            guard let entry = stores.lazy.compactMap({ $0.videoOutputs.stages.first { $0.stage == id } }).first,
                entry != before
            else { return true }
            return entry.mapping != nil && entry.panicked
        }
        let plan = BackstopPlan.covers(
            workspaces: workspaces, displays: displays, dismissed: dismissed, carried: carried)
        for id in windows.keys where plan[id] == nil {
            windows.removeValue(forKey: id)?.close()
        }
        for (id, frame) in plan {
            let window = windows[id] ?? Self.makeWindow(frame: frame)
            windows[id] = window
            // Also puts back a window the system moved (its display went away).
            if window.frame != frame { window.setFrame(frame, display: false) }
            // Never key, never activating: the operator's keys and focus stay where they are.
            if !window.isVisible { window.orderFrontRegardless() }
        }
    }

    /// A borderless black window over `frame`, not yet ordered front.
    private static func makeWindow(frame: NSRect) -> NSWindow {
        let window = BackstopWindow(contentRect: frame, styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.level = level
        // On every Space and over another app's full screen, out of Mission Control and cycling.
        window.collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenAuxiliary]
        window.ignoresMouseEvents = true
        window.hasShadow = false
        window.isOpaque = true
        window.backgroundColor = .black
        // Hiding the app (Cmd-H) must not uncover an output display.
        window.canHide = false
        window.isExcludedFromWindowsMenu = true
        window.animationBehavior = .none
        window.contentView = BlackView(frame: NSRect(origin: .zero, size: frame.size))
        return window
    }

    /// The connected displays: each screen's ColorSync UUID (the stage host's display id), its
    /// frame, and whether it is the main display (the one with the menu bar).
    static func systemDisplays() -> [BackstopDisplay] {
        let menuBar = CGMainDisplayID()
        return NSScreen.screens.compactMap { screen in
            guard let number = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber,
                let uuid = CGDisplayCreateUUIDFromDisplayID(number.uint32Value)?.takeRetainedValue(),
                let text = CFUUIDCreateString(nil, uuid) as String?
            else { return nil }
            return BackstopDisplay(id: text, frame: screen.frame, hasMenuBar: number.uint32Value == menuBar)
        }
    }
}

/// Never key or main, whatever its style.
private final class BackstopWindow: NSWindow {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

/// Black over its whole bounds.
private final class BlackView: NSView {
    override var isOpaque: Bool { true }
    override func draw(_ dirtyRect: NSRect) {
        NSColor.black.setFill()
        dirtyRect.fill()
    }
}
