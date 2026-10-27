// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The inspector's time fields: `mm:ss.sss` (QLab 5's inspector), `h:mm:ss.sss` from an hour.
enum InspectorTime {
    /// Core's `MAX_TIME`: 24 hours.
    static let maxSeconds: Double = 24 * 60 * 60

    static func format(_ seconds: Double) -> String {
        guard seconds.isFinite, seconds >= 0 else { return "" }
        let ms = Int(min(seconds, maxSeconds * 2) * 1000 + 0.5)
        let (h, m, s, f) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
        return h > 0 ? String(format: "%d:%02d:%02d.%03d", h, m, s, f) : String(format: "%02d:%02d.%03d", m, s, f)
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
