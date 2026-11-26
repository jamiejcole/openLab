// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// A settings or editor window in which Esc does nothing but panic. The app's key monitor has
/// already panicked when Esc reaches the window, and a field being typed in still takes Esc to
/// end its editing first. A plain window's own cancelOperation answers Esc with macOS's "no such
/// key" beep after every panic.
final class PanicKeyWindow: NSWindow {
    override func cancelOperation(_ sender: Any?) {}
}
