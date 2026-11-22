// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import QuartzCore

/// What a meter shows: a cue's peaks, or an output patch's cue outputs (each before the patch's
/// crosspoints, so a row shows what that cue output carries wherever it is routed).
enum MeterSource: Hashable {
    case cue(ModelID)
    case output(ModelID)
}

/// The latest `meters` peaks, folded into one level per channel and drawn at the display rate
/// (NFR-26). The engine pushes at up to 30 Hz; each push is max-folded into a preallocated slot,
/// and a tick (60 Hz while some meter is on screen) turns the folded peak into the shown level,
/// which falls at a fixed rate, and marks the views whose level changed. With no meter on screen
/// a push is dropped unread and no timer runs, so a hidden tab costs nothing.
@MainActor
final class MeterBank {
    /// Most channels kept per source (a patch's most cue outputs), and most sources: a bound on
    /// what a push can make this keep.
    nonisolated static let maxChannels = LevelSet.maxCueOutputs
    nonisolated static let maxSources = 256
    /// Fall rate of the shown level, in dB per second.
    static let fallDbPerSecond = 20.0
    /// A level below this (-80 dB) is silence.
    nonisolated static let floor: Float = 1e-4
    /// Ticks per second while any meter is on screen.
    static let tickRate = 60.0
    /// A source silent for this many ticks is forgotten.
    static let idleTicks = 120

    fileprivate struct Slot {
        var incoming = [Float](repeating: 0, count: MeterBank.maxChannels)
        var shown = [Float](repeating: 0, count: MeterBank.maxChannels)
        var count = 0
        var silentTicks = 0
        var dirty = false
        var changed = false
    }

    private var slots: [MeterSource: Slot] = [:]
    private struct WeakView { weak var view: MeterView? }
    /// Weak: a window closed with its views in place must still be freed.
    private var views = [ObjectIdentifier: WeakView]()
    private var stopTicker: (() -> Void)?
    private let startTicker: (@escaping @MainActor (Double) -> Void) -> () -> Void

    /// `startTicker` calls its handler (with the seconds since the last call) at the display rate
    /// until the returned closure is called; tests pass a fake that fires by hand.
    init(startTicker: ((@escaping @MainActor (Double) -> Void) -> () -> Void)? = nil) {
        self.startTicker = startTicker ?? MeterBank.timerTicker
        slots.reserveCapacity(32)
    }

    var visibleCount: Int { views.count }
    var isTicking: Bool { stopTicker != nil }
    var sourceCount: Int { slots.count }

    // MARK: Views come and go

    func attach(_ view: MeterView) {
        views[ObjectIdentifier(view)] = WeakView(view: view)
        if stopTicker == nil {
            stopTicker = startTicker { [weak self] dt in self?.tick(dt) }
        }
    }

    func detach(_ view: MeterView) {
        views[ObjectIdentifier(view)] = nil
        if views.isEmpty {
            stopTicker?()
            stopTicker = nil
            slots.removeAll(keepingCapacity: true)  // stale peaks must not greet the next view
        }
    }

    // MARK: Pushes

    func ingest(outputs: [OutputMeter], cues: [CueMeter]) {
        guard !views.isEmpty else { return }
        for m in outputs { fold(.output(m.patch), m.cueOutputs) }
        for m in cues { fold(.cue(m.cue), m.peaks) }
    }

    private func fold(_ source: MeterSource, _ peaks: [Float]) {
        if slots[source] == nil {
            guard slots.count < Self.maxSources else { return }
            slots[source] = Slot()
        }
        slots[source]?.merge(peaks)
    }

    /// Forgets every level (the engine run or the connection is gone).
    func reset() {
        for key in Array(slots.keys) {
            slots[key]?.clear()
        }
    }

    // MARK: Reading

    func channelCount(_ source: MeterSource) -> Int { slots[source]?.count ?? 0 }

    /// The shown linear level of `channel` (0 when unknown).
    func level(_ source: MeterSource, channel: Int) -> Float {
        guard let slot = slots[source], channel >= 0, channel < slot.count else { return 0 }
        return slot.shown[channel]
    }

    // MARK: Display tick

    /// One display frame: fold, fall, mark the views whose source changed, forget idle sources.
    func tick(_ dt: Double) {
        let fall = Float(pow(10.0, -Self.fallDbPerSecond * min(max(dt, 0), 1) / 20))
        var any = false
        var idle = false
        // In place through `values`: no copy of a slot's arrays and no key list per frame.
        for i in slots.values.indices {
            let changed = slots.values[i].advance(fall: fall)
            slots.values[i].changed = changed
            any = any || changed
            idle = idle || slots.values[i].silentTicks > Self.idleTicks
        }
        if any {
            for w in views.values {
                if let v = w.view, let s = v.source, slots[s]?.changed == true { v.invalidate() }
            }
        }
        if idle { slots = slots.filter { $0.value.silentTicks <= Self.idleTicks } }
    }

    private static func timerTicker(_ handler: @escaping @MainActor (Double) -> Void) -> () -> Void {
        var last = CACurrentMediaTime()
        let timer = Timer(timeInterval: 1 / tickRate, repeats: true) { _ in
            let now = CACurrentMediaTime()
            let dt = now - last
            last = now
            MainActor.assumeIsolated { handler(dt) }
        }
        RunLoop.main.add(timer, forMode: .common)
        return { timer.invalidate() }
    }
}

extension MeterBank.Slot {
    /// Folds one push into the peaks waiting for the next tick; never allocates.
    fileprivate mutating func merge(_ peaks: [Float]) {
        let n = min(peaks.count, MeterBank.maxChannels)
        count = max(count, n)
        for i in 0..<n {
            let p = peaks[i]
            // The wire never carries NaN or a negative; a hostile value is silence, not a crash.
            if p.isFinite, p > incoming[i] { incoming[i] = min(p, 8) }
        }
        dirty = true
    }

    fileprivate mutating func clear() {
        for i in 0..<MeterBank.maxChannels {
            incoming[i] = 0
            shown[i] = 0
        }
        dirty = true
    }

    /// Takes the waiting peaks, lets the shown level fall; true when anything shown changed.
    fileprivate mutating func advance(fall: Float) -> Bool {
        var changed = dirty
        var quiet = true
        for i in 0..<count {
            let old = shown[i]
            var new = max(incoming[i], old * fall)
            if new < MeterBank.floor { new = 0 }
            incoming[i] = 0
            shown[i] = new
            if new != old { changed = true }
            if new > 0 { quiet = false }
        }
        dirty = false
        silentTicks = quiet ? silentTicks + 1 : 0
        return changed
    }
}

/// Peak bars for one source, one thin bar per channel, drawn only while the view is on screen:
/// it registers with its bank when it has a window, is not hidden and the window is not occluded.
@MainActor
final class MeterView: NSView {
    weak var bank: MeterBank? {
        didSet { if oldValue !== bank { updateLive(old: oldValue) } }
    }
    var source: MeterSource? {
        didSet { if oldValue != source { invalidate() } }
    }
    /// Draws this many bars whatever the source has sent, so each lines up with a row beside it
    /// (the patch editor's cue outputs); nil: one bar per channel sent.
    var fixedRows: Int? {
        didSet { if oldValue != fixedRows { invalidate() } }
    }
    /// Upright bars, one per column, filling from the bottom: channel n's bar is `width` wide at
    /// `first + n * step` (the Levels tab puts one beside each cue output's fader). nil: the
    /// channels are stacked rows across the view.
    struct Columns: Equatable {
        var first: CGFloat
        var step: CGFloat
        var width: CGFloat
        var count: Int
    }
    var columns: Columns? {
        didSet { if oldValue != columns { invalidate() } }
    }
    private var attachedTo: MeterBank?
    #if DEBUG
        /// Redraws requested so far (tests count them: a view that cannot draw ignores `needsDisplay`).
        private(set) var invalidations = 0
        /// Columns the last draw painted (tests check a draw paints only what is in view).
        private(set) var drawnColumns = 0
    #endif

    func invalidate() {
        #if DEBUG
            invalidations += 1
        #endif
        // A row of columns can be far wider than what shows of it: only the part in view is drawn
        // again (a part scrolled into view later is drawn as it comes in).
        if columns != nil { setNeedsDisplay(visibleRect) } else { needsDisplay = true }
    }
    /// Whether a window is covered: asked when the view joins a window (`atJoin`) and on each
    /// occlusion change. At join only an active app is believed, as a headless host reports a
    /// non-visible state for windows that are on screen.
    var windowIsCovered = MeterView.defaultWindowIsCovered
    /// What new views ask (a test host overrides it for every view it makes).
    static var defaultWindowIsCovered: (_ window: NSWindow, _ atJoin: Bool) -> Bool = { w, atJoin in
        (!atJoin || NSApp.isActive) && !w.occlusionState.contains(.visible)
    }
    private var observer: NSObjectProtocol?
    private var closeObserver: NSObjectProtocol?
    /// The window is covered or off screen: seeded when the view joins it, then set by its
    /// occlusion notification (a window that reports nothing counts as showing).
    private var occluded = false

    static let rangeDb: Float = 60

    nonisolated override var isFlipped: Bool { true }
    override var intrinsicContentSize: NSSize { NSSize(width: 120, height: 22) }
    override var isOpaque: Bool { false }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if let observer { NotificationCenter.default.removeObserver(observer) }
        closeObserver.map(NotificationCenter.default.removeObserver)
        observer = nil
        closeObserver = nil
        occluded = false
        if let w = window {
            // A meter added to a window that is already covered must not run until the next
            // occlusion change.
            occluded = windowIsCovered(w, true)
            closeObserver = NotificationCenter.default.addObserver(
                forName: NSWindow.willCloseNotification, object: w, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated {
                    self?.occluded = true
                    self?.updateLive()
                }
            }
            observer = NotificationCenter.default.addObserver(
                forName: NSWindow.didChangeOcclusionStateNotification, object: w, queue: .main
            ) { [weak self, weak w] _ in
                MainActor.assumeIsolated {
                    guard let w else { return }
                    if let self { self.occluded = self.windowIsCovered(w, false) }
                    self?.updateLive()
                }
            }
        }
        updateLive()
    }

    override func viewDidHide() {
        super.viewDidHide()
        updateLive()
    }

    override func viewDidUnhide() {
        super.viewDidUnhide()
        updateLive()
    }

    #if DEBUG
        /// A scenario run draws windows that are never put on screen: their meters count as shown.
        static var countsWindowsOffScreen = false
    #endif

    var isOnScreen: Bool {
        guard let w = window, !isHiddenOrHasHiddenAncestor else { return false }
        #if DEBUG
            if Self.countsWindowsOffScreen { return !occluded }
        #endif
        return w.isVisible && !w.isMiniaturized && !occluded
    }

    /// Registers with the bank while on screen and leaves it otherwise.
    func updateLive(old: MeterBank? = nil) {
        if let old, old === attachedTo {
            old.detach(self)
            attachedTo = nil
        }
        let want = isOnScreen ? bank : nil
        if want === attachedTo { return }
        attachedTo?.detach(self)
        attachedTo = want
        want?.attach(self)
        invalidate()
    }

    /// 0...1 bar fill of a linear peak on a 60 dB scale.
    static func fill(_ level: Float) -> CGFloat {
        guard level > 0 else { return 0 }
        let db = 20 * log10(level)
        return CGFloat(min(max((db + rangeDb) / rangeDb, 0), 1))
    }

    /// Clicks go to what lies under the bars (the faders and the board beside them).
    override func hitTest(_ point: NSPoint) -> NSView? { columns == nil ? super.hitTest(point) : nil }

    private static func colour(_ level: Float) -> NSColor {
        level >= 0.708 ? .systemRed : level >= 0.25 ? .systemYellow : .systemGreen
    }

    override func draw(_ dirtyRect: NSRect) {
        NSBezierPath.clip(bounds)
        if let columns { return drawColumns(columns, in: dirtyRect) }
        let count = source.flatMap { bank?.channelCount($0) } ?? 0
        // A source with nothing to show (no channels sent yet) draws nothing, not an empty bar.
        if fixedRows == nil && count == 0 { return }
        let rows = shownRows
        for row in 0..<rows {
            let r = rowRect(row)
            NSColor.quaternaryLabelColor.setFill()
            r.fill()
            guard count > 0, let source, let level = bank?.level(source, channel: row), level > 0 else { continue }
            Self.colour(level).setFill()
            NSRect(x: r.minX, y: r.minY, width: r.width * Self.fill(level), height: r.height).fill()
        }
    }

    /// The rows drawn when the channels are stacked rows (`columns` nil).
    private var shownRows: Int {
        fixedRows.map { max($0, 1) } ?? max(source.flatMap { bank?.channelCount($0) } ?? 0, 1)
    }

    /// Where channel `row`'s bar is drawn when the channels are stacked rows. Every row stays inside
    /// the view: with many channels the gaps go first, then the rows get thinner than a point.
    func rowRect(_ row: Int) -> NSRect {
        let rows = shownRows
        var gap: CGFloat = 1
        if (bounds.height - gap * CGFloat(rows - 1)) / CGFloat(rows) < 1 { gap = 0 }
        let h = (bounds.height - gap * CGFloat(rows - 1)) / CGFloat(rows)
        return NSRect(x: 0, y: CGFloat(row) * (h + gap), width: bounds.width, height: h)
    }

    /// Only the columns in `dirty`: a tick redraws what is in view, not all 128. As for rows, a
    /// channel not sent (the cue is not playing) draws nothing, not an empty bar.
    private func drawColumns(_ c: Columns, in dirty: NSRect) {
        guard let source, let bank, c.count > 0, c.step > 0 else { return }
        let first = max(0, Int(((dirty.minX - c.first - c.width) / c.step).rounded(.down)))
        let last = min(
            c.count - 1, Int(((dirty.maxX - c.first) / c.step).rounded(.down)), bank.channelCount(source) - 1)
        #if DEBUG
            drawnColumns = max(0, last - first + 1)
        #endif
        guard first <= last else { return }
        for n in first...last {
            let track = NSRect(x: c.first + CGFloat(n) * c.step, y: 0, width: c.width, height: bounds.height)
            NSColor.quaternaryLabelColor.setFill()
            track.fill()
            let level = bank.level(source, channel: n)
            guard level > 0 else { continue }
            let h = track.height * Self.fill(level)
            Self.colour(level).setFill()
            NSRect(x: track.minX, y: track.maxY - h, width: track.width, height: h).fill()
        }
    }
}
