// SPDX-License-Identifier: GPL-3.0-or-later

import AVFoundation
import AppKit
import ImageIO

/// A Video cue's picture as the Geometry tab draws it: a still as it is, a movie as its first frame,
/// read at a small size off the main thread. A file that cannot be read gives nothing (the tab keeps
/// drawing the picture's shape alone).
enum MediaFrame {
    /// The longest side of the frame kept, in pixels: the tab draws it a few hundred points wide at most.
    static let longestSide = 640

    private struct Frame: @unchecked Sendable { let image: CGImage }

    /// The reads run one at a time on a queue of their own, never on the shared pool: a file on a
    /// dead share can block its read for as long as the volume takes to answer.
    private static let queue = DispatchQueue(label: "openlab.media-frame", qos: .utility)

    private final class Cancelled: @unchecked Sendable {
        private let lock = NSLock()
        private var value = false
        var isSet: Bool { lock.lock(); defer { lock.unlock() }; return value }
        func set() { lock.lock(); value = true; lock.unlock() }
    }

    /// The frame of `url`, or nil when it cannot be read or the task was cancelled (a read still
    /// queued when cancelled never starts).
    static func load(url: URL, still: Bool) async -> NSImage? {
        let cancelled = Cancelled()
        let frame: Frame? = await withTaskCancellationHandler {
            await withCheckedContinuation { (c: CheckedContinuation<Frame?, Never>) in
                queue.async {
                    guard !cancelled.isSet else { return c.resume(returning: nil) }
                    let image = still ? readStill(url) : readFirstFrame(url)
                    c.resume(returning: image.map { Frame(image: $0) })
                }
            }
        } onCancel: {
            cancelled.set()
        }
        guard !Task.isCancelled else { return nil }
        return frame.map { NSImage(cgImage: $0.image, size: NSSize(width: $0.image.width, height: $0.image.height)) }
    }

    static func readStill(_ url: URL) -> CGImage? {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: longestSide,
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    }

    static func readFirstFrame(_ url: URL) -> CGImage? {
        let generator = AVAssetImageGenerator(asset: AVURLAsset(url: url))
        generator.appliesPreferredTrackTransform = true
        generator.maximumSize = CGSize(width: longestSide, height: longestSide)
        generator.requestedTimeToleranceBefore = .zero
        generator.requestedTimeToleranceAfter = CMTime(seconds: 1, preferredTimescale: 600)
        return try? generator.copyCGImage(at: .zero, actualTime: nil)
    }
}
