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

    /// The frame of `url`, or nil when it cannot be read.
    static func load(url: URL, still: Bool) async -> NSImage? {
        let frame = await Task.detached(priority: .utility) { () -> Frame? in
            let image = still ? readStill(url) : readFirstFrame(url)
            return image.map { Frame(image: $0) }
        }.value
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
