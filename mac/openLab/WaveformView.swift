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

/// The waveform of an Audio cue's file with its trims (AUD-11, AUD-14): a time ruler on top with the
/// start and end handles at its ends, the file's channels stacked below, the trimmed-off parts
/// dimmed, and the playhead while the cue plays. Dragging a handle moves that trim; the new time is
/// reported as it moves and committed once when the mouse goes up.
@MainActor
final class WaveformView: NSView {
    enum Handle: Equatable { case start, end }

    static let rulerHeight: CGFloat = 18
    /// How near a handle (points) a click must be to take it.
    static let grab: CGFloat = 6

    var scale = WaveformScale(start: 0, end: 1, width: 0) {
        didSet {
            if scale != oldValue {
                drawing = nil
                needsDisplay = true
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
    var isLocked = false
    /// A text shown in the middle when there is nothing to draw ("reading", why not).
    var message: String? { didSet { needsDisplay = true } }
    /// A handle moved (live, during the drag).
    var onTrimChanging: ((Handle, Double) -> Void)?
    /// A drag moved a handle: its new time (for the end, nil is the end of the file).
    var onTrimCommit: ((Handle, Double?) -> Void)?
    /// The width changed: the peaks drawn no longer fit it.
    var onResize: (() -> Void)?

    private(set) var dragging: Handle?
    /// The trims when the drag began: letting go there sends nothing.
    private var dragOrigin: (start: Double, end: Double?) = (0, nil)
    private var drawing: WaveformDrawing?

    override var isFlipped: Bool { true }

    var waveRect: CGRect {
        CGRect(x: 0, y: Self.rulerHeight, width: bounds.width, height: max(0, bounds.height - Self.rulerHeight))
    }

    var endTime: Double { trimEnd ?? duration ?? 0 }

    override func setFrameSize(_ newSize: NSSize) {
        let changed = newSize.width != frame.width
        super.setFrameSize(newSize)
        if changed {
            scale.width = newSize.width
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

    /// Takes the handle under `x`, if any (not in Show Mode, nor before the file's length is known).
    @discardableResult
    func beginDrag(atX x: CGFloat) -> Bool {
        guard !isLocked, duration != nil else { return false }
        let ds = abs(scale.x(trimStart) - x)
        let de = abs(scale.x(endTime) - x)
        guard min(ds, de) <= Self.grab else { return false }
        dragging = ds <= de ? .start : .end
        dragOrigin = (trimStart, trimEnd)
        return true
    }

    func drag(toX x: CGFloat) {
        guard let dragging, let duration else { return }
        let t = scale.seconds(x)
        switch dragging {
        case .start:
            trimStart = Self.clampStart(t, end: endTime)
            onTrimChanging?(.start, trimStart)
        case .end:
            let e = Self.clampEnd(t, start: trimStart, duration: duration)
            trimEnd = e
            onTrimChanging?(.end, e)
        }
    }

    /// Commits the drag when it moved the handle from where it was taken. An end at the file's end
    /// is no trim at all (nil), as if never set.
    func endDrag() {
        guard let handle = dragging else { return }
        dragging = nil
        switch handle {
        case .start:
            if trimStart != dragOrigin.start { onTrimCommit?(.start, trimStart) }
        case .end:
            let end: Double? = endTime >= (duration ?? .infinity) ? nil : endTime
            trimEnd = end
            if end != dragOrigin.end { onTrimCommit?(.end, end) }
        }
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

    override func mouseDown(with event: NSEvent) {
        beginDrag(atX: convert(event.locationInWindow, from: nil).x)
    }

    override func mouseDragged(with event: NSEvent) {
        drag(toX: convert(event.locationInWindow, from: nil).x)
    }

    override func mouseUp(with event: NSEvent) { endDrag() }

    override func resetCursorRects() {
        guard !isLocked, duration != nil else { return }
        for t in [trimStart, endTime] {
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
