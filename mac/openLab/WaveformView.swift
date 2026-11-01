// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// File seconds to view points and back, for a visible stretch `start..<end` of the file drawn
/// across `width` points.
struct WaveformScale: Equatable {
    var start: Double
    var end: Double
    var width: CGFloat

    func x(_ seconds: Double) -> CGFloat {
        guard end > start, width > 0 else { return 0 }
        return CGFloat((seconds - start) / (end - start)) * width
    }

    func seconds(_ x: CGFloat) -> Double {
        guard width > 0 else { return start }
        return start + Double(x / width) * (end - start)
    }
}

/// What the waveform draws (AUD-14), worked out once per set of peaks and size so a redraw (the
/// playhead moving, a handle dragged) only strokes what is here. Each file channel has a lane of
/// its own, stacked top to bottom as QLab does; each pixel column is a vertical line from the
/// highest to the lowest sample under it. Pure: tests check it without drawing.
struct WaveformDrawing: Equatable {
    struct Column: Equatable {
        let x: CGFloat
        let top: CGFloat
        let bottom: CGFloat
    }
    struct Lane: Equatable {
        let rect: CGRect
        let columns: [Column]
    }
    let lanes: [Lane]

    static let empty = WaveformDrawing(lanes: [])

    init(lanes: [Lane]) { self.lanes = lanes }

    /// `peaks` drawn in `rect` (flipped: y grows down) at `scale`; columns outside the peaks' range
    /// are left out.
    init(peaks: WaveformPeaks, scale: WaveformScale, in rect: CGRect) {
        let count = peaks.channels.count
        let columns = Int(rect.width.rounded(.down))
        guard count > 0, columns > 0, peaks.end > peaks.start else {
            self.lanes = []
            return
        }
        let laneHeight = rect.height / CGFloat(count)
        lanes = peaks.channels.enumerated().map { c, channel in
            let lane = CGRect(
                x: rect.minX, y: rect.minY + CGFloat(c) * laneHeight, width: rect.width, height: laneHeight)
            let n = min(channel.min.count, channel.max.count)
            guard n > 0 else { return Lane(rect: lane, columns: []) }
            let perBucket = (peaks.end - peaks.start) / Double(n)
            let half = (lane.height / 2) * 0.95
            var out: [Column] = []
            out.reserveCapacity(columns)
            for px in 0..<columns {
                let t0 = scale.seconds(CGFloat(px))
                let t1 = scale.seconds(CGFloat(px + 1))
                guard t1 > peaks.start, t0 < peaks.end else { continue }
                let first = max(0, Int(((t0 - peaks.start) / perBucket).rounded(.down)))
                let last = min(n, max(first + 1, Int(((t1 - peaks.start) / perBucket).rounded(.up))))
                guard first < last else { continue }
                var lo = Int16.max
                var hi = Int16.min
                for i in first..<last {
                    lo = min(lo, channel.min[i])
                    hi = max(hi, channel.max[i])
                }
                let top = lane.midY - CGFloat(Float(hi) / WaveformPeaks.fullScale) * half
                let bottom = lane.midY - CGFloat(Float(lo) / WaveformPeaks.fullScale) * half
                // Silence still shows as a hairline, as a centre line does.
                out.append(Column(x: rect.minX + CGFloat(px) + 0.5, top: top, bottom: max(bottom, top + 1)))
            }
            return Lane(rect: lane, columns: out)
        }
    }
}

/// The waveform of an Audio cue's file with its trims, slices and built-in fades (AUD-10, AUD-11,
/// AUD-12, AUD-14): a time ruler on top with the start and end handles at its ends, the file's
/// channels stacked below, the trimmed-off parts dimmed, slice markers with each slice's play count
/// along the bottom, the fade-in and fade-out envelope along the top, and the playhead while the
/// cue plays. Dragging a handle moves that trim, marker or fade; a trim's new time is reported as
/// it moves, and every drag is committed once when the mouse goes up. A slice marker dragged out
/// of the view is deleted (QLab: drag its handle out of the waveform). A click elsewhere places the
/// insertion point that a new slice marker goes to.
@MainActor
class WaveformView: NSView {
    enum Handle: Equatable {
        case start, end
        case slice(Int)
        case fadeIn, fadeOut
    }

    static let rulerHeight: CGFloat = 18
    /// How near a handle (points) a click must be to take it.
    static let grab: CGFloat = 6
    /// The band along the top of the waves where the fade handles are taken, and along the bottom
    /// where the slice play counts are (double-click to edit one), points.
    static let fadeBand: CGFloat = 12
    static let countBand: CGFloat = 16

    var scale = WaveformScale(start: 0, end: 1, width: 0) {
        didSet {
            if scale != oldValue {
                drawing = nil
                needsDisplay = true
                window?.invalidateCursorRects(for: self)
            }
        }
    }
    var peaks: WaveformPeaks? {
        didSet {
            if peaks != oldValue {
                drawing = nil
                needsDisplay = true
            }
        }
    }
    /// The file's length; nil while it is not known (nothing to draw or drag).
    var duration: Double? { didSet { needsDisplay = true } }
    /// The trims, file seconds; `end` nil is the end of the file.
    var trimStart: Double = 0 { didSet { needsDisplay = true } }
    var trimEnd: Double? { didSet { needsDisplay = true } }
    /// Where the cue is playing in the file, or nil.
    var playhead: Double? {
        didSet {
            guard playhead != oldValue else { return }
            // Only the two thin strips the line leaves and enters are drawn again.
            for t in [oldValue, playhead].compactMap({ $0 }) {
                setNeedsDisplay(NSRect(x: scale.x(t) - 2, y: 0, width: 4, height: bounds.height))
            }
        }
    }
    /// Slice markers (file seconds, increasing) and each slice's play count, one more than the
    /// markers (0 = until devamped). While a marker is dragged the view keeps its own copy.
    var sliceMarks: [Double] = [] { didSet { needsDisplay = true } }
    var sliceCounts: [Int] = [] { didSet { needsDisplay = true } }
    /// The built-in fades (seconds from the start trim and back from the end trim) and their curve
    /// (core's `FadeCurve` name); drawn and draggable only while `fadesShown`.
    var fadesShown = false { didSet { needsDisplay = true } }
    var fadeIn: Double = 0 { didSet { needsDisplay = true } }
    var fadeOut: Double = 0 { didSet { needsDisplay = true } }
    var fadeCurve = "linear" { didSet { needsDisplay = true } }
    /// Where a click left the insertion point (file seconds), or nil.
    var insertion: Double? { didSet { needsDisplay = true } }
    var isLocked = false
    /// A text shown in the middle when there is nothing to draw ("reading", why not).
    var message: String? { didSet { needsDisplay = true } }
    /// A handle moved (live, during the drag).
    var onTrimChanging: ((Handle, Double) -> Void)?
    /// A drag moved a handle: its new time (for the end, nil is the end of the file).
    var onTrimCommit: ((Handle, Double?) -> Void)?
    /// A drag moved the slice marker that was at `from` to `to`, or (nil) took it out of the view.
    var onSliceCommit: ((_ from: Double, _ to: Double?) -> Void)?
    /// A drag ended, committed or not: what the view kept for it is to be shown afresh.
    var onDragEnded: (() -> Void)?
    /// A drag changed the fades: the new fade-in and fade-out lengths.
    var onFadesCommit: ((Double, Double) -> Void)?
    /// The insertion point moved.
    var onInsertion: (() -> Void)?
    /// A slice's play count was double-clicked: edit slice `index`'s count.
    var onEditCount: ((Int) -> Void)?
    /// The width changed: the peaks drawn no longer fit it.
    var onResize: (() -> Void)?
    /// A gesture, wheel or key asks to zoom: the factor on the visible span (below 1 zooms in) and
    /// where in the view's width (0...1) the time under it stays.
    var onZoom: ((_ factor: Double, _ anchor: Double) -> Void)?
    /// A scroll asks to pan: the fraction of the view's width the content moves right by.
    var onPan: ((_ fraction: Double) -> Void)?

    private(set) var dragging: Handle?
    /// The dragged slice marker is outside the view: letting go there deletes it.
    private(set) var removing = false
    /// The trims, marker and fades when the drag began: letting go there sends nothing.
    private var dragOrigin: (start: Double, end: Double?) = (0, nil)
    private var markOrigin: Double = 0
    private var fadeOrigin: (in: Double, out: Double) = (0, 0)
    private var drawing: WaveformDrawing?
    /// The width changed during a live resize: [`onResize`] is owed when it ends.
    private var resizedLive = false

    override var isFlipped: Bool { true }

    var waveRect: CGRect {
        CGRect(x: 0, y: Self.rulerHeight, width: bounds.width, height: max(0, bounds.height - Self.rulerHeight))
    }

    var endTime: Double { trimEnd ?? duration ?? 0 }

    override func setFrameSize(_ newSize: NSSize) {
        let changed = newSize.width != frame.width
        super.setFrameSize(newSize)
        guard changed else { return }
        scale.width = newSize.width
        // While the window is dragged to a new size the drawing stretches to fit; the peaks for
        // the new width are asked for once, when the drag ends.
        if inLiveResize {
            resizedLive = true
        } else {
            onResize?()
        }
    }

    override func viewDidEndLiveResize() {
        super.viewDidEndLiveResize()
        if resizedLive {
            resizedLive = false
            onResize?()
        }
    }

    /// The drawing for the current peaks and size, built once.
    func currentDrawing() -> WaveformDrawing {
        if let drawing { return drawing }
        let d = peaks.map { WaveformDrawing(peaks: $0, scale: scale, in: waveRect) } ?? .empty
        drawing = d
        return d
    }

    // MARK: Dragging (also the seam tests drive)

    /// Takes the handle under `x` (at height `y`; the default is the ruler), if any: a fade handle
    /// in the band along the top of the waves, otherwise the nearest trim handle or slice marker
    /// (not in Show Mode, nor before the file's length is known).
    @discardableResult
    func beginDrag(atX x: CGFloat, y: CGFloat = 0) -> Bool {
        guard !isLocked, duration != nil else { return false }
        removing = false
        if fadesShown, y >= waveRect.minY, y <= waveRect.minY + Self.fadeBand {
            let di = abs(scale.x(trimStart + fadeIn) - x)
            let dout = abs(scale.x(endTime - fadeOut) - x)
            if min(di, dout) <= Self.grab {
                dragging = di <= dout ? .fadeIn : .fadeOut
                fadeOrigin = (fadeIn, fadeOut)
                return true
            }
        }
        var best: (handle: Handle, distance: CGFloat)?
        let marks = sliceMarks.enumerated().map { (Handle.slice($0), $1) }
        for (handle, t) in [(Handle.start, trimStart), (.end, endTime)] + marks {
            let d = abs(scale.x(t) - x)
            if d <= Self.grab, d < (best?.distance ?? .infinity) { best = (handle, d) }
        }
        guard let handle = best?.handle else { return false }
        dragging = handle
        dragOrigin = (trimStart, trimEnd)
        if case .slice(let i) = handle { markOrigin = sliceMarks[i] }
        return true
    }

    /// Moves what is being dragged to `x`; a slice marker dragged above or below the view (`y`
    /// outside it) is shown as gone.
    func drag(toX x: CGFloat, y: CGFloat = 0) {
        guard let dragging, let duration else { return }
        let t = scale.seconds(x)
        switch dragging {
        case .start:
            // Core drops a marker the trims pass (with a warning): the trims stop short of them.
            trimStart = Self.clampStart(t, end: sliceMarks.first ?? endTime)
            onTrimChanging?(.start, trimStart)
        case .end:
            let e = Self.clampEnd(t, start: sliceMarks.last ?? trimStart, duration: duration)
            trimEnd = e
            onTrimChanging?(.end, e)
        case .slice(let i):
            guard sliceMarks.indices.contains(i) else { return }
            removing = y < 0 || y > bounds.height
            let lo = (i == 0 ? trimStart : sliceMarks[i - 1]) + Self.minLength
            let hi = (i == sliceMarks.count - 1 ? endTime : sliceMarks[i + 1]) - Self.minLength
            if lo <= hi { sliceMarks[i] = Self.clampRounded(t, lo, hi) }
        case .fadeIn:
            fadeIn = Self.clampRounded(t - trimStart, 0, max(0, endTime - trimStart - fadeOut))
        case .fadeOut:
            fadeOut = Self.clampRounded(endTime - t, 0, max(0, endTime - trimStart - fadeIn))
        }
    }

    /// Commits the drag when it moved the handle from where it was taken. An end at the file's end
    /// is no trim at all (nil), as if never set. A slice marker let go outside the view is deleted.
    func endDrag() {
        guard let handle = dragging else { return }
        dragging = nil
        defer { onDragEnded?() }
        switch handle {
        case .start:
            if trimStart != dragOrigin.start { onTrimCommit?(.start, trimStart) }
        case .end:
            let end: Double? = endTime >= (duration ?? .infinity) ? nil : endTime
            trimEnd = end
            if end != dragOrigin.end { onTrimCommit?(.end, end) }
        case .slice(let i):
            guard sliceMarks.indices.contains(i) else { return }
            if removing {
                removing = false
                sliceMarks.remove(at: i)
                if sliceCounts.indices.contains(i) { sliceCounts.remove(at: i) }
                onSliceCommit?(markOrigin, nil)
            } else if sliceMarks[i] != markOrigin {
                onSliceCommit?(markOrigin, sliceMarks[i])
            }
        case .fadeIn, .fadeOut:
            if fadeIn != fadeOrigin.in || fadeOut != fadeOrigin.out { onFadesCommit?(fadeIn, fadeOut) }
        }
    }

    /// A click away from every handle: the insertion point goes there, inside the file.
    func click(atX x: CGFloat) {
        guard !isLocked, let duration else { return }
        insertion = Self.clampRounded(scale.seconds(x), 0, duration)
        onInsertion?()
    }

    /// The slice (0-based, the last after the final marker) that plays file time `t`, or nil
    /// when `t` is outside the trims or there are no markers.
    func slice(at t: Double) -> Int? {
        guard !sliceMarks.isEmpty, t >= trimStart, t <= endTime else { return nil }
        return sliceMarks.filter { $0 <= t }.count
    }

    /// A double-click on a slice's play count asks to edit it.
    func doubleClick(atX x: CGFloat, y: CGFloat) {
        guard !isLocked, y >= waveRect.maxY - Self.countBand, let i = slice(at: scale.seconds(x)) else { return }
        onEditCount?(i)
    }

    /// Where slice `index`'s play count is drawn: the middle of the visible part of the slice,
    /// along the bottom of the waves.
    func countRect(_ index: Int) -> NSRect {
        let a = index > 0 && index <= sliceMarks.count ? sliceMarks[index - 1] : trimStart
        let b = index < sliceMarks.count ? sliceMarks[index] : endTime
        let mid = (max(scale.x(a), 0) + min(scale.x(b), bounds.width)) / 2
        return NSRect(x: mid - 25, y: waveRect.maxY - Self.countBand, width: 50, height: Self.countBand)
    }

    /// The shortest stretch core keeps between the trims (`MIN_SLICE_SECONDS`).
    static let minLength = 0.05

    static func clampStart(_ t: Double, end: Double) -> Double {
        rounded(min(max(0, t), max(0, end - minLength)))
    }

    static func clampEnd(_ t: Double, start: Double, duration: Double) -> Double {
        rounded(max(min(duration, t), min(duration, start + minLength)))
    }

    /// To the millisecond, as the fields show and core keeps.
    private static func rounded(_ t: Double) -> Double { (t * 1000).rounded() / 1000 }

    /// `t` within `lo...hi`, to the millisecond unless that would leave the range (core keeps
    /// markers 50 ms apart to within 1 ns, so a bound is never rounded past).
    static func clampRounded(_ t: Double, _ lo: Double, _ hi: Double) -> Double {
        min(max(rounded(min(max(t, lo), hi)), lo), hi)
    }

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        let p = convert(event.locationInWindow, from: nil)
        if event.clickCount == 2 {
            doubleClick(atX: p.x, y: p.y)
        } else if !beginDrag(atX: p.x, y: p.y) {
            click(atX: p.x)
        }
    }

    override func mouseDragged(with event: NSEvent) {
        let p = convert(event.locationInWindow, from: nil)
        drag(toX: p.x, y: p.y)
    }

    override func mouseUp(with event: NSEvent) { endDrag() }

    // MARK: Zoom and pan

    override var acceptsFirstResponder: Bool { true }

    private func anchor(of event: NSEvent) -> Double {
        guard bounds.width > 0 else { return 0.5 }
        let x = convert(event.locationInWindow, from: nil).x
        return Double(min(max(x / bounds.width, 0), 1))
    }

    /// Pinch: spreading the fingers zooms in about the pointer.
    override func magnify(with event: NSEvent) {
        let m = Double(event.magnification)
        guard m.isFinite, m > -0.9 else { return }
        onZoom?(1 / (1 + m), anchor(of: event))
    }

    /// Option + vertical scroll or a mouse wheel zooms about the pointer (up zooms in); on a
    /// trackpad a horizontal scroll pans, and Shift + wheel pans for a mouse.
    override func scrollWheel(with event: NSEvent) {
        let precise = event.hasPreciseScrollingDeltas
        let dx = Double(event.scrollingDeltaX)
        let dy = Double(event.scrollingDeltaY)
        let width = max(1, Double(bounds.width))
        if !precise && event.modifierFlags.contains(.shift) {
            let d = dx != 0 ? dx : dy
            if d != 0 { onPan?(d * Self.wheelPoints / width) }
        } else if event.modifierFlags.contains(.option) || !precise {
            guard dy != 0 else { return }
            onZoom?(exp(-dy * (precise ? 0.02 : 0.2)), anchor(of: event))
        } else if abs(dx) > abs(dy) {
            onPan?(dx / width)
        } else {
            super.scrollWheel(with: event)
        }
    }

    /// Points a mouse-wheel notch moves a pan.
    static let wheelPoints = 20.0

    /// Cmd-= (or Cmd-+) zooms in and Cmd-- out about the middle while the waveform is focused.
    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        guard window?.firstResponder === self,
            event.modifierFlags.intersection(.deviceIndependentFlagsMask).subtracting(.shift) == .command,
            let key = event.charactersIgnoringModifiers
        else { return super.performKeyEquivalent(with: event) }
        switch key {
        case "=", "+": onZoom?(0.5, 0.5)
        case "-": onZoom?(2, 0.5)
        default: return super.performKeyEquivalent(with: event)
        }
        return true
    }

    override func resetCursorRects() {
        guard !isLocked, duration != nil else { return }
        for t in [trimStart, endTime] + sliceMarks {
            addCursorRect(
                NSRect(x: scale.x(t) - Self.grab, y: 0, width: Self.grab * 2, height: bounds.height),
                cursor: .resizeLeftRight)
        }
    }

    // MARK: Drawing

    override func draw(_ dirtyRect: NSRect) {
        NSColor(calibratedRed: 0.02, green: 0.02, blue: 0.1, alpha: 1).setFill()
        waveRect.fill()
        NSColor.controlBackgroundColor.setFill()
        NSRect(x: 0, y: 0, width: bounds.width, height: Self.rulerHeight).fill()
        drawRuler(dirtyRect)
        let wave = waveRect
        NSColor.systemTeal.setStroke()
        for lane in currentDrawing().lanes {
            let path = NSBezierPath()
            path.lineWidth = 1
            for c in lane.columns where c.x >= dirtyRect.minX - 1 && c.x <= dirtyRect.maxX + 1 {
                path.move(to: NSPoint(x: c.x, y: c.top))
                path.line(to: NSPoint(x: c.x, y: c.bottom))
            }
            path.stroke()
        }
        // Channel separators.
        NSColor.white.withAlphaComponent(0.15).setFill()
        for lane in currentDrawing().lanes.dropFirst() {
            NSRect(x: 0, y: lane.rect.minY, width: wave.width, height: 1).fill()
        }
        if duration != nil {
            // What the trims leave out is dimmed.
            NSColor.black.withAlphaComponent(0.55).setFill()
            let a = scale.x(trimStart)
            let b = scale.x(endTime)
            NSRect(x: 0, y: wave.minY, width: max(0, a), height: wave.height).fill()
            NSRect(x: b, y: wave.minY, width: max(0, bounds.width - b), height: wave.height).fill()
            drawHandle(at: a, start: true)
            drawHandle(at: b, start: false)
            drawSlices()
            if fadesShown { drawFades() }
            if let insertion {
                NSColor.white.withAlphaComponent(0.7).setFill()
                NSRect(x: scale.x(insertion) - 0.5, y: wave.minY, width: 1, height: wave.height).fill()
            }
        }
        if let playhead {
            NSColor.systemYellow.setFill()
            NSRect(x: scale.x(playhead) - 0.5, y: wave.minY, width: 1, height: wave.height).fill()
        }
        if let message, peaks == nil {
            let attrs: [NSAttributedString.Key: Any] = [
                .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize),
                .foregroundColor: NSColor.secondaryLabelColor,
            ]
            let size = message.size(withAttributes: attrs)
            message.draw(
                at: NSPoint(x: wave.midX - size.width / 2, y: wave.midY - size.height / 2), withAttributes: attrs)
        }
    }

    /// Each marker as a green line with a handle in the ruler (the one being dragged out, not at
    /// all), and each slice's play count along the bottom (∞: until devamped).
    private func drawSlices() {
        guard !sliceMarks.isEmpty else { return }
        let wave = waveRect
        for (i, t) in sliceMarks.enumerated() where !(removing && dragging == .slice(i)) {
            let x = scale.x(t)
            NSColor.systemGreen.setFill()
            NSRect(x: x - 0.5, y: wave.minY, width: 1, height: wave.height).fill()
            NSRect(x: x - 3, y: Self.rulerHeight - 8, width: 6, height: 8).fill()
        }
        let attrs: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedDigitSystemFont(ofSize: 10, weight: .semibold),
            .foregroundColor: NSColor.systemGreen,
        ]
        for i in 0...sliceMarks.count {
            let count = sliceCounts.indices.contains(i) ? sliceCounts[i] : 1
            let text = count == 0 ? "\u{221E}" : String(count)
            let r = countRect(i)
            let size = text.size(withAttributes: attrs)
            text.draw(at: NSPoint(x: r.midX - size.width / 2, y: r.midY - size.height / 2), withAttributes: attrs)
        }
    }

    /// The fade-in rising on its curve from the start trim and the fade-out falling to the end
    /// trim, across the height of the waves, with a handle at the top of each.
    private func drawFades() {
        let wave = waveRect.insetBy(dx: 0, dy: 3)
        let a = trimStart
        let b = endTime
        let steps = 32
        func point(_ t: Double, _ gain: Double) -> NSPoint {
            NSPoint(x: scale.x(t), y: wave.maxY - CGFloat(gain) * wave.height)
        }
        let path = NSBezierPath()
        path.lineWidth = 1.5
        path.move(to: point(a, fadeIn > 0 ? 0 : 1))
        if fadeIn > 0 {
            for k in 1...steps {
                let f = Double(k) / Double(steps)
                path.line(to: point(a + f * fadeIn, Self.shape(fadeCurve, f)))
            }
        }
        path.line(to: point(b - fadeOut, 1))
        if fadeOut > 0 {
            for k in 1...steps {
                let f = Double(k) / Double(steps)
                path.line(to: point(b - fadeOut + f * fadeOut, Self.shape(fadeCurve, 1 - f)))
            }
        }
        NSColor.systemYellow.setStroke()
        path.stroke()
        NSColor.systemYellow.setFill()
        for t in [a + fadeIn, b - fadeOut] {
            NSRect(x: scale.x(t) - 3, y: wave.minY - 3, width: 6, height: 6).fill()
        }
    }

    /// A rising fade's gain at fraction `t` on core's curve `curve` (docs/audio.md, Curves); a
    /// falling one at fraction `t` is `shape(1 - t)`. For drawing only: the engine computes what is
    /// heard.
    static func shape(_ curve: String, _ t: Double) -> Double {
        let t = min(max(t, 0), 1)
        let k = log(1000.0)
        let e = { (t: Double) in (exp(k * t) - 1) / (exp(k) - 1) }
        switch curve {
        case "equal_power": return sin(Double.pi / 2 * t)
        case "s_curve": return (1 - cos(Double.pi * t)) / 2
        case "exponential": return e(t)
        case "logarithmic": return 1 - e(1 - t)
        default: return t
        }
    }

    private func drawHandle(at x: CGFloat, start: Bool) {
        NSColor.systemOrange.setFill()
        NSRect(x: x - 0.5, y: Self.rulerHeight, width: 1, height: waveRect.height).fill()
        let tri = NSBezierPath()
        let w: CGFloat = 8
        tri.move(to: NSPoint(x: x, y: Self.rulerHeight))
        tri.line(to: NSPoint(x: x, y: 2))
        tri.line(to: NSPoint(x: start ? x + w : x - w, y: 2))
        tri.close()
        tri.fill()
    }

    /// Ticks at a step that leaves room for a label.
    private func drawRuler(_ dirtyRect: NSRect) {
        guard scale.end > scale.start, bounds.width > 0 else { return }
        let span = scale.end - scale.start
        let minStep = span * 70 / Double(bounds.width)
        let steps: [Double] = [0.01, 0.05, 0.1, 0.25, 0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 1800, 3600]
        let step = steps.first { $0 >= minStep } ?? 3600
        let attrs: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedDigitSystemFont(ofSize: 9, weight: .regular),
            .foregroundColor: NSColor.secondaryLabelColor,
        ]
        NSColor.tertiaryLabelColor.setFill()
        var t = (scale.start / step).rounded(.up) * step
        while t <= scale.end {
            let x = scale.x(t)
            NSRect(x: x, y: Self.rulerHeight - 6, width: 1, height: 6).fill()
            CueTime.running(t).draw(at: NSPoint(x: x + 2, y: 2), withAttributes: attrs)
            t += step
        }
    }
}
