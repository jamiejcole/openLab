// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import os

/// The one table of every SF Symbol the app shows. A concept (a cue kind, a state, a transport
/// button) names its symbol, the macOS version that symbol first shipped in and a fallback for an
/// older system. The deployment target is macOS 12, which carries SF Symbols 3.
///
/// `minMacOS` was read from the system's own `name_availability.plist` (CoreGlyphs): 2019 symbols
/// are macOS 10.15 (written 10), 2020 are 11, 2021 are 12. Every row is at or below 12, so none
/// needs an `#available` branch today; `image(_:)` still picks the fallback when the running system
/// is older than a row's minimum or the name does not resolve, and as a last resort draws the
/// localised name as text, so a slot is never silently blank.
enum Icon: CaseIterable {
    // Cue kinds (the toolbar and the cue list's type column).
    case kindAudio, kindGroup, kindMemo, kindWait, kindFade, kindStart, kindStop, kindPause, kindLoad
    case kindReset, kindDevamp, kindGoTo, kindTarget, kindArm, kindDisarm, kindNetwork
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
    case playhead, playheadInside, preview, flagged, flaggedInside, warningMarker, engineAlarm
    // Time column header toggle (elapsed / remaining).
    case timeToggle, timeToggleUp
    // Reserved for screens that do not exist yet; they resolve and are tested already.
    case reservedTargetCue, reservedTargetFile, reservedFieldClear, reservedFieldChoose, reservedPopOut

    struct Entry {
        let symbol: String
        let minMacOS: Int
        let fallback: String?
    }

    private static func row(_ symbol: String, _ minMacOS: Int = 10, fallback: String? = nil) -> Entry {
        Entry(symbol: symbol, minMacOS: minMacOS, fallback: fallback)
    }

    /// Every row above macOS 10 has a fallback.
    var entry: Entry {
        switch self {
        case .kindAudio: return Self.row("speaker.wave.2", 11, fallback: "speaker.2")
        case .kindGroup: return Self.row("folder")
        case .kindMemo: return Self.row("note.text", 11, fallback: "doc.text")
        case .kindWait: return Self.row("timer")
        case .kindFade: return Self.row("slider.horizontal.3")
        case .kindStart: return Self.row("play.fill")
        case .kindStop: return Self.row("stop.fill")
        case .kindPause: return Self.row("pause.fill")
        case .kindLoad: return Self.row("arrow.down.circle")
        case .kindReset: return Self.row("arrow.counterclockwise")
        case .kindDevamp: return Self.row("arrow.uturn.forward", 11, fallback: "arrow.uturn.right")
        case .kindGoTo: return Self.row("arrow.right.to.line")
        case .kindTarget: return Self.row("scope")
        case .kindArm: return Self.row("record.circle", 11, fallback: "largecircle.fill.circle")
        case .kindDisarm: return Self.row("slash.circle")
        case .kindNetwork: return Self.row("network", 11, fallback: "globe")
        case .kindGroupPlaylist: return Self.row("list.bullet")
        case .kindGroupEnter: return Self.row("arrow.right.circle")
        case .kindGroupNext: return Self.row("forward.end")
        case .kindGroupRandom: return Self.row("shuffle")
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
        case .transportReset: return Self.row("stop.circle")
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
        case .flagged: return Self.row("flag.fill")
        case .flaggedInside: return Self.row("flag")
        case .warningMarker: return Self.row("exclamationmark.triangle.fill")
        case .engineAlarm: return Self.row("exclamationmark.triangle.fill")
        case .timeToggle: return Self.row("chevron.down")
        case .timeToggleUp: return Self.row("chevron.up")
        case .reservedTargetCue: return Self.row("scope")
        case .reservedTargetFile: return Self.row("doc")
        case .reservedFieldClear: return Self.row("xmark.circle.fill")
        case .reservedFieldChoose: return Self.row("magnifyingglass")
        case .reservedPopOut: return Self.row("arrow.up.forward.square", 11, fallback: "rectangle.on.rectangle")
        }
    }

    /// The symbol name the table gives this concept.
    var symbol: String { entry.symbol }

    /// The name to ask the system for: the fallback when this system is older than the row's minimum.
    var nameForThisSystem: String {
        let e = entry
        let atLeast = ProcessInfo.processInfo.isOperatingSystemAtLeast(
            OperatingSystemVersion(majorVersion: e.minMacOS, minorVersion: 0, patchVersion: 0))
        return atLeast ? e.symbol : (e.fallback ?? e.symbol)
    }

    /// The icon as an image, never nil. A symbol the system does not know is logged once and replaced
    /// by the fallback symbol; if that is also missing the image is `label` drawn as text
    /// (`compact`: its first letter, for a slot only 16 points wide).
    func image(_ label: String, compact: Bool = false) -> NSImage {
        let first = nameForThisSystem
        if let image = IconLookup.shared.image(first, description: label) { return image }
        IconLookup.shared.reportMissing(first, concept: self)
        if let fb = entry.fallback, fb != first, let image = IconLookup.shared.image(fb, description: label) {
            return image
        }
        return IconLookup.textImage(label, compact: compact)
    }

    /// True when a symbol (not the text stand-in) shows for this icon on this system.
    var resolvesToSymbol: Bool {
        if IconLookup.shared.image(nameForThisSystem, description: nil) != nil { return true }
        if let fb = entry.fallback { return IconLookup.shared.image(fb, description: nil) != nil }
        return false
    }
}

/// The system lookup behind `Icon`, with a seam so a test can make a name vanish (a symbol the
/// running macOS lacks) and see the text stand-in. Each missing name is logged once.
final class IconLookup: @unchecked Sendable {
    static let shared = IconLookup()
    private static let systemHas: @Sendable (String) -> Bool = {
        NSImage(systemSymbolName: $0, accessibilityDescription: nil) != nil
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

    func reportMissing(_ name: String, concept: Icon) {
        lock.lock()
        let first = reported.insert(name).inserted
        lock.unlock()
        if first {
            log.error(
                "SF Symbol \(name, privacy: .public) is missing for \(String(describing: concept), privacy: .public)")
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
