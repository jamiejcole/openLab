// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import ObjectiveC

/// Diagnostic for #530: macOS 27 logs "Invalid view geometry: width is negative" without naming the view,
/// and macOS 26 does not log it. While installed, any view given a negative frame size is written to stderr
/// as `NEGATIVE-FRAME` with its class path, accessibility id, the size and the call stack, so a CI log names
/// it. It changes nothing: the original setter still runs. Remove once the view is fixed.
enum NegativeFrameProbe {
    nonisolated(unsafe) private static var installed = false
    nonisolated(unsafe) private static var reported = 0

    static func install() {
        guard !installed else { return }
        installed = true
        if let method = class_getInstanceMethod(NSView.self, #selector(NSView.setFrameSize(_:))) {
            typealias Original = @convention(c) (NSView, Selector, CGSize) -> Void
            let original = unsafeBitCast(method_getImplementation(method), to: Original.self)
            let block: @convention(block) (NSView, CGSize) -> Void = { view, size in
                if size.width < 0 || size.height < 0 { note(view, "setFrameSize", size) }
                original(view, #selector(NSView.setFrameSize(_:)), size)
            }
            method_setImplementation(method, imp_implementationWithBlock(block))
        }
        if let method = class_getInstanceMethod(NSView.self, NSSelectorFromString("setFrame:")) {
            typealias Original = @convention(c) (NSView, Selector, CGRect) -> Void
            let original = unsafeBitCast(method_getImplementation(method), to: Original.self)
            let block: @convention(block) (NSView, CGRect) -> Void = { view, rect in
                if rect.width < 0 || rect.height < 0 { note(view, "setFrame", rect.size) }
                original(view, NSSelectorFromString("setFrame:"), rect)
            }
            method_setImplementation(method, imp_implementationWithBlock(block))
        }
    }

    /// AppKit lays views out on the main thread; a setter reached from another one is not worth the risk.
    private static func note(_ view: NSView, _ what: String, _ size: CGSize) {
        guard Thread.isMainThread else { return }
        MainActor.assumeIsolated { report(view, what, size) }
    }

    @MainActor
    private static func report(_ view: NSView, _ what: String, _ size: CGSize) {
        guard reported < 30 else { return }
        reported += 1
        var path = String(describing: type(of: view))
        var up = view.superview
        while let parent = up {
            path = String(describing: type(of: parent)) + "/" + path
            up = parent.superview
        }
        let frames = Thread.callStackSymbols.filter {
            !$0.contains("NegativeFrameProbe") && !$0.contains("assumeIsolated")
        }
        let stack = frames.prefix(14).joined(separator: "\n  ")
        fputs("NEGATIVE-FRAME \(what) \(size) view=\(path) id=\(view.accessibilityIdentifier())\n  \(stack)\n", stderr)
    }
}
