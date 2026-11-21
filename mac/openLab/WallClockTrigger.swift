// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// A cue's wall-clock trigger (`triggers.wall_clock`, docs/file-format.md §5.1): on or off, a time of
/// day in the computer's local time, the days of the week it fires on, and whether the inspector
/// shows the time on a 12-hour clock. Absent from a cue: a new cue's (off, 00:00:00, every day).
struct WallClockTrigger: Equatable {
    /// The file's day names, Sunday first (index = day number).
    static let dayNames = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"]
    static let everyDay = Set(0..<7)

    var enabled = false
    var hours = 0
    var minutes = 0
    var seconds = 0
    /// Day numbers, 0 Sunday to 6 Saturday.
    var days = everyDay
    var twelveHour = false

    init() {}

    init(json: JSONValue?) {
        guard let o = json?.objectValue else { return }
        enabled = o["enabled"] == .bool(true)
        hours = Self.int(o["hours"]) ?? 0
        minutes = Self.int(o["minutes"]) ?? 0
        seconds = Self.int(o["seconds"]) ?? 0
        if let names = o["days"]?.arrayValue {
            days = Set(names.compactMap { $0.stringValue.flatMap(Self.dayNames.firstIndex(of:)) })
        }
        twelveHour = o["twelve_hour"] == .bool(true)
    }

    private static func int(_ v: JSONValue?) -> Int? {
        switch v {
        case .int(let n): return Int(exactly: n)
        case .double(let d): return Int(exactly: d)
        default: return nil
        }
    }

    var json: JSONValue {
        var o: [String: JSONValue] = [
            "enabled": .bool(enabled), "hours": .int(Int64(hours)), "minutes": .int(Int64(minutes)),
            "seconds": .int(Int64(seconds)),
            "days": .array(days.sorted().map { .string(Self.dayNames[$0]) }),
        ]
        if twelveHour { o["twelve_hour"] = .bool(true) }
        return .object(o)
    }

    var isPM: Bool { hours >= 12 }

    /// The time as the field shows it: `19:25:00` on the 24-hour clock, `7:25:00` on the 12-hour one
    /// (its pop-up says AM or PM).
    var display: String {
        let h = twelveHour ? (hours % 12 == 0 ? 12 : hours % 12) : hours
        return twelveHour
            ? String(format: "%d:%02d:%02d", h, minutes, seconds)
            : String(format: "%02d:%02d:%02d", h, minutes, seconds)
    }

    /// The time typed in the field, read from the right as QLab reads it: `10:30` is ten minutes and
    /// thirty seconds past midnight, `5` five seconds. On the 12-hour clock the hour is 1 to 12 (or 0,
    /// as 12) and `pm` says which half of the day; on the 24-hour clock 0 to 23. Nil for anything
    /// else (a part that is not a whole number, more than three parts, a value out of range).
    static func parse(_ text: String, twelveHour: Bool, pm: Bool) -> (hours: Int, minutes: Int, seconds: Int)? {
        let parts = text.trimmingCharacters(in: .whitespaces).split(separator: ":", omittingEmptySubsequences: false)
        guard (1...3).contains(parts.count) else { return nil }
        var values: [Int] = []
        for p in parts {
            guard !p.isEmpty, p.count <= 2, p.allSatisfy({ $0.isASCII && $0.isNumber }), let n = Int(p) else {
                return nil
            }
            values.append(n)
        }
        while values.count < 3 { values.insert(0, at: 0) }
        let (h, m, s) = (values[0], values[1], values[2])
        guard m < 60, s < 60 else { return nil }
        if twelveHour {
            guard h <= 12 else { return nil }
            return (h % 12 + (pm ? 12 : 0), m, s)
        }
        guard h < 24 else { return nil }
        return (h, m, s)
    }

    /// What the days button says: every day, weekdays, the weekend, or the days' short names.
    var daysSummary: String {
        if days == Self.everyDay { return loc("inspector.wallClock.everyDay") }
        if days == Set(1...5) { return loc("inspector.wallClock.weekdays") }
        if days == [0, 6] { return loc("inspector.wallClock.weekend") }
        // Monday first, Sunday last, as a week is read in a show's schedule.
        let order = [1, 2, 3, 4, 5, 6, 0]
        return order.filter(days.contains).map { loc("inspector.wallClock.short.\(Self.dayNames[$0])") }
            .joined(separator: ", ")
    }
}
