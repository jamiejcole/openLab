// SPDX-License-Identifier: GPL-3.0-or-later

import Combine
import Foundation

/// What is wrong with one patch on this machine (WS-07): shown in its row of the settings and in
/// the patch editor, so a silent patch is never a surprise at the show (QLab 5 manual p. 191).
enum PatchProblem: Equatable {
    case unmapped
    case deviceMissing(String)
    /// The patch routes to output `needed` (1-based) of a device of which openLab can use `available`.
    case channelsMissing(device: String, needed: Int, available: Int)
    case outputCountMismatch
}

/// One output patch as the settings window lists it.
struct PatchRow: Equatable {
    let id: ModelID
    let name: String
    let outputCount: Int
    let mapping: PatchMapping?
    /// Where the engine resolves it now (`list_patch_mappings`); nil from an engine without it.
    let resolution: WireVariant?
}

/// The state behind the workspace settings window (AUD-07, WS-07, PB-02, PB-03) and the patch
/// editors opened from it. It reads and writes through the `WorkspaceStore` only; the windows draw
/// what it holds.
///
/// Each patch plays on the device picked for it on this machine (several at once, AUD-02). Every
/// device runs at the clock device's output count, so a device with more outputs offers only that
/// many; the clock device itself is an application preference.
@MainActor
final class WorkspaceSettingsModel: ObservableObject {
    let store: WorkspaceStore
    @Published private(set) var devices: [DeviceEntry] = []
    @Published private(set) var patches: [PatchRow] = []
    /// The last refusal or failure, in words; cleared by the next success.
    @Published var message: String?
    /// The last key typed into the key map that was not taken, and why; cleared by the next success.
    @Published var keyRefusal: KeyRefusal?
    /// The device list could not be read (engine gone): nothing here can be changed.
    @Published private(set) var loaded = false

    init(store: WorkspaceStore) { self.store = store }

    // MARK: Reading

    /// Reads the device list and the patch mappings again.
    func refresh() async {
        switch await store.listDevices() {
        case .success(let ok):
            if case .devices(let list, _)? = ok.data {
                // Assigned only on a change: each assignment publishes, and the window rebuilds.
                if devices != list { devices = list }
                if !loaded { loaded = true }
            }
        case .failure(let e):
            if loaded { loaded = false }
            message = describe(e)
            return
        }
        switch await store.listPatchMappings() {
        case .success(let entries):
            let byPatch = Dictionary(entries.map { ($0.patch, $0) }, uniquingKeysWith: { first, _ in first })
            let rows = (store.model?.patches ?? []).map { object in
                PatchRow(
                    id: object.id, name: object.raw["name"]?.stringValue ?? "",
                    outputCount: object.raw["outputs"]?.arrayValue?.count ?? 0,
                    mapping: byPatch[object.id]?.mapping, resolution: byPatch[object.id]?.resolution)
            }
            if patches != rows { patches = rows }
        case .failure(let e): message = describe(e)
        }
    }

    // MARK: Output patches (add, rename, remove)

    /// Adds a two-output patch after the last one, named by its place. Returns its id.
    @discardableResult
    func addPatch() async -> ModelID? {
        let count = store.model?.patches.count ?? 0
        let id = ModelID(UUID().uuidString.lowercased())
        let output = JSONValue.object(["level_db": .double(0), "mute": .bool(false), "polarity_inverted": .bool(false)])
        let patch = JSONValue.object([
            "id": .string(id.raw), "name": .string(String(format: loc("settings.audio.newPatchName"), count + 1)),
            "master_db": .double(0), "outputs": .array([output, output]),
        ])
        let command = Command(
            .object(["type": .string("add_patch"), "index": .int(Int64(count)), "patch": patch]))
        return await sendPatch(command) ? id : nil
    }

    /// Renames a patch. An unchanged name sends nothing.
    @discardableResult
    func renamePatch(_ id: ModelID, to name: String) async -> Bool {
        guard let row = patches.first(where: { $0.id == id }), row.name != name else { return true }
        return await sendPatch(
            Command(
                .object([
                    "type": .string("set_patch_property"), "id": .string(id.raw),
                    "property": .object(["type": .string("name"), "value": .string(name)]),
                ])))
    }

    /// Removes a patch. The engine refuses one a cue plays through, and the last one; it says why.
    @discardableResult
    func removePatch(_ id: ModelID) async -> Bool {
        await sendPatch(Command(.object(["type": .string("remove_patch"), "id": .string(id.raw)])))
    }

    /// Names cue output `output` of the patch (empty: its number). An unchanged name sends nothing.
    @discardableResult
    func renameOutput(_ id: ModelID, output: Int, to name: String) async -> Bool {
        let names = outputNames(of: id)
        guard names.indices.contains(output), names[output] != name else { return true }
        return await sendPatch(.setPatchOutputName(id, output: output, name: name))
    }

    /// Gives the patch `count` cue outputs (1 to 128), then puts its routing on this machine at the
    /// same length: the outputs it keeps keep their device outputs, a new one goes to the device output
    /// of its own number (counted round the device's outputs when it has fewer).
    @discardableResult
    func setOutputCount(_ id: ModelID, _ count: Int) async -> Bool {
        guard (1...LevelSet.maxCueOutputs).contains(count) else {
            message = loc("patchEditor.countRange")
            return false
        }
        guard let row = patches.first(where: { $0.id == id }) else { return false }
        guard row.outputCount != count else { return true }
        guard await sendPatch(.setPatchOutputCount(id, count)) else { return false }
        guard let mapping = row.mapping, !mapping.device.isEmpty, let fresh = patches.first(where: { $0.id == id })
        else { return true }
        let usable = max(1, usableChannels(of: mapping.device))
        let channels = (0..<count).map { i in
            mapping.channels.indices.contains(i) ? mapping.channels[i] : UInt32(i % usable)
        }
        return await sendMapping(fresh, PatchMapping(device: mapping.device, channels: channels))
    }

    private func sendPatch(_ command: Command) async -> Bool {
        switch await store.send(command: command) {
        case .success:
            message = nil
            await refresh()
            return true
        case .failure(let e):
            message = e.detailText
            return false
        }
    }

    /// The names of the patch's cue outputs as the model has them ("" for one never named).
    func outputNames(of id: ModelID) -> [String] {
        let outputs = store.model?.patches.first { $0.id == id }?.raw["outputs"]?.arrayValue ?? []
        return outputs.map { $0["name"]?.stringValue ?? "" }
    }

    /// Everything the window draws; the window rebuilds its controls only when this changes.
    struct DisplayState: Equatable {
        var devices: [DeviceEntry]
        var patches: [PatchRow]
        var loaded: Bool
        var message: String?
        var panic: Double?
        var goInterval: Double?
        var limits: VolumeLimits
        var osc: OscAccessTable
        var network: [NetworkPatchRow]
        var keys: [WorkspaceKey: KeyChord]
        var keyRefusal: KeyRefusal?
    }

    var displayState: DisplayState {
        DisplayState(
            devices: devices, patches: patches, loaded: loaded, message: message, panic: panicDuration,
            goInterval: minGoInterval, limits: limits, osc: osc, network: networkPatches, keys: keyMap.bound,
            keyRefusal: keyRefusal)
    }

    /// What keeps the patch from playing now, as the engine resolves it (or, from an engine that
    /// does not say, as the device list implies).
    func problem(of row: PatchRow) -> PatchProblem? {
        if let r = row.resolution {
            let device = r.json["device"]?.stringValue ?? row.mapping?.device ?? ""
            switch r.type {
            case "unmapped": return .unmapped
            case "device_missing": return .deviceMissing(device)
            case "channels_missing":
                return .channelsMissing(
                    device: device, needed: Int(r.json["needed"]?.doubleValue ?? 0),
                    available: Int(r.json["available"]?.doubleValue ?? 0))
            case "output_count_mismatch": return .outputCountMismatch
            default: return nil
            }
        }
        guard let mapping = row.mapping else { return .unmapped }
        guard devices.contains(where: { $0.name == mapping.device }) else { return .deviceMissing(mapping.device) }
        if mapping.channels.count != row.outputCount { return .outputCountMismatch }
        let usable = usableChannels(of: mapping.device)
        if let worst = mapping.channels.max(), Int(worst) >= usable {
            return .channelsMissing(device: mapping.device, needed: Int(worst) + 1, available: usable)
        }
        return nil
    }

    /// The devices a patch may be put on: every listed one, and the one it names (so a mapping to an
    /// absent device still shows what it names).
    func deviceChoices(for row: PatchRow) -> [String] {
        var names = devices.map(\.name)
        if let current = row.mapping?.device, !current.isEmpty, !names.contains(current) { names.append(current) }
        return names
    }

    /// The outputs of `device` a patch can use: all of them on the clock device, else no more than
    /// the clock device has (every device runs at its output count).
    func usableChannels(of device: String) -> Int {
        guard let entry = devices.first(where: { $0.name == device }) else { return 0 }
        let clock = store.status?.outputs.first { $0.primary }.map { Int($0.channels) } ?? 0
        guard !entry.primary, clock > 0 else { return entry.outputChannels }
        return min(entry.outputChannels, clock)
    }

    /// Every output the device has (shown, with a note when openLab cannot use them all).
    func channelCount(of device: String) -> Int {
        devices.first { $0.name == device }?.outputChannels ?? 0
    }

    // MARK: Editing

    /// Puts `row` on `device`, output i on device output `channels[i]` (default: the first
    /// outputs); nil `device` clears it (the engine pauses the cues playing through the patch, as
    /// QLab does). A cue that plays on keeps its device; the next start plays on the new one.
    func setMapping(_ row: PatchRow, device: String?, channels: [UInt32]? = nil) async {
        // The popup fires its action when the same device is picked again; that must not reset
        // the routing the show was built with to the first outputs.
        if channels == nil, device == row.mapping?.device { return }
        var mapping: PatchMapping?
        if let device {
            let usable = usableChannels(of: device)
            let chosen = channels ?? (0..<row.outputCount).map { UInt32($0) }
            guard chosen.count == row.outputCount, chosen.allSatisfy({ Int($0) < usable }) else {
                message = String(format: loc("settings.audio.tooFewChannels"), device, row.outputCount, usable)
                return
            }
            mapping = PatchMapping(device: device, channels: chosen)
        }
        _ = await sendMapping(row, mapping)
    }

    private func sendMapping(_ row: PatchRow, _ mapping: PatchMapping?) async -> Bool {
        let result = await store.setPatchMapping(row.id, mapping)
        switch result {
        case .success: message = nil
        case .failure(let e): message = describe(e)
        }
        await refresh()
        if case .success = result { return true }
        return false
    }

    /// Sends cue output `output` of the patch to device output `channel`. The whole routing goes at
    /// once, so a cue playing through the patch moves to it at once.
    func route(_ row: PatchRow, output: Int, to channel: UInt32) async {
        guard var mapping = row.mapping, mapping.channels.indices.contains(output),
            mapping.channels[output] != channel
        else { return }
        mapping.channels[output] = channel
        await setMapping(row, device: mapping.device, channels: mapping.channels)
    }

    // MARK: General

    static let panicRange = 0.0...60.0
    static let goIntervalRange = 0.0...10.0

    var panicDuration: Double? { store.model?.settings["panic_duration"]?.doubleValue }
    var minGoInterval: Double? { store.model?.settings["min_go_interval"]?.doubleValue }

    /// Sets `panic_duration` (seconds) from the text the operator typed. False, with a message,
    /// when it is not a number in range: the engine's value stays.
    @discardableResult
    func setPanicDuration(_ text: String) async -> Bool {
        await setNumber("panic_duration", text, Self.panicRange)
    }

    @discardableResult
    func setMinGoInterval(_ text: String) async -> Bool {
        await setNumber("min_go_interval", text, Self.goIntervalRange)
    }

    // MARK: Volume limits (PB-11)

    /// The workspace's volume limits, as the engine has them.
    var limits: VolumeLimits { store.volumeLimits }

    /// Sets the maximum volume limit (-30 to +12 dB) from typed text. False, with a message, when it
    /// is not a number in range: the engine's value stays. The engine puts every level above the new
    /// maximum at it, in the same undo step.
    @discardableResult
    func setOutputMax(_ text: String) async -> Bool {
        await setLimit("output_max_db", text, VolumeLimits.maxRange)
    }

    /// Sets the minimum volume limit (-180 to -40 dB): levels at or below it are silent.
    @discardableResult
    func setOutputMin(_ text: String) async -> Bool {
        await setLimit("output_min_db", text, VolumeLimits.minRange)
    }

    private func setLimit(_ type: String, _ text: String, _ range: ClosedRange<Double>) async -> Bool {
        let trimmed = text.trimmingCharacters(in: .whitespaces).replacingOccurrences(of: ",", with: ".")
        guard let value = Double(trimmed), value.isFinite, range.contains(value) else {
            message = loc("settings.audio.limitOutOfRange")
            return false
        }
        return await setNumber(type, trimmed, range)
    }

    private func setNumber(_ type: String, _ text: String, _ range: ClosedRange<Double>) async -> Bool {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        guard let value = Double(trimmed), value.isFinite, range.contains(value) else {
            message = String(format: loc("settings.general.outOfRange"), range.lowerBound, range.upperBound)
            return false
        }
        // The value is the one the engine has (the field was only left, or the old value typed
        // again): nothing to send, and an earlier refusal line no longer applies.
        if store.model?.settings[type]?.doubleValue == value {
            message = nil
            return true
        }
        switch await store.send(command: .setWorkspaceNumber(type, value)) {
        case .success:
            message = nil
            return true
        case .failure(let e):
            message = describe(e)
            return false
        }
    }

    private func describe(_ e: EngineError) -> String { e.detailText }
}

extension EngineError {
    /// The error in words, for a window that shows why an edit did not take.
    var detailText: String {
        switch self {
        case .notConnected, .disconnected: return loc("error.detail.notConnected")
        case .engine(_, let text) where !text.isEmpty: return text
        case .timeout: return loc("error.detail.timeout")
        default: return loc("error.detail.generic")
        }
    }
}
