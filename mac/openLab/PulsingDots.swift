// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Three dots that brighten one after another: the cue list's mark for a cue whose level is changing
/// (fading), so a fade reads as something happening, not as a still glyph. Drawn from one shared clock
/// (every fading row moves in step); with Reduce Motion on, the dots stand still.
final class PulsingDots: NSView {
    static let period: TimeInterval = 1.2
    static let dimAlpha: CGFloat = 0.3

    var colour: NSColor = MarkColour.caution {
        didSet { needsDisplay = true }
    }

    /// The shared clock's time, in seconds; tests set it.
    nonisolated(unsafe) static var now: () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }

    /// Dot `i`'s alpha at `time`: each peaks a third of a period after the one before.
    static func alpha(dot i: Int, at time: TimeInterval) -> CGFloat {
        let phase = (time / period - Double(i) / 3).truncatingRemainder(dividingBy: 1)
        let p = phase < 0 ? phase + 1 : phase
        // A smooth rise and fall over the period: 1 at its peak, dimAlpha at its lowest.
        let wave = (1 - cos(2 * Double.pi * p)) / 2
        return dimAlpha + (1 - dimAlpha) * CGFloat(wave)
    }

    static var moves: Bool { !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion }

    override var isHidden: Bool {
        didSet { Ticker.shared.follow(self) }
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        Ticker.shared.follow(self)
    }

    var wantsTicks: Bool { window != nil && !isHidden && Self.moves }

    override func draw(_ dirtyRect: NSRect) {
        let d = min(bounds.height, bounds.width / 4).rounded(.down)
        let gap = (bounds.width - 3 * d) / 4
        let time = Self.now()
        for i in 0..<3 {
            let alpha = Self.moves ? Self.alpha(dot: i, at: time) : 1
            colour.withAlphaComponent(alpha).setFill()
            let rect = NSRect(x: gap + CGFloat(i) * (d + gap), y: (bounds.height - d) / 2, width: d, height: d)
            NSBezierPath(ovalIn: rect).fill()
        }
    }

    /// One timer for every shown dots view, running only while one wants ticks.
    @MainActor
    final class Ticker {
        static let shared = Ticker()
        private let views = NSHashTable<PulsingDots>.weakObjects()
        private var timer: Timer?

        func follow(_ view: PulsingDots) {
            if view.wantsTicks { views.add(view) } else { views.remove(view) }
            if views.count > 0, timer == nil {
                let t = Timer(timeInterval: 1.0 / 30, repeats: true) { _ in
                    MainActor.assumeIsolated { Ticker.shared.tick() }
                }
                RunLoop.main.add(t, forMode: .common)
                timer = t
            } else if views.count == 0 {
                timer?.invalidate()
                timer = nil
            }
        }

        private func tick() {
            for view in views.allObjects where view.wantsTicks { view.needsDisplay = true }
        }

        var isRunning: Bool { timer != nil }
    }
}
