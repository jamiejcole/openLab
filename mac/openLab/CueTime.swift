// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// Cue times as text, for the cue list, the inspector and the active-cues panel alike. QLab 5 shows
/// hundredths (manual pp. 126, 131: 100 s shows as `01:40.00`) but keeps milliseconds, so a stored
/// time with a millisecond digit shows it: an edit field must give back exactly what it shows.
/// `mm:ss.cc`, `h:mm:ss.cc` from an hour.
enum TimeShown {
    /// The cue's own time: its duration, also while it runs.
    case full
    /// The time left while it runs.
    case remaining
}

enum CueTime {
    /// Core's `MAX_TIME`: 24 hours.
    static let maxSeconds: Double = 24 * 60 * 60

    /// A stored time: hundredths, or milliseconds when the third digit is not zero.
    static func format(_ seconds: Double) -> String {
        guard seconds.isFinite, seconds >= 0 else { return "" }
        let ms = Int(min(seconds, 1e9) * 1000 + 0.5)
        return ms % 10 == 0 ? text(ms / 10, perSecond: 100) : text(ms, perSecond: 1000)
    }

    /// A running time (elapsed, remaining): always hundredths, so the width does not flicker.
    static func running(_ seconds: Double) -> String {
        guard seconds.isFinite, seconds >= 0 else { return "" }
        return text(Int(min(seconds, 1e9) * 100 + 0.5), perSecond: 100)
    }

    /// A running cue: `elapsed / total` when the end is known, else `elapsed`.
    static func progress(elapsed: Double, remaining: Double?) -> String {
        guard let remaining else { return running(elapsed) }
        return "\(running(elapsed)) / \(running(elapsed + remaining))"
    }

    private static func text(_ units: Int, perSecond: Int) -> String {
        let s = units / perSecond
        let (h, m, sec) = (s / 3600, s / 60 % 60, s % 60)
        let fraction = String(format: perSecond == 100 ? "%02d" : "%03d", units % perSecond)
        return (h > 0 ? String(format: "%d:%02d:%02d.", h, m, sec) : String(format: "%02d:%02d.", m, sec)) + fraction
    }

    /// Seconds from `s.sss`, `m:ss.sss` or `h:mm:ss.sss` (rounded to a millisecond), or nil for
    /// anything else: empty, signs, exponents, `nan`, a colon part of 60 or more, over 24 hours.
    static func parse(_ text: String) -> Double? {
        let parts = text.trimmingCharacters(in: .whitespaces).split(separator: ":", omittingEmptySubsequences: false)
        guard (1...3).contains(parts.count) else { return nil }
        func digits(_ p: Substring) -> Bool { !p.isEmpty && p.allSatisfy { $0 >= "0" && $0 <= "9" } }
        let secPart = parts[parts.count - 1].split(separator: ".", omittingEmptySubsequences: false)
        guard (1...2).contains(secPart.count), digits(secPart[0]), secPart.count == 1 || digits(secPart[1]),
            parts.dropLast().allSatisfy(digits), let sec = Double(parts[parts.count - 1])
        else { return nil }
        let lead = parts.dropLast().compactMap { Double($0) }
        var total = sec
        if parts.count > 1 {
            guard sec < 60 else { return nil }
            total += lead[lead.count - 1] * 60
            if parts.count == 3 {
                guard lead[1] < 60 else { return nil }
                total += lead[0] * 3600
            }
        }
        total = (total * 1000).rounded() / 1000
        return total <= maxSeconds ? total : nil
    }
}
