// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

// Test media written at test time, shared by the app's unit tests and its UI tests (both targets
// compile this file): nothing binary is checked in.

/// A test media file the bundled stage host could not make.
struct TestMediaError: Error, CustomStringConvertible {
    let description: String
}

/// The app under test: the unit tests' host, or the app beside the UI tests' runner.
private var appBundleURL: URL {
    let main = Bundle.main.bundleURL
    if Bundle.main.bundleIdentifier == "dev.openlab.openLab" { return main }
    return main.deletingLastPathComponent().appendingPathComponent("openLab.app")
}

/// A 16-bit WAV of `seconds` at 8 kHz with `channels` channels (stereo by default): a quiet tone, each channel at half the one before.
func writeWAV(_ url: URL, seconds: Double, rate: Int = 8000, channels: Int = 2) throws {
    let frames = Int((seconds * Double(rate)).rounded())
    var d = Data()
    func u32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
    func u16(_ v: UInt16) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
    d.append(contentsOf: Array("RIFF".utf8))
    u32(UInt32(36 + frames * 2 * channels))
    d.append(contentsOf: Array("WAVEfmt ".utf8))
    u32(16)
    u16(1)
    u16(UInt16(channels))
    u32(UInt32(rate))
    u32(UInt32(rate * 2 * channels))
    u16(UInt16(2 * channels))
    u16(16)
    d.append(contentsOf: Array("data".utf8))
    u32(UInt32(frames * 2 * channels))
    d.reserveCapacity(d.count + frames * 2 * channels)
    for i in 0..<frames {
        let v = Int16(3000 * sin(2 * Double.pi * 440 * Double(i) / Double(rate)))
        for c in 0..<channels { u16(UInt16(bitPattern: v / Int16(1 << min(c, 14)))) }
    }
    try d.write(to: url)
}

/// A test movie or still made by the bundled stage host (test-support builds: `--make-test-media`):
/// every frame is the test card for its own number (288 x 160), so a stage probe reads which frame
/// is shown. `codec` is `h264`, `hevc` or `prores` (a movie of `frames` at `fps`) or `png`, `jpeg`,
/// `tiff` (a still). Nothing is checked in; a codec this Mac cannot encode fails the test.
///
/// Each kind of file is made once per test process (`TestMediaCache`) and copied from there: making
/// a movie takes the stage host most of a second, and the Video tests make four per test.
func makeTestMedia(_ url: URL, codec: String, frames: Int = 0, fps: Int = 25) throws {
    let fm = FileManager.default
    try fm.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    try? fm.removeItem(at: url)
    let made = try TestMediaCache.file(codec: codec, frames: frames, fps: fps, ext: url.pathExtension) {
        try runMakeTestMedia($0, codec: codec, frames: frames, fps: fps)
    }
    try fm.copyItem(at: made, to: url)
    // As if written now: a copy keeps the made file's date, and the engine tells a changed file by it.
    try fm.setAttributes([.modificationDate: Date()], ofItemAtPath: url.path)
}

/// The test media made so far in this test process, in a folder of its own that is removed when
/// the test bundle ends.
enum TestMediaCache {
    static let folder = FileManager.default.temporaryDirectory.appendingPathComponent(
        "openlab-test-media-\(ProcessInfo.processInfo.processIdentifier)", isDirectory: true)

    /// The file made for these arguments, made by `make` (given where to write it) the first time.
    static func file(codec: String, frames: Int, fps: Int, ext: String, make: (URL) throws -> Void) throws -> URL {
        let name = "\(codec)-\(frames)-\(fps)".map { $0.isLetter || $0.isNumber || $0 == "-" ? $0 : "_" }
        let url = folder.appendingPathComponent(String(name)).appendingPathExtension(ext)
        if FileManager.default.fileExists(atPath: url.path) { return url }
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        // Made under a temporary name and moved in whole: a make that fails leaves nothing to reuse.
        let partial = folder.appendingPathComponent("partial-\(UUID().uuidString)").appendingPathExtension(ext)
        defer { try? FileManager.default.removeItem(at: partial) }
        try make(partial)
        try FileManager.default.moveItem(at: partial, to: url)
        return url
    }

    static func removeAll() { try? FileManager.default.removeItem(at: folder) }
}

private func runMakeTestMedia(_ url: URL, codec: String, frames: Int, fps: Int) throws {
    let host = appBundleURL.appendingPathComponent(
        "Contents/Helpers/openLab Stage.app/Contents/MacOS/openlab-stage")
    let process = Process()
    process.executableURL = host
    process.arguments = ["--make-test-media", codec, url.path] + (frames > 0 ? [String(frames), String(fps)] : [])
    let errors = Pipe()
    process.standardError = errors
    process.standardOutput = FileHandle.nullDevice
    try process.run()
    let said = String(decoding: errors.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
    process.waitUntilExit()
    guard process.terminationStatus == 0 else {
        throw TestMediaError(description: "--make-test-media \(codec) \(url.lastPathComponent): \(said)")
    }
}

/// A media file at `url` of `seconds`, its kind by its extension: a WAV tone, a movie at 25 fps
/// (`.mov` H.264, `.mp4` HEVC) or a still (`.png`, `.jpg`, `.tiff`; `seconds` unused). `sound`
/// gives a movie audio tracks: one per `+`-joined part (`pcm:2:48000`, `aac:2:44100+pcm:6:48000`).
func writeTestMedia(_ url: URL, seconds: Double, sound: String? = nil) throws {
    let frames = max(1, Int((seconds * 25).rounded()))
    let tracks = sound.map { "+\($0)" } ?? ""
    switch url.pathExtension.lowercased() {
    case "mov": try makeTestMedia(url, codec: "h264" + tracks, frames: frames)
    case "mp4": try makeTestMedia(url, codec: "hevc" + tracks, frames: frames)
    case "png": try makeTestMedia(url, codec: "png")
    case "jpg", "jpeg": try makeTestMedia(url, codec: "jpeg")
    case "tif", "tiff": try makeTestMedia(url, codec: "tiff")
    default: try writeWAV(url, seconds: seconds)
    }
}
