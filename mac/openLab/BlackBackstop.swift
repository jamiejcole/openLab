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
/// keeps black and is shown there on this machine, the display is connected, and it is not the one
/// with the menu bar. Stage states are not read: the backstop is for the moments the stage host
/// cannot say anything.
enum BackstopPlan {
    /// The frames to hold black, by display id.
    static func covers(
        workspaces: [(model: WorkspaceModel?, outputs: VideoOutputs)], displays: [BackstopDisplay]
    ) -> [String: NSRect] {
        var wanted = Set<String>()
        for (model, outputs) in workspaces {
            for stage in model?.stages ?? [] where stage.raw["keep_black"] == .bool(true) {
                if let display = outputs.stages.first(where: { $0.stage == stage.id })?.mapping?.display {
                    wanted.insert(display)
                }
            }
        }
        var out: [String: NSRect] = [:]
        for display in displays where wanted.contains(display.id) && !display.hasMenuBar && out[display.id] == nil {
            out[display.id] = display.frame
        }
        return out
    }
}

/// VID-12: the stage host draws the output windows, so between its death and its relaunch (a crash,
/// a kill, the pause after repeated crashes, the engine's own death until a relaunched engine has
/// started a new one) an output display would show the desktop. The app holds a borderless black
/// window on each display a keep-black stage is shown on, one level under the stage host's windows
/// there, so the audience sees black in those gaps. It never covers the display with the menu bar,
/// never takes key or main, lets every click through, and follows the workspaces' models and the
/// display choices they last heard (kept while the engine is gone), and this Mac's displays.
@MainActor
final class BlackBackstop {
    /// One under the stage host's output windows on a display without the menu bar (screen-saver
    /// level): over every app's windows, menus, the menu bar and Dock there, and under the stages.
    static let level = NSWindow.Level(rawValue: NSWindow.Level.screenSaver.rawValue - 1)

    /// This Mac's displays, read at start and on every display change (a seam: tests give virtual
    /// displays places off every screen).
    var readDisplays: @MainActor () -> [BackstopDisplay] = BlackBackstop.systemDisplays
    /// The windows up, by display id.
    private(set) var windows: [String: NSWindow] = [:]
    private var displays: [BackstopDisplay]
    private var stores: [WorkspaceStore] = []
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

    /// The workspaces to follow: bound or waiting to be reattached after an engine restart.
    func follow(_ live: [WorkspaceStore]) {
        stores = live
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

    private func update() {
        let plan = BackstopPlan.covers(workspaces: stores.map { ($0.model, $0.videoOutputs) }, displays: displays)
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
