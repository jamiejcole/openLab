// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// A patch's routing as the patch editor draws and edits it (AUD-07; QLab 5 manual p. 190): every
/// cue output to any device outputs, each crosspoint at its own level, and a level and mute per
/// device output. The show holds the crosspoints set by hand; a cue output with none listed for a
/// device output goes to the device output this machine maps it to at 0 dB (docs/file-format.md,
/// patches). Indices are 0-based.
struct PatchRouting: Equatable {
    struct Pair: Hashable, Comparable {
        var output: Int
        var deviceOutput: Int
        static func < (a: Pair, b: Pair) -> Bool { (a.output, a.deviceOutput) < (b.output, b.deviceOutput) }
    }

    struct DeviceOutput: Equatable {
        var level: Double = 0
        var mute = false
        var isDefault: Bool { level == 0 && !mute }
    }

    struct CueOutput: Equatable {
        var level: Double = 0
        var mute = false
        var polarityInverted = false
    }

    /// The most device outputs a patch can route to (the engine's limit).
    static let maxDeviceOutputs = 64

    var master: Double = 0
    var outputs: [CueOutput] = []
    /// The device output this machine sends each cue output to; nil with no device.
    var channels: [Int]?
    /// The crosspoints the show lists.
    var crosspoints: [Pair: Double] = [:]
    /// Device outputs not at 0 dB, unmuted.
    var deviceOutputs: [Int: DeviceOutput] = [:]

    init() {}

    /// The routing of `patch` (a model patch object) with this machine's `mapping`.
    init(patch: JSONValue, mapping: PatchMapping?) {
        master = patch["master_db"]?.doubleValue ?? 0
        outputs = (patch["outputs"]?.arrayValue ?? []).map {
            CueOutput(
                level: $0["level_db"]?.doubleValue ?? 0, mute: Self.flag($0["mute"]),
                polarityInverted: Self.flag($0["polarity_inverted"]))
        }
        channels = mapping.map { $0.channels.map { Int($0) } }
        for c in patch["crosspoints"]?.arrayValue ?? [] {
            guard let o = c["output"]?.doubleValue, let d = c["device_output"]?.doubleValue,
                let db = c["db"]?.doubleValue
            else { continue }
            crosspoints[Pair(output: Int(o), deviceOutput: Int(d))] = db
        }
        for e in patch["device_outputs"]?.arrayValue ?? [] {
            guard let d = e["device_output"]?.doubleValue else { continue }
            deviceOutputs[Int(d)] = DeviceOutput(level: e["level_db"]?.doubleValue ?? 0, mute: Self.flag(e["mute"]))
        }
    }

    private static func flag(_ v: JSONValue?) -> Bool {
        if case .bool(let b) = v { return b }
        return false
    }

    /// Whether this machine sends cue output `o` to device output `d` when no crosspoint says otherwise.
    func isMapped(_ o: Int, _ d: Int) -> Bool {
        guard let channels, channels.indices.contains(o) else { return false }
        return channels[o] == d
    }

    /// The level cue output `o` reaches device output `d` at (the crosspoint's), nil when it does not.
    func level(_ o: Int, _ d: Int) -> Double? {
        let db = crosspoints[Pair(output: o, deviceOutput: d)] ?? (isMapped(o, d) ? 0 : LevelText.silence)
        return db > LevelText.silence ? db : nil
    }

    func isListed(_ o: Int, _ d: Int) -> Bool { crosspoints[Pair(output: o, deviceOutput: d)] != nil }

    func deviceOutput(_ d: Int) -> DeviceOutput { deviceOutputs[d] ?? DeviceOutput() }

    /// The device outputs the patch names: one past the highest its mapping, its device-output
    /// settings or a crosspoint that routes one of its cue outputs names (as the engine counts them
    /// for its warning; a silent crosspoint, or one of a cue output the patch no longer has, routes
    /// nothing).
    var namedDeviceOutputs: Int {
        let routed = crosspoints.filter { $0.key.output < outputs.count && $0.value > LevelText.silence }
        let highest = [
            channels?.max(), routed.keys.map(\.deviceOutput).max(), deviceOutputs.keys.max(),
        ]
        .compactMap { $0 }.max()
        return highest.map { $0 + 1 } ?? 0
    }

    /// Cue outputs heard on device output `d` (they reach it above silence).
    func feeders(of d: Int) -> [Int] {
        outputs.indices.filter { level($0, d) != nil }
    }

    /// What typing `db` (silence: not routed) into the box of cue output `o` and device output `d`
    /// sends: nil when nothing changes, else the crosspoint's new value (`.some(nil)` takes it out of
    /// the list, so the mapping decides again). The list stays as short as the routing allows.
    func typed(_ db: Double, output o: Int, deviceOutput d: Int) -> Double?? {
        let wanted = db > LevelText.silence ? db : LevelText.silence
        let byMapping = isMapped(o, d) ? 0 : LevelText.silence
        if wanted == byMapping { return isListed(o, d) ? .some(nil) : nil }
        if crosspoints[Pair(output: o, deviceOutput: d)] == wanted { return nil }
        return .some(wanted)
    }

    // MARK: Whole-patch edits

    /// The crosspoints that make this patch route as `source` does, whichever device outputs each
    /// machine mapping sends their cue outputs to: for each cue output both have, every device
    /// output either routes; cue outputs only this patch has keep their own routing.
    func crosspoints(copying source: PatchRouting) -> [Pair: Double] {
        var out: [Pair: Double] = [:]
        let width = max(namedDeviceOutputs, source.namedDeviceOutputs)
        for o in outputs.indices {
            for d in 0..<width {
                let wanted =
                    o < source.outputs.count
                    ? source.level(o, d) ?? LevelText.silence
                    : crosspoints[Pair(output: o, deviceOutput: d)]
                guard let wanted else { continue }
                let byMapping = isMapped(o, d) ? 0 : LevelText.silence
                if wanted != byMapping { out[Pair(output: o, deviceOutput: d)] = wanted }
            }
        }
        return out
    }

    /// The commands (one undo step) that give patch `id` the routing of `source`: the main level,
    /// each cue output's level, mute and polarity (names stay), the crosspoints and the device
    /// outputs. Cue outputs `source` lacks are set to 0 dB, unmuted.
    func copyCommands(_ id: ModelID, from source: PatchRouting, outputsJSON: [JSONValue]) -> Command? {
        var target = self
        target.master = source.master
        for i in target.outputs.indices {
            target.outputs[i] = i < source.outputs.count ? source.outputs[i] : CueOutput()
        }
        target.crosspoints = crosspoints(copying: source)
        target.deviceOutputs = source.deviceOutputs
        return commands(id, to: target, outputsJSON: outputsJSON)
    }

    /// Back to a new patch's routing: main and cue outputs at 0 dB, unmuted, polarity normal (names
    /// stay), no crosspoints (each cue output to its mapped device output) and device outputs at
    /// 0 dB, unmuted.
    func defaultsCommands(_ id: ModelID, outputsJSON: [JSONValue]) -> Command? {
        var target = self
        target.master = 0
        target.outputs = outputs.map { _ in CueOutput() }
        target.crosspoints = [:]
        target.deviceOutputs = [:]
        return commands(id, to: target, outputsJSON: outputsJSON)
    }

    /// Every cue output and device output unmuted, levels kept.
    func unmuteCommands(_ id: ModelID, outputsJSON: [JSONValue]) -> Command? {
        var target = self
        for i in target.outputs.indices { target.outputs[i].mute = false }
        target.deviceOutputs = deviceOutputs.mapValues { DeviceOutput(level: $0.level, mute: false) }
            .filter { !$0.value.isDefault }
        return commands(id, to: target, outputsJSON: outputsJSON)
    }

    var hasMutes: Bool { outputs.contains { $0.mute } || deviceOutputs.values.contains { $0.mute } }

    /// The commands that turn this routing into `target`, as one step; nil when they are the same.
    private func commands(_ id: ModelID, to target: PatchRouting, outputsJSON: [JSONValue]) -> Command? {
        var commands: [Command] = []
        if target.master != master { commands.append(.setPatchLevel(id, output: nil, db: target.master)) }
        if target.outputs != outputs {
            let json: [JSONValue] = target.outputs.enumerated().map { i, o in
                var entry = outputsJSON.indices.contains(i) ? outputsJSON[i] : .object([:])
                entry["level_db"] = .double(o.level)
                entry["mute"] = .bool(o.mute)
                entry["polarity_inverted"] = .bool(o.polarityInverted)
                return entry
            }
            commands.append(.setPatchProperty(id, ["type": .string("outputs"), "value": .array(json)]))
        }
        if target.crosspoints != crosspoints {
            commands.append(.setPatchCrosspoints(id, target.crosspoints))
        }
        if target.deviceOutputs != deviceOutputs {
            commands.append(.setPatchDeviceOutputs(id, target.deviceOutputs))
        }
        return Command.transaction(commands)
    }
}

extension Command {
    /// `SetPatchProperty { crosspoint }`: cue output `output` to device output `deviceOutput` at `db`
    /// (-120: not routed); nil takes it out of the list, so this machine's mapping decides again.
    static func setPatchCrosspoint(_ patch: ModelID, output: Int, deviceOutput: Int, db: Double?) -> Command {
        setPatchProperty(
            patch,
            [
                "type": .string("crosspoint"), "output": .int(Int64(output)),
                "device_output": .int(Int64(deviceOutput)), "value": db.map { .double($0) } ?? .null,
            ])
    }

    /// `SetPatchProperty { crosspoints }`: every crosspoint of the patch, sorted as the engine keeps them.
    static func setPatchCrosspoints(_ patch: ModelID, _ crosspoints: [PatchRouting.Pair: Double]) -> Command {
        let list: [JSONValue] = crosspoints.keys.sorted().map { pair in
            .object([
                "output": .int(Int64(pair.output)), "device_output": .int(Int64(pair.deviceOutput)),
                "db": .double(crosspoints[pair] ?? LevelText.silence),
            ])
        }
        return setPatchProperty(patch, ["type": .string("crosspoints"), "value": .array(list)])
    }

    /// `SetPatchProperty { device_output_level }`.
    static func setPatchDeviceOutputLevel(_ patch: ModelID, deviceOutput: Int, db: Double) -> Command {
        setPatchProperty(
            patch,
            [
                "type": .string("device_output_level"), "device_output": .int(Int64(deviceOutput)),
                "value": .double(db),
            ])
    }

    /// `SetPatchProperty { device_output_mute }`.
    static func setPatchDeviceOutputMute(_ patch: ModelID, deviceOutput: Int, mute: Bool) -> Command {
        setPatchProperty(
            patch,
            [
                "type": .string("device_output_mute"), "device_output": .int(Int64(deviceOutput)),
                "value": .bool(mute),
            ])
    }

    /// `SetPatchProperty { device_outputs }`: every device output's level and mute, sorted.
    static func setPatchDeviceOutputs(_ patch: ModelID, _ outputs: [Int: PatchRouting.DeviceOutput]) -> Command {
        let list: [JSONValue] = outputs.keys.sorted().map { d in
            let o = outputs[d] ?? PatchRouting.DeviceOutput()
            return .object([
                "device_output": .int(Int64(d)), "level_db": .double(o.level), "mute": .bool(o.mute),
            ])
        }
        return setPatchProperty(patch, ["type": .string("device_outputs"), "value": .array(list)])
    }
}
