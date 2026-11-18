// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The audio part of the application preferences (UI-01, AUD-21): this machine's output devices,
/// the buffer size each is asked for, and the clock device the engine opens at its next start.
/// Machine settings, not the show's: it reads and writes them through the app's own engine
/// connection, which the window never sees.
@MainActor
final class AudioPreferencesModel {
    /// The buffer sizes offered, in frames (the engine takes 16 to 1024; QLab 5 offers powers of two).
    static let bufferSizes: [Int] = [16, 32, 64, 128, 256, 512, 1024]

    private let client: EngineClient
    /// Set by `refresh`; a test sets it to draw a device the engine's host does not have.
    var devices: [DeviceEntry] = []
    /// The device kept for the next engine start (nil: the system default).
    private(set) var chosenDevice: String?
    /// The device list was read: anything here can be changed.
    private(set) var loaded = false
    /// The last refusal or failure, in words; cleared by the next success.
    var message: String?

    init(client: EngineClient) { self.client = client }

    /// How many backups of each workspace this machine keeps (nil: not read yet).
    private(set) var backupKeep: UInt32?
    static let backupKeepRange: ClosedRange<UInt32> = 0...1000

    /// Reads the backup count (with `refresh`).
    func refreshBackups() async {
        if case .success(let ok) = await client.send({ .request(id: $0, .backupKeep) }),
            case .backupKeep(let count)? = ok.data
        {
            backupKeep = count
        }
    }

    /// Keeps `text` backups of each workspace: a whole number; 0 turns backups off. False, with a
    /// message, when it is not a number in range or the engine refuses.
    @discardableResult
    func setBackupKeep(_ text: String) async -> Bool {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        guard let count = UInt32(trimmed), Self.backupKeepRange.contains(count) else {
            message = String(
                format: loc("prefs.general.backups.range"), Int(Self.backupKeepRange.lowerBound),
                Int(Self.backupKeepRange.upperBound))
            return false
        }
        let result = await client.send({ .request(id: $0, .setBackupKeep(count: count)) })
        await refreshBackups()
        switch result {
        case .success:
            message = nil
            return true
        case .failure(let e):
            message = e.detailText
            return false
        }
    }

    func refresh() async {
        switch await client.send({ .request(id: $0, .listDevices) }) {
        case .success(let ok):
            if case .devices(let list, let selected)? = ok.data {
                devices = list
                chosenDevice = selected
                loaded = true
            }
        case .failure(let e):
            loaded = false
            message = e.detailText
        }
    }

    /// The device whose clock the show runs on now (the engine's primary).
    var primary: String? { devices.first { $0.primary }?.name }

    /// What the engine opens at its next start: the chosen device, else the system default.
    var nextStartDevice: String? { chosenDevice ?? devices.first { $0.isDefault }?.name }

    /// The next start would open another clock device than the one playing now.
    var needsRestart: Bool {
        guard let next = nextStartDevice, let primary else { return false }
        return next != primary
    }

    /// The chosen device is not in the list (unplugged): the next start falls back to the default.
    var chosenDeviceMissing: Bool {
        guard loaded, let chosen = chosenDevice else { return false }
        return !devices.contains { $0.name == chosen }
    }

    /// Keeps `name` (nil: the system default) as the clock device for the next engine start.
    func chooseDevice(_ name: String?) async {
        switch await client.send({ .request(id: $0, .setOutputDevice(device: name)) }) {
        case .success: message = nil
        case .failure(let e): message = e.detailText
        }
        await refresh()
    }

    /// Asks `device` for `frames` per callback. A device that plays reopens at once at that size
    /// (its cues play on after the gap), so nothing needs a relaunch.
    func setBufferSize(_ device: String, frames: Int) async {
        guard devices.first(where: { $0.name == device })?.bufferFrames != frames else { return }
        switch await client.send({ .request(id: $0, .setBufferSize(device: device, frames: UInt32(frames))) }) {
        case .success: message = nil
        case .failure(let e): message = e.detailText
        }
        await refresh()
    }
}
