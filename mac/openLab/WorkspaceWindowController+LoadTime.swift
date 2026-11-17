// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// What the Load to Time tool holds for one workspace window.
struct LoadTimeState {
    /// The bar is up, in the place of the cue-type row.
    var isOpen = false
    /// Whether the cue-type row was hidden when the bar opened; it comes back so.
    var toolbarWasHidden = false
}

/// Load to Time (QLab manual p. 149): Tools > Load to Time cycles the bar through three states, and a
/// time typed or the slider let go loads the standing-by cue to it. It is an operator's tool, so it
/// works in Show Mode too.
extension WorkspaceWindowController {
    func wireLoadTime() {
        loadBar.onLoad = { [weak self] seconds in self?.loadStandingBy(to: seconds) }
        loadBar.onSubmit = { [weak self] text in
            guard let self else { return }
            if let id = standingByCue, let problem = applyLoadTime(text, to: id) { showNotice(problem) }
            else if standingByCue == nil { showNotice(loc("loadTime.nothing")) }
        }
        loadBar.onDone = { [weak self] in self?.closeLoadBar() }
    }

    /// Cmd-T cycles: the bar opens; then the cursor goes to its field; then, with the cursor in the
    /// field, the bar closes and the focus returns to the cue list.
    func toggleLoadToTime() {
        if !loadTime.isOpen {
            openLoadBar()
        } else if !loadBar.fieldHasFocus {
            loadBar.focusField()
        } else {
            closeLoadBar()
        }
    }

    /// The cue GO would start: the shown list's playhead cue.
    var standingByCue: ModelID? { cueList.listID.flatMap { store.playheads[$0] } }

    /// The bar takes the place of the cue-type row (and of Find's bar, which shares it).
    func openLoadBar() {
        guard !loadTime.isOpen else { return }
        closeFind()
        loadTime.isOpen = true
        loadTime.toolbarWasHidden = cueToolbar.isHidden
        cueToolbar.isHidden = true
        loadBar.isHidden = false
        syncLoadBar()
    }

    func closeLoadBar() {
        guard loadTime.isOpen else { return }
        let hadFocus = loadBar.fieldHasFocus || (window?.firstResponder as? NSView)?.isDescendant(of: loadBar) == true
        let hidden = loadTime.toolbarWasHidden
        loadTime = LoadTimeState()
        loadBar.clear()
        loadBar.isHidden = true
        cueToolbar.isHidden = hidden
        if hadFocus { window?.makeFirstResponder(cueList.outlineView) }
    }

    /// The slider spans the standing-by cue's length; it follows the playhead.
    func syncLoadBar() {
        guard loadTime.isOpen else { return }
        loadBar.setLength(standingByCue.flatMap { cueList.node($0) }.flatMap { cueList.restingDuration($0) })
    }

    /// The field's answer for Load to Time. Nil: the load was sent; else the notice to show. A
    /// negative time counts back from the cue's end (the engine's `load` takes it as such).
    func applyLoadTime(_ text: String, to id: ModelID) -> String? {
        guard let seconds = CueTime.parseSigned(text) else { return loc("cue.prompt.badTime") }
        loadBar.show(loaded: seconds)
        loadToTime(id, seconds)
        return nil
    }

    /// Loads the standing-by cue (a group: its sequence) to `seconds`; negative counts back from its end.
    func loadStandingBy(to seconds: Double) {
        guard let id = standingByCue else { return showNotice(loc("loadTime.nothing")) }
        loadToTime(id, seconds)
    }
}
