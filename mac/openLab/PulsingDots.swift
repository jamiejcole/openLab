// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// A ring of eight dots round a ring of four, with a brightness sweeping round them:
/// the cue list's mark for a cue on its way out (a panic, a stop with a fade), so the fade-out reads as
/// something happening, not as a still glyph. Drawn from one shared clock (every such row moves in step); with
/// Reduce Motion on, it stands still and evenly lit.
final class PulsingDots: NSView {
    /// One turn of the brightness sweep, clockwise.
    static let period: TimeInterval = 0.8
    static let dimAlpha: CGFloat = 0.25
    /// Dots per ring, outer then inner, and each ring's radius as a share of half the view.
    static let rings: [(count: Int, radius: CGFloat, dot: CGFloat)] = [(8, 0.78, 0.17), (4, 0.36, 0.16)]

    /// Presses and hovers fall through to the state image under the dots, which holds the label and
    /// tooltip.
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    var colour: NSColor = MarkColour.stopping {
        didSet { needsDisplay = true }
    }

    /// The shared clock's time, in seconds; tests set it.
    nonisolated(unsafe) static var now: () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }

    /// The alpha of a dot at `angle` (radians) at `time`: brightest where the sweep
    /// is, fading behind it, never fully out.
    static func alpha(angle: Double, at time: TimeInterval) -> CGFloat {
        // Clockwise: the head's angle falls with time (the view's y runs up).
        let head = -2 * Double.pi * (time / period).truncatingRemainder(dividingBy: 1)
        // How far the dot trails the sweep, 0 at the head to 1 just ahead of it.
        var behind = (angle - head) / (2 * Double.pi)
        behind -= behind.rounded(.down)
        return dimAlpha + (1 - dimAlpha) * CGFloat(pow(1 - behind, 2))
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
        let half = min(bounds.width, bounds.height) / 2
        let centre = NSPoint(x: bounds.midX, y: bounds.midY)
        let time = Self.now()
        let moving = Self.moves
        for ring in Self.rings {
            for k in 0..<ring.count {
                let angle = 2 * Double.pi * Double(k) / Double(ring.count)
                let a = angle
                let alpha = moving ? Self.alpha(angle: a, at: time) : 1
                colour.withAlphaComponent(alpha).setFill()
                let r = half * ring.radius
                let d = half * ring.dot * 2
                let at = NSPoint(x: centre.x + r * CGFloat(cos(a)), y: centre.y + r * CGFloat(sin(a)))
                NSBezierPath(ovalIn: NSRect(x: at.x - d / 2, y: at.y - d / 2, width: d, height: d)).fill()
            }
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
