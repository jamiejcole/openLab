// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import os

/// The one table of every icon the app shows. A cue kind or Group mode names openLab's own drawing
/// (`drawing`, an SVG in the asset catalog, so another platform can use the same files). Every other
/// concept (a state, a transport button) names an SF Symbol, the macOS version that symbol first
/// shipped in and a fallback for an older system. The deployment target is macOS 12, which carries
/// SF Symbols 3.
///
/// `minMacOS` was read from the system's own `name_availability.plist` (CoreGlyphs): 2019 symbols
/// are macOS 10.15 (written 10), 2020 are 11, 2021 are 12. Every row is at or below 12, so none
/// needs an `#available` branch today; `image(_:)` still picks the fallback when the running system
/// is older than a row's minimum or the name does not resolve, and as a last resort draws the
/// localised name as text, so a slot is never silently blank.
enum Icon: CaseIterable {
    // Cue kinds (the toolbar and the cue list's type column).
    case kindAudio, kindGroup, kindMemo, kindWait, kindFade, kindStart, kindStop, kindPause, kindLoad
    case kindReset, kindDevamp, kindGoTo, kindArm, kindDisarm, kindNetwork
    // A Group cue's icon follows its mode (the timeline mode uses `kindGroup`).
    case kindGroupPlaylist, kindGroupEnter, kindGroupNext, kindGroupRandom
    // Cue states (UI-04: each non-idle state has its own shape).
    case stateLoaded, statePreWaiting, stateRunning, statePaused, statePostWaiting, stateFading
    case stateStopping, stateBroken
    // Swatch beside a colour name in the Inspector pop-up.
    case colourSwatch
    // Continue modes.
    case continueAuto, continueFollow
    // Transport strip and the Active Cues rows.
    case transportReset, transportPause, transportResume, transportPanic
    case activeCuePause, activeCueResume, activeCuePanic
    // Footer.
    case layoutToolbar, layoutMasthead, layoutInspector, layoutSidebar, settings
    // Engine status levels.
    case statusWaiting, statusNormal, statusWarning, statusAlarm
    // Cue list markers.
    case playhead, playheadInside, preview, outputOverridden, flagged, flaggedInside, warningMarker, engineAlarm
    // Time column header toggle (elapsed / remaining).
    case timeToggle, timeToggleUp
    // The Target column's button for a file cue (present and missing), and the Target field's clear, choose and jump glyphs.
    case targetFile, targetFileMissing, fieldClear, fieldChoose, fieldJump
    // Reserved for a screen that does not exist yet; it resolves and is tested already.
    case reservedPopOut

    struct Entry {
        let symbol: String
        let minMacOS: Int
        let fallback: String?
    }

    private static func row(_ symbol: String, _ minMacOS: Int = 10, fallback: String? = nil) -> Entry {
        Entry(symbol: symbol, minMacOS: minMacOS, fallback: fallback)
    }

    /// The asset-catalog name of openLab's own drawing for a cue kind or Group mode; nil for a
    /// concept shown as an SF Symbol.
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
        case .kindGroupPlaylist: return "kind-group-playlist"
        case .kindGroupEnter: return "kind-group-enter"
        case .kindGroupNext: return "kind-group-next"
        case .kindGroupRandom: return "kind-group-random"
        default: return nil
        }
    }

    /// The symbol row; nil for a concept with its own drawing. Every row above macOS 10 has a fallback.
    var entry: Entry? {
        switch self {
        case .kindAudio, .kindGroup, .kindMemo, .kindWait, .kindFade, .kindStart, .kindStop, .kindPause, .kindLoad,
            .kindReset, .kindDevamp, .kindGoTo, .kindArm, .kindDisarm, .kindNetwork, .kindGroupPlaylist,
            .kindGroupEnter, .kindGroupNext, .kindGroupRandom:
            return nil
        case .stateLoaded: return Self.row("arrow.down.circle.fill")
        case .statePreWaiting: return Self.row("hourglass")
        case .stateRunning: return Self.row("play.fill")
        case .statePaused: return Self.row("pause.fill")
        case .statePostWaiting: return Self.row("timer")
        case .stateFading: return Self.row("waveform")
        case .stateStopping: return Self.row("stop.fill")
        case .stateBroken: return Self.row("exclamationmark.triangle.fill")
        case .colourSwatch: return Self.row("circle.fill")
        case .continueAuto: return Self.row("arrow.down")
        case .continueFollow: return Self.row("arrow.turn.down.right")
        case .transportReset: return Self.row("backward.end")
        case .transportPause: return Self.row("pause.circle")
        case .transportResume: return Self.row("play.circle")
        case .transportPanic: return Self.row("exclamationmark.octagon")
        case .activeCuePause: return Self.row("pause.fill")
        case .activeCueResume: return Self.row("play.fill")
        case .activeCuePanic: return Self.row("exclamationmark.octagon")
        case .layoutToolbar: return Self.row("square.grid.2x2")
        case .layoutMasthead: return Self.row("rectangle.topthird.inset.filled", 12, fallback: "rectangle.split.3x1")
        case .layoutInspector:
            return Self.row("rectangle.bottomthird.inset.filled", 12, fallback: "rectangle.split.3x1")
        case .layoutSidebar: return Self.row("sidebar.right")
        case .settings: return Self.row("gearshape", 11, fallback: "gear")
        case .statusWaiting: return Self.row("circle.dashed", 11, fallback: "ellipsis.circle")
        case .statusNormal: return Self.row("checkmark.circle")
        case .statusWarning: return Self.row("exclamationmark.triangle.fill")
        case .statusAlarm: return Self.row("exclamationmark.octagon.fill")
        case .playhead: return Self.row("arrowtriangle.right.fill")
        case .playheadInside: return Self.row("arrowtriangle.right")
        case .preview: return Self.row("headphones")
        case .outputOverridden: return Self.row("circle.fill")
        case .flagged: return Self.row("flag.fill")
        case .flaggedInside: return Self.row("flag")
        case .warningMarker: return Self.row("exclamationmark.triangle.fill")
        case .engineAlarm: return Self.row("exclamationmark.triangle.fill")
        case .timeToggle: return Self.row("chevron.down")
        case .timeToggleUp: return Self.row("chevron.up")
        case .targetFile: return Self.row("arrow.down.right.circle")
        case .targetFileMissing: return Self.row("trash")
        case .fieldClear: return Self.row("xmark.circle.fill")
        case .fieldChoose: return Self.row("folder.fill")  // filled: the outline is the Group cue
        case .fieldJump: return Self.row("arrow.right.circle.fill")  // filled: the outline is a Group mode
        case .reservedPopOut: return Self.row("arrow.up.forward.square", 11, fallback: "rectangle.on.rectangle")
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

    /// The symbol name the table gives this concept; nil for one with its own drawing.
    var symbol: String? { entry?.symbol }

    /// The name to ask the system for: the fallback when this system is older than the row's minimum.
    private func nameForThisSystem(_ e: Entry) -> String {
        let atLeast = ProcessInfo.processInfo.isOperatingSystemAtLeast(
            OperatingSystemVersion(majorVersion: e.minMacOS, minorVersion: 0, patchVersion: 0))
        return atLeast ? e.symbol : (e.fallback ?? e.symbol)
    }

    /// The icon as an image, never nil. A symbol the system does not know (or a drawing missing from
    /// the bundle) is logged once and replaced by the fallback symbol; if that is also missing the
    /// image is `label` drawn as text (`compact`: its first letter, for a slot only 16 points wide).
    func image(_ label: String, compact: Bool = false) -> NSImage {
        if self == .transportPanic || self == .activeCuePanic { return Self.fadeOut(label) }
        guard let entry else {
            let name = drawing ?? ""
            if let image = IconLookup.shared.drawing(name, description: label) { return image }
            IconLookup.shared.reportMissing(name, concept: self)
            return IconLookup.textImage(label, compact: compact)
        }
        let first = nameForThisSystem(entry)
        if let image = IconLookup.shared.image(first, description: label) { return image }
        IconLookup.shared.reportMissing(first, concept: self)
        if let fb = entry.fallback, fb != first, let image = IconLookup.shared.image(fb, description: label) {
            return image
        }
        return IconLookup.textImage(label, compact: compact)
    }

    /// True when a symbol or drawing (not the text stand-in) shows for this icon on this system.
    var resolvesToSymbol: Bool {
        guard let entry else { return IconLookup.shared.drawing(drawing ?? "", description: nil) != nil }
        if IconLookup.shared.image(nameForThisSystem(entry), description: nil) != nil { return true }
        if let fb = entry.fallback { return IconLookup.shared.image(fb, description: nil) != nil }
        return false
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
