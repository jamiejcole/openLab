// SPDX-License-Identifier: GPL-3.0-or-later
//
// Builds the app icon: the flask of openlab-logo-1024.png on a dark rounded-square tile (824 pt in
// a 1024 canvas, transparent outside it), at every size the asset catalogue needs.
//   swift mac/Branding/make-icon.swift <slate|black> <output folder>
// Writes icon-16, -32, -64, -128, -256, -512 and -1024 .png there.
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let tints: [String: (top: [CGFloat], bottom: [CGFloat])] = [
    "slate": ([0x1b, 0x20, 0x26].map { CGFloat($0) / 255 }, [0x10, 0x13, 0x17].map { CGFloat($0) / 255 }),
    "black": ([0, 0, 0], [0x0a, 0x0a, 0x0a].map { CGFloat($0) / 255 }),
]
guard CommandLine.arguments.count == 3, let tint = tints[CommandLine.arguments[1]] else {
    FileHandle.standardError.write(Data("usage: make-icon.swift <slate|black> <output folder>\n".utf8))
    exit(2)
}
let out = URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true)
try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)
let here = URL(fileURLWithPath: CommandLine.arguments[0]).deletingLastPathComponent()
guard let src = CGImageSourceCreateWithURL(here.appendingPathComponent("openlab-logo-1024.png") as CFURL, nil),
    let logo = CGImageSourceCreateImageAtIndex(src, 0, nil)
else { fatalError("cannot read openlab-logo-1024.png") }

let space = CGColorSpace(name: CGColorSpace.sRGB)!
let info = CGImageAlphaInfo.premultipliedLast.rawValue

/// The box of pixels that are not near transparent, y up from the bottom.
func inkBox(_ image: CGImage) -> CGRect {
    let w = image.width
    let h = image.height
    var data = [UInt8](repeating: 0, count: w * h * 4)
    let ctx = CGContext(
        data: &data, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4, space: space, bitmapInfo: info)!
    ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
    var minX = w
    var maxX = 0
    var minY = h
    var maxY = 0
    for y in 0..<h {
        for x in 0..<w where data[(y * w + x) * 4 + 3] > 40 {
            minX = min(minX, x)
            maxX = max(maxX, x)
            minY = min(minY, y)
            maxY = max(maxY, y)
        }
    }
    // Context rows run top to bottom in memory; flip to y up.
    return CGRect(x: minX, y: h - 1 - maxY, width: maxX - minX + 1, height: maxY - minY + 1)
}

let size: CGFloat = 1024
let tile: CGFloat = 824
let radius: CGFloat = 185
let ink = inkBox(logo)
let scale = tile * 0.72 / ink.height
let ctx = CGContext(
    data: nil, width: 1024, height: 1024, bitsPerComponent: 8, bytesPerRow: 0, space: space, bitmapInfo: info)!
let tileRect = CGRect(x: (size - tile) / 2, y: (size - tile) / 2, width: tile, height: tile)
ctx.saveGState()
ctx.addPath(CGPath(roundedRect: tileRect, cornerWidth: radius, cornerHeight: radius, transform: nil))
ctx.clip()
let gradient = CGGradient(
    colorsSpace: space,
    colors: [
        CGColor(colorSpace: space, components: tint.top + [1])!,
        CGColor(colorSpace: space, components: tint.bottom + [1])!,
    ] as CFArray,
    locations: [0, 1])!
ctx.drawLinearGradient(
    gradient, start: CGPoint(x: 0, y: tileRect.maxY), end: CGPoint(x: 0, y: tileRect.minY), options: [])
ctx.restoreGState()
// The flask's ink box centred on the tile, nudged up a little: its wide base is the heavy part.
let drawW = CGFloat(logo.width) * scale
let drawH = CGFloat(logo.height) * scale
let cx = tileRect.midX - (ink.midX * scale - drawW / 2) - drawW / 2
let cy = tileRect.midY + 6 - (ink.midY * scale - drawH / 2) - drawH / 2
ctx.interpolationQuality = .high
ctx.draw(logo, in: CGRect(x: cx, y: cy, width: drawW, height: drawH))
let master = ctx.makeImage()!

func write(_ image: CGImage, _ name: String) {
    guard
        let dest = CGImageDestinationCreateWithURL(
            out.appendingPathComponent(name) as CFURL, UTType.png.identifier as CFString, 1, nil)
    else { fatalError("cannot write \(name)") }
    CGImageDestinationAddImage(dest, image, nil)
    guard CGImageDestinationFinalize(dest) else { fatalError("cannot write \(name)") }
}
for n in [1024, 512, 256, 128, 64, 32, 16] {
    let small = CGContext(
        data: nil, width: n, height: n, bitsPerComponent: 8, bytesPerRow: 0, space: space, bitmapInfo: info)!
    small.interpolationQuality = .high
    small.draw(master, in: CGRect(x: 0, y: 0, width: n, height: n))
    write(small.makeImage()!, "icon-\(n).png")
}
