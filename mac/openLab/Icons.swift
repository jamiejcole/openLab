// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import os

/// The one table of every icon the app shows. Nearly every concept names openLab's own drawing (`drawing`, a
/// 16-point SVG in the asset catalog, so another platform can use the same files). Panic All draws its own
/// picture (`fadeOut`), and the few left name an SF Symbol (`symbol`), each from SF Symbols 1 (macOS 11), below
/// the macOS 12 target. A name that does not resolve is logged once and drawn as the localised label, so a slot
/// is never silently blank.
enum Icon: CaseIterable {
    // Cue kinds (the toolbar and the cue list's type column).
    case kindAudio, kindGroup, kindMemo, kindWait, kindFade, kindStart, kindStop, kindPause, kindLoad
    case kindReset, kindDevamp, kindGoTo, kindArm, kindDisarm, kindNetwork, kindVideo
    // A Group cue's icon follows its mode (the timeline mode uses `kindGroup`).
    case kindGroupPlaylist, kindGroupEnter, kindGroupNext, kindGroupRandom
    // Cue states (UI-04: each state the list marks has its own shape; a wait shows the running mark, as QLab).
    case stateLoaded, stateRunning, statePaused, stateFading, stateStopping, stateBroken
    // The second colour's lock in the Inspector (locked: no second colour; open: one to pick).
    case colourLocked, colourUnlocked
    // Continue modes.
    case continueAuto, continueFollow
    // Transport strip and the Active Cues rows.
    case transportReset, transportPause, transportResume, transportPanic
    case activeCuePause, activeCueResume, activeCuePanic
    // Footer.
    case layoutMasthead, layoutInspector, layoutSidebar, settings
    // Engine status levels.
    case statusWaiting, statusNormal, statusWarning, statusAlarm
    // Cue list markers.
    case playhead, preview, outputOverridden, flagged, flaggedInside, warningMarker, engineAlarm
    // Time column header toggle (elapsed / remaining).
    case timeToggle, timeToggleUp
    // The Target column's button for a file cue (present and missing), and the Target field's clear, choose and jump glyphs.
    case targetFile, targetFileMissing, fieldClear, fieldChoose, fieldJump
    // Reserved for a screen that does not exist yet; it resolves and is tested already.
    case reservedPopOut

    /// The asset-catalog name of openLab's own drawing; nil for a concept shown as an SF Symbol.
    var drawing: String? {
        switch self {
        case .kindAudio: return "kind-audio"
        case .kindGroup: return "kind-group"
        case .kindMemo: return "kind-memo"
        case .kindWait: return "kind-wait"
        case .kindFade: return "kind-fade"
        case .kindStart: return "kind-start"
        case .kindStop: return "kind-stop"
        case .kindPause: return "kind-pause"
        case .kindLoad: return "kind-load"
        case .kindReset: return "kind-reset"
        case .kindDevamp: return "kind-devamp"
        case .kindGoTo: return "kind-go-to"
        case .kindArm: return "kind-arm"
        case .kindDisarm: return "kind-disarm"
        case .kindNetwork: return "kind-network"
        case .kindVideo: return "kind-video"
        case .kindGroupPlaylist: return "kind-group-playlist"
        case .kindGroupEnter: return "kind-group-enter"
        case .kindGroupNext: return "kind-group-next"
        case .kindGroupRandom: return "kind-group-random"
        case .colourLocked: return "colour-locked"
        case .colourUnlocked: return "colour-unlocked"
        case .stateLoaded: return "state-loaded"
        case .stateRunning: return "state-running"
        case .statePaused: return "state-paused"
        case .stateFading: return "state-fading"
        case .stateStopping: return "state-stopping"
        case .stateBroken: return "state-broken"
        case .continueAuto: return "continue-auto"
        case .continueFollow: return "continue-follow"
        // Reset, Pause and Resume All share the circle outline.
        case .transportReset: return "transport-reset"
        case .transportPause: return "transport-pause"
        case .transportResume: return "transport-resume"
        case .activeCuePanic: return "active-cue-panic"
        case .layoutMasthead: return "layout-masthead"
        case .layoutInspector: return "layout-inspector"
        case .layoutSidebar: return "layout-sidebar"
        case .settings: return "settings"
        case .statusWaiting: return "status-waiting"
        case .statusNormal: return "status-normal"
        case .statusWarning: return "status-warning"
        case .statusAlarm: return "status-alarm"
        case .playhead: return "playhead"
        case .preview: return "preview"
        case .outputOverridden: return "output-overridden"
        case .flagged: return "flagged"
        case .flaggedInside: return "flagged-inside"
        case .warningMarker: return "warning-marker"
        // Right while the column counts up, left while it counts down.
        case .timeToggle: return "time-toggle"
        case .timeToggleUp: return "time-toggle-up"
        case .targetFile: return "target-file"
        case .fieldClear: return "field-clear"
        case .fieldChoose: return "field-choose"
        case .fieldJump: return "field-jump"
        case .reservedPopOut: return "reserved-pop-out"
        case .transportPanic, .activeCuePause, .activeCueResume, .engineAlarm, .targetFileMissing: return nil
        }
    }

    /// The SF Symbol for a concept without a drawing of its own; nil for every other concept.
    var symbol: String? {
        switch self {
        case .activeCuePause: return "pause.fill"
        case .activeCueResume: return "play.fill"
        case .engineAlarm: return "exclamationmark.triangle.fill"
        case .targetFileMissing: return "trash"
        default: return nil
        }
    }

    /// Panic's own picture (no symbol says "fade everything out"): a level falling to nothing inside
    /// a frame. A template, so it takes the button's colour.
    static func fadeOut(_ label: String) -> NSImage {
        let image = NSImage(size: NSSize(width: 20, height: 20), flipped: false) { rect in
            let box = rect.insetBy(dx: 2, dy: 2)
            let frame = NSBezierPath(roundedRect: box, xRadius: 2, yRadius: 2)
            frame.lineWidth = 1.5
            NSColor.black.set()
            frame.stroke()
            let wedge = NSBezierPath()
            wedge.move(to: NSPoint(x: box.minX, y: box.maxY))
            wedge.line(to: NSPoint(x: box.minX, y: box.minY))
            wedge.line(to: NSPoint(x: box.maxX, y: box.minY))
            wedge.close()
            wedge.fill()
            return true
        }
        image.isTemplate = true
        image.accessibilityDescription = label
        return image
    }

    /// The icon as an image, never nil. A symbol the system does not know (or a drawing missing from
    /// the bundle) is logged once and the image is `label` drawn as text (`compact`: its first letter,
    /// for a slot only 16 points wide).
    func image(_ label: String, compact: Bool = false) -> NSImage {
        if self == .transportPanic { return Self.fadeOut(label) }
        let found: NSImage?
        let name: String
        if let symbol {
            name = symbol
            found = IconLookup.shared.image(symbol, description: label)
        } else {
            name = drawing ?? ""
            found = IconLookup.shared.drawing(name, description: label)
        }
        if let found { return found }
        IconLookup.shared.reportMissing(name, concept: self)
        return IconLookup.textImage(label, compact: compact)
    }

    /// True when a symbol or drawing (not the text stand-in) shows for this icon on this system.
    var resolvesToSymbol: Bool {
        if self == .transportPanic { return true }
        if let symbol { return IconLookup.shared.image(symbol, description: nil) != nil }
        return IconLookup.shared.drawing(drawing ?? "", description: nil) != nil
    }
}

/// The lookup behind `Icon`, with a seam so a test can make a name vanish (a symbol the running
/// macOS lacks, a drawing missing from the bundle) and see the text stand-in. Each missing name is logged once.
final class IconLookup: @unchecked Sendable {
    static let shared = IconLookup()
    private static let systemHas: @Sendable (String) -> Bool = {
        NSImage(systemSymbolName: $0, accessibilityDescription: nil) != nil || NSImage(named: $0) != nil
    }

    private let lock = NSLock()
    private var reported: Set<String> = []
    private var has: @Sendable (String) -> Bool = IconLookup.systemHas
    private let log = Logger(subsystem: Bundle.main.bundleIdentifier ?? "openLab", category: "icons")

    func image(_ name: String, description: String?) -> NSImage? {
        lock.lock()
        let exists = has(name)
        lock.unlock()
        return exists ? NSImage(systemSymbolName: name, accessibilityDescription: description) : nil
    }

    /// One of openLab's own drawings from the asset catalog, as a template so it takes the control's
    /// tint. A copy: `NSImage(named:)` hands out one shared object, and each use carries its own label.
    func drawing(_ name: String, description: String?) -> NSImage? {
        lock.lock()
        let exists = has(name)
        lock.unlock()
        guard exists, let image = NSImage(named: name)?.copy() as? NSImage else { return nil }
        image.isTemplate = true
        image.accessibilityDescription = description
        #if DEBUG
            objc_setAssociatedObject(image, &Self.drawingKey, name, .OBJC_ASSOCIATION_COPY_NONATOMIC)
        #endif
        return image
    }

    func reportMissing(_ name: String, concept: Icon) {
        lock.lock()
        let first = reported.insert(name).inserted
        lock.unlock()
        if first {
            log.error(
                "Icon \(name, privacy: .public) is missing for \(String(describing: concept), privacy: .public)")
        }
    }

    #if DEBUG
        /// Test seam, not in a Release build: replaces the existence check (nil: the system's) and
        /// forgets what was logged.
        func setLookup(_ newLookup: (@Sendable (String) -> Bool)?) {
            lock.lock()
            has = newLookup ?? IconLookup.systemHas
            reported = []
            lock.unlock()
        }

        /// Tags each drawing's image with its name, so a test can tell which drawing a view shows
        /// without comparing pixels (a copy of a named image keeps neither its name nor its representations).
        nonisolated(unsafe) private static var drawingKey: UInt8 = 0

        /// The name of the drawing `image` was made from; nil for a symbol or the text stand-in.
        static func drawingName(of image: NSImage?) -> String? {
            image.flatMap { objc_getAssociatedObject($0, &drawingKey) as? String }
        }

        var reportedNames: Set<String> {
            lock.lock()
            defer { lock.unlock() }
            return reported
        }
    #endif

    /// `text` drawn as a template image, so it takes the control's tint.
    static func textImage(_ text: String, compact: Bool) -> NSImage {
        let shown = compact ? String(text.prefix(1)) : text
        let attrs: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: compact ? 11 : 12, weight: .semibold),
            .foregroundColor: NSColor.black,
        ]
        let size = (shown as NSString).size(withAttributes: attrs)
        let image = NSImage(size: NSSize(width: ceil(size.width) + 2, height: ceil(size.height)), flipped: false) { _ in
            (shown as NSString).draw(at: NSPoint(x: 1, y: 0), withAttributes: attrs)
            return true
        }
        image.isTemplate = true
        image.accessibilityDescription = text
        return image
    }
}
