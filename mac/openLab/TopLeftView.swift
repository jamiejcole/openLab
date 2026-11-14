// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// A flipped view: content laid out from the top left, as a scroll view's document view needs to
/// keep a short table at the top instead of the bottom.
final class TopLeftView: NSView {
    override var isFlipped: Bool { true }
}
