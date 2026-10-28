// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// Levels as the inspector shows and types them (AUD-06, AUD-08). The wire holds dB with
/// -120 meaning silence (docs/file-format.md); the UI shows that as "-inf".
enum LevelText {
    static let silence = -120.0
    static let maxDb = 12.0
    /// QLab 5's default lower limit: a level typed at or below it is silent (manual p. 200).
    static let floorDb = -60.0

    static func format(_ db: Double) -> String {
        if db.isNaN || db <= silence { return "-inf" }
        let text = String(format: "%.1f", db)
        return text == "-0.0" ? "0.0" : text
    }

    /// The level for typed text, as QLab reads it (manual pp. 181-182, 200): a number without a
    /// sign is negative (`6` is -6 dB, `+6` is +6 dB); a value at or below the lower limit, or text
    /// that is not a number, is -inf. Above +12 is held at +12 (QLab limits it to the workspace
    /// maximum). A decimal comma is read as a point.
    static func parse(_ text: String) -> Double {
        var t = text.trimmingCharacters(in: .whitespaces).lowercased().replacingOccurrences(of: ",", with: ".")
        t = t.replacingOccurrences(of: "db", with: "").trimmingCharacters(in: .whitespaces)
        guard !t.isEmpty, let v = Double(t), !v.isNaN else { return silence }
        let signed = t.hasPrefix("+") || t.hasPrefix("-")
        return clamp(signed || v == 0 ? v : -v)
    }

    /// Held to what the engine accepts: at most +12, and at or below the floor means silent.
    static func clamp(_ db: Double) -> Double {
        if db.isNaN || db <= floorDb { return silence }
        return min(db, maxDb)
    }
}

/// An Audio cue's levels (master, cue outputs, routing matrix, gangs), or the levels a Fade cue
/// moves its target to: the `levels` object of the cue's kind. In a Fade a value is only present
/// when it is "active" (set to fade), and nothing has a mute (a fade never changes mutes).
struct LevelSet: Equatable {
    struct Output: Equatable {
        /// nil: a Fade leaves this output alone.
        var db: Double?
        var mute = false
    }
    struct Point: Hashable, Comparable {
        var input: Int
        var output: Int
        static func < (a: Point, b: Point) -> Bool { (a.input, a.output) < (b.input, b.output) }
    }
    struct Cross: Equatable {
        var db: Double
        var mute = false
    }
    /// Which level of the matrix: the master, a cue output's, or a crosspoint's.
    enum Slot: Hashable {
        case master
        case output(Int)
        case cross(input: Int, output: Int)
    }

    /// The wire's most cue outputs, and file channels (AUD-03).
    static let maxChannels = 64

    var isFade: Bool
    /// nil: a Fade leaves the master alone.
    var master: Double?
    var outputs: [Output]
    var crosspoints: [Point: Cross]
    var gangs: [[Int]]

    /// Reads `kind.levels` of an Audio or Fade cue; nil for anything else or a malformed object.
    init?(kind: JSONValue) {
        guard let type = kind["type"]?.stringValue, type == "audio" || type == "fade",
            let levels = kind["levels"], case .object = levels
        else { return nil }
        isFade = type == "fade"
        master = levels["master_db"]?.doubleValue
        outputs = []
        crosspoints = [:]
        gangs = []
        if isFade {
            outputs = (levels["output_db"]?.arrayValue ?? []).map { Output(db: $0.doubleValue) }
        } else {
            outputs = (levels["outputs"]?.arrayValue ?? []).map {
                Output(db: $0["db"]?.doubleValue ?? LevelText.silence, mute: $0["mute"] == .bool(true))
            }
            gangs = (levels["gangs"]?.arrayValue ?? []).map {
                ($0.arrayValue ?? []).compactMap { $0.doubleValue.map(Int.init) }
            }
        }
        for x in levels["crosspoints"]?.arrayValue ?? [] {
            guard let i = x["input"]?.doubleValue, let o = x["output"]?.doubleValue, let db = x["db"]?.doubleValue,
                i >= 0, o >= 0, i < 64, o < 64
            else { continue }
            crosspoints[Point(input: Int(i), output: Int(o))] = Cross(db: db, mute: x["mute"] == .bool(true))
        }
    }

    /// `kind` with its `levels` replaced by this set. Other fields of `levels` and of the kind stay.
    func json(into kind: JSONValue) -> JSONValue {
        var kind = kind
        var levels = kind["levels"] ?? .object([:])
        func num(_ d: Double) -> JSONValue { .double(d) }
        levels.set("master_db", master.map(num) ?? (isFade ? .null : num(LevelText.silence)))
        let points = crosspoints.keys.sorted().map { p -> JSONValue in
            let c = crosspoints[p] ?? Cross(db: LevelText.silence)
            var o: [String: JSONValue] = [
                "input": .int(Int64(p.input)), "output": .int(Int64(p.output)), "db": num(c.db),
            ]
            if !isFade { o["mute"] = .bool(c.mute) }
            return .object(o)
        }
        levels.set("crosspoints", .array(points))
        if isFade {
            var out = outputs
            while out.last?.db == nil, !out.isEmpty { out.removeLast() }
            levels.set("output_db", .array(out.map { $0.db.map(num) ?? .null }))
        } else {
            levels.set(
                "outputs",
                .array(outputs.map { .object(["db": num($0.db ?? LevelText.silence), "mute": .bool($0.mute)]) }))
            levels.set("gangs", .array(gangs.map { .array($0.map { .int(Int64($0)) }) }))
        }
        kind.set("levels", levels)
        return kind
    }

    /// The highest file channel the routing mentions, plus one.
    var inputExtent: Int { (crosspoints.keys.map(\.input).max() ?? -1) + 1 }
    /// The highest cue output mentioned, plus one.
    var outputExtent: Int {
        max(outputs.count, (crosspoints.keys.map(\.output).max() ?? -1) + 1)
    }

    // MARK: Reading a slot

    func db(_ slot: Slot) -> Double? {
        switch slot {
        case .master: return master
        case .output(let o): return outputs.indices.contains(o) ? outputs[o].db : (isFade ? nil : LevelText.silence)
        case .cross(let i, let o):
            return crosspoints[Point(input: i, output: o)]?.db ?? (isFade ? nil : LevelText.silence)
        }
    }

    func isMuted(_ slot: Slot) -> Bool {
        switch slot {
        case .master: return false
        case .output(let o): return outputs.indices.contains(o) && outputs[o].mute
        case .cross(let i, let o): return crosspoints[Point(input: i, output: o)]?.mute ?? false
        }
    }

    /// Whether a Fade moves this level; an Audio cue's levels are always live.
    func isActive(_ slot: Slot) -> Bool { isFade ? db(slot) != nil : true }

    /// The gang (index into `gangs`) holding cue output `o`.
    func gang(of o: Int) -> Int? { gangs.firstIndex { $0.contains(o) } }

    // MARK: Editing

    /// Sets a level. A cue output in a gang moves its whole gang by the same amount, each member
    /// held to the limits on its own (QLab, manual p. 181).
    mutating func set(_ slot: Slot, db: Double, drag: Bool = false) {
        let db = LevelText.clamp(db)
        switch slot {
        case .master: master = db
        case .output(let o):
            guard o >= 0, o < Self.maxChannels else { return }
            // Levels move on the fader's travel, where -inf is the bottom (-60 dB): a member at -inf
            // is at the floor, not 60 dB below it, so the delta is what the control moved.
            func travel(_ x: Double) -> Double { max(x, LevelText.floorDb) }
            let old = travel(self.db(slot) ?? db)
            padOutputs(to: o + 1)
            if !isFade, let g = gang(of: o), gangs[g].count > 1 {
                let delta = travel(db) - old
                for m in gangs[g] where m != o && outputs.indices.contains(m) {
                    let was = outputs[m].db ?? LevelText.silence
                    var moved = travel(was) + delta
                    // A drag never takes a member past 0 dB (a member already above stays where it is).
                    if drag { moved = min(moved, max(0, was)) }
                    outputs[m].db = LevelText.clamp(moved)
                }
            }
            outputs[o].db = db
        case .cross(let i, let o):
            guard i >= 0, o >= 0, i < Self.maxChannels, o < Self.maxChannels else { return }
            let p = Point(input: i, output: o)
            if !isFade, db <= LevelText.silence, crosspoints[p]?.mute != true {
                crosspoints[p] = nil  // absent is silent
            } else {
                crosspoints[p] = Cross(db: db, mute: crosspoints[p]?.mute ?? false)
            }
        }
    }

    /// Mute keeps the level. Only an Audio cue's outputs and listed crosspoints have one.
    mutating func setMute(_ slot: Slot, _ mute: Bool) {
        guard !isFade else { return }
        switch slot {
        case .master: return
        case .output(let o): if outputs.indices.contains(o) { outputs[o].mute = mute }
        case .cross(let i, let o):
            let p = Point(input: i, output: o)
            if crosspoints[p] != nil { crosspoints[p]?.mute = mute }
        }
    }

    /// A Fade stops moving this level (grey). Not for an Audio cue.
    mutating func deactivate(_ slot: Slot) {
        guard isFade else { return }
        switch slot {
        case .master: master = nil
        case .output(let o): if outputs.indices.contains(o) { outputs[o].db = nil }
        case .cross(let i, let o): crosspoints[Point(input: i, output: o)] = nil
        }
    }

    /// "Set all silent": an Audio cue's levels all to -inf; a Fade back to fading nothing.
    mutating func silenceAll() {
        crosspoints = [:]
        if isFade {
            master = nil
            outputs = []
        } else {
            master = LevelText.silence
            for o in outputs.indices { outputs[o].db = LevelText.silence }
        }
    }

    /// Puts cue output `o` in the gang lettered `letter` (gangs are lettered A, B, ... in array
    /// order; a letter with no gang starts a new one), or in none for an empty `letter`. An output
    /// is in at most one gang; emptied gangs are dropped (so the letters of later gangs may move up).
    mutating func setGang(of o: Int, letter: String) {
        guard !isFade, outputs.indices.contains(o) else { return }
        let named = letter.trimmingCharacters(in: .whitespaces).uppercased().unicodeScalars.first
        let target = named.flatMap { Self.gangIndex(letter: $0) }
        gangs = gangs.map { $0.filter { $0 != o } }
        if let target {
            if target < gangs.count, !gangs[target].isEmpty {
                gangs[target] = (gangs[target] + [o]).sorted()
            } else {
                gangs.append([o])
            }
        }
        gangs = gangs.filter { !$0.isEmpty }
    }

    static func gangLetter(_ index: Int) -> String {
        guard let a = Unicode.Scalar(UInt32(65 + index % 26)) else { return "?" }
        return String(Character(a))
    }

    private static func gangIndex(letter: Unicode.Scalar) -> Int? {
        let v = Int(letter.value) - 65
        return (0..<26).contains(v) ? v : nil
    }

    private mutating func padOutputs(to n: Int) {
        while outputs.count < n { outputs.append(Output(db: isFade ? nil : LevelText.silence)) }
    }
}
