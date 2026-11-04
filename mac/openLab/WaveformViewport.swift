// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The stretch of a file the waveform shows (`start..<end`, seconds) and how zoom and pan move it
/// within `0...duration`. Pure: the view and the panel only apply what it works out.
struct WaveformViewport: Equatable {
    /// The narrowest stretch a zoom shows, seconds.
    static let minSpan = 0.1

    var start: Double
    var end: Double
    let duration: Double

    init(start: Double, end: Double, duration: Double) {
        self.duration = max(0, duration)
        let span = min(self.duration, max(end - start, 0))
        let s = min(max(0, start), max(0, self.duration - span))
        self.start = s
        self.end = s + span
    }

    var span: Double { end - start }

    /// The viewport `span` wide whose start is `start`, kept inside the file.
    private func placed(start: Double, span: Double) -> WaveformViewport {
        WaveformViewport(start: start, end: start + span, duration: duration)
    }

    /// Narrows (`factor` < 1) or widens it, within `minSpan` and the whole file, keeping the time
    /// at `anchor` (0 = left edge, 1 = right edge of the view) where it is on screen.
    func zoomed(by factor: Double, anchor: Double = 0.5) -> WaveformViewport {
        guard factor.isFinite, factor > 0, duration > 0 else { return self }
        let f = min(max(anchor.isFinite ? anchor : 0.5, 0), 1)
        let newSpan = min(duration, max(min(Self.minSpan, duration), span * factor))
        return placed(start: start + f * span - f * newSpan, span: newSpan)
    }

    /// Moves it by `seconds` (negative: earlier), stopping at the file's start and end.
    func panned(by seconds: Double) -> WaveformViewport {
        guard seconds.isFinite else { return self }
        return placed(start: start + seconds, span: span)
    }
}
