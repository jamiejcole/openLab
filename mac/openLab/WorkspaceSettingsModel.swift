// SPDX-License-Identifier: GPL-3.0-or-later

import Combine
import Foundation

/// What is wrong with one patch on this machine (WS-07): the settings window shows it beside the
/// patch so a silent patch is never a surprise at the show.
enum PatchProblem: Equatable {
    case unmapped
    case deviceMissing(String)
    case channelsMissing
    case outputCountMismatch
}

/// One output patch as the settings window lists it.
struct PatchRow: Equatable {
    let id: ModelID
    let name: String
    let outputCount: Int
    let mapping: PatchMapping?
}

/// The state behind the workspace settings window (AUD-07, WS-07, PB-02, PB-03). It reads and
/// writes through the `WorkspaceStore` only; the window draws what it holds.
///
/// The engine plays on one output device for its whole run. The device chosen here is kept for
/// the next start (`needsRestart` says when it differs from what plays now); the patch editor
/// maps patches onto the device that is playing.
@MainActor
final class WorkspaceSettingsModel: ObservableObject {
    let store: WorkspaceStore
    @Published private(set) var devices: [DeviceEntry] = []
    /// The device kept for the next engine start (nil: the system default).
    @Published private(set) var chosenDevice: String?
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
            if case .devices(let list, let selected)? = ok.data {
                // Assigned only on a change: each assignment publishes, and the window rebuilds.
                if devices != list { devices = list }
                if chosenDevice != selected { chosenDevice = selected }
                if !loaded { loaded = true }
            }
        case .failure(let e):
            if loaded { loaded = false }
            message = describe(e)
            return
        }
        switch await store.listPatchMappings() {
        case .success(let entries):
            let byPatch = Dictionary(entries.map { ($0.patch, $0.mapping) }, uniquingKeysWith: { first, _ in first })
            let rows = (store.model?.patches ?? []).map { object in
                PatchRow(
                    id: object.id, name: object.raw["name"]?.stringValue ?? "",
                    outputCount: object.raw["outputs"]?.arrayValue?.count ?? 0,
                    mapping: byPatch[object.id] ?? nil)
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

    /// The device the engine is playing on now, from the status message.
    var playing: DeviceStatus? { store.status?.device }

    /// What the engine will open at its next start: the chosen device, else the system default
    /// (nil when the list names none).
    var nextStartDevice: String? { chosenDevice ?? devices.first { $0.isDefault }?.name }

    /// The next start would open a different device from the one playing now.
    var needsRestart: Bool {
        guard let next = nextStartDevice, let name = playing?.name else { return false }
        return next != name
    }

    /// Everything the window draws; the window rebuilds its controls only when this changes.
    struct DisplayState: Equatable {
        var devices: [DeviceEntry]
        var chosen: String?
        var patches: [PatchRow]
        var playingName: String?
        var playingState: String?
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
            devices: devices, chosen: chosenDevice, patches: patches, playingName: playing?.name,
            playingState: playing?.state.raw, loaded: loaded, message: message, panic: panicDuration,
            goInterval: minGoInterval, limits: limits, osc: osc, network: networkPatches, keys: keyMap.bound,
            keyRefusal: keyRefusal)
    }

    /// The playing device is lost, failed or absent: patches on it are silent until it is back.
    var deviceProblem: Bool {
        guard let state = playing?.state else { return false }
        return state != .ok
    }

    /// The chosen device is not in the list (unplugged): the engine falls back to the default at
    /// its next start.
    var chosenDeviceMissing: Bool {
        guard loaded, let chosen = chosenDevice else { return false }
        return !devices.contains { $0.name == chosen }
    }

    func problem(of row: PatchRow) -> PatchProblem? {
        guard let mapping = row.mapping else { return .unmapped }
        guard let device = devices.first(where: { $0.name == mapping.device }) else {
            return .deviceMissing(mapping.device)
        }
        if mapping.channels.count != row.outputCount { return .outputCountMismatch }
        if mapping.channels.contains(where: { Int($0) >= device.outputChannels }) { return .channelsMissing }
        return nil
    }

    /// The devices a patch may be put on: the one playing, and the one it is on now (so a mapping
    /// to an absent device still shows what it names). With no device playing, every listed one.
    func deviceChoices(for row: PatchRow) -> [String] {
        var names: [String] = []
        if let name = playing?.name { names.append(name) } else { names = devices.map(\.name) }
        if let current = row.mapping?.device, !names.contains(current) { names.append(current) }
        return names
    }

    func channelCount(of device: String) -> Int {
        devices.first { $0.name == device }?.outputChannels ?? 0
    }

    // MARK: Editing

    /// Keeps `name` (nil: the system default) for the next engine start.
    func chooseDevice(_ name: String?) async {
        switch await store.setOutputDevice(name) {
        case .success:
            message = nil
            chosenDevice = name
        case .failure(let e): message = describe(e)
        }
        await refresh()
    }

    /// Puts `row` on `device`, output i on device channel `channels[i]`; nil `device` clears it
    /// (the engine pauses the cues playing through the patch, as QLab does).
    func setMapping(_ row: PatchRow, device: String?, channels: [UInt32]? = nil) async {
        // The popup fires its action when the same device is picked again; that must not reset
        // the routing the show was built with to the first channels.
        if channels == nil, device == row.mapping?.device { return }
        var mapping: PatchMapping?
        if let device {
            let count = channelCount(of: device)
            let chosen = channels ?? (0..<row.outputCount).map { UInt32($0) }
            guard chosen.count == row.outputCount, chosen.allSatisfy({ Int($0) < count }) else {
                message = String(format: loc("settings.audio.tooFewChannels"), device, row.outputCount)
                return
            }
            mapping = PatchMapping(device: device, channels: chosen)
        }
        switch await store.setPatchMapping(row.id, mapping) {
        case .success: message = nil
        case .failure(let e): message = describe(e)
        }
        await refresh()
    }

    /// Sets the whole channel list at once when one output's channel is changed.
    func setChannel(_ row: PatchRow, output: Int, channel: UInt32) async {
        guard var mapping = row.mapping, mapping.channels.indices.contains(output) else { return }
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
