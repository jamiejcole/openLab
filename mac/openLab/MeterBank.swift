// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import QuartzCore

/// What a meter shows: a cue's peaks, or an output patch's.
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
    /// Most channels kept per source, and most sources: a bound on what a push can make this keep.
    nonisolated static let maxChannels = 64
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
        for m in outputs { fold(.output(m.patch), m.peaks) }
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
    private var attachedTo: MeterBank?
    /// Redraws requested so far (tests count them: a view that cannot draw ignores `needsDisplay`).
    private(set) var invalidations = 0

    func invalidate() {
        invalidations += 1
        needsDisplay = true
    }
    private var observer: NSObjectProtocol?
    private var closeObserver: NSObjectProtocol?
    /// The window says it is covered or off screen (set by its occlusion notification, so a window
    /// that never reports one counts as showing).
    private var occluded = false

    static let rangeDb: Float = 60

    override var isFlipped: Bool { true }
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
                    self?.occluded = !w.occlusionState.contains(.visible)
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

    var isOnScreen: Bool {
        guard let w = window, !isHiddenOrHasHiddenAncestor else { return false }
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

    override func draw(_ dirtyRect: NSRect) {
        let count = source.flatMap { bank?.channelCount($0) } ?? 0
        let rows = max(count, 1)
        let gap: CGFloat = 1
        let h = max((bounds.height - gap * CGFloat(rows - 1)) / CGFloat(rows), 1)
        for row in 0..<rows {
            let y = CGFloat(row) * (h + gap)
            NSColor.quaternaryLabelColor.setFill()
            NSRect(x: 0, y: y, width: bounds.width, height: h).fill()
            guard count > 0, let source, let level = bank?.level(source, channel: row), level > 0 else { continue }
            (level >= 0.708 ? NSColor.systemRed : level >= 0.25 ? NSColor.systemYellow : NSColor.systemGreen)
                .setFill()
            NSRect(x: 0, y: y, width: bounds.width * Self.fill(level), height: h).fill()
        }
    }
}
