// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// What Find (ED-09) holds for one workspace window.
struct FindState {
    /// The bar is up, in the place of the cue-type row.
    var isOpen = false
    /// Whether the cue-type row was hidden when the bar opened (View > Toolbar); it comes back so.
    var toolbarWasHidden = false
    var query = ""
    /// The cues of the shown list that match, in list order (collapsed groups included).
    var matches: [ModelID] = []
    /// The match Next / Previous last went to.
    var currentID: ModelID?
    /// How many times the list was scanned, for tests.
    private(set) var scans = 0
    mutating func countScan() { scans += 1 }

    var currentIndex: Int? { currentID.flatMap { matches.firstIndex(of: $0) } }
}

/// Find (ED-09), QLab's Edit > Find: one cue list at a time, no replace, and off in Show Mode.
extension WorkspaceWindowController {
    func wireFind() {
        findBar.onQueryChange = { [weak self] in self?.setFindQuery($0) }
        findBar.onStep = { [weak self] forward in self?.stepFind(forward: forward) }
        findBar.onSelectFound = { [weak self] in self?.selectFound() }
        findBar.onDone = { [weak self] in self?.closeFind() }
        // `cueListApplied` rescans: only a reset or a model change can change what is found, and
        // runtime and progress ticks (up to 30 a second while cues play) must not.
    }

    /// Cmd-F cycles as in QLab: the bar opens; then the cursor goes to its field; then, with the
    /// cursor in the field, the bar closes and the focus returns to the cue list.
    func toggleFind() {
        guard !isShowMode else { return NSSound.beep() }
        if !find.isOpen {
            openFind()
        } else if !findBar.fieldHasFocus {
            findBar.focusField()
        } else {
            closeFind()
        }
    }

    /// The bar replaces the cue-type row.
    func openFind() {
        guard !isShowMode, !find.isOpen else { return }
        find.isOpen = true
        find.toolbarWasHidden = cueToolbar.isHidden
        cueToolbar.isHidden = true
        findBar.isHidden = false
        findBar.focusField()
    }

    /// Done, Esc in the field, Cmd-F in the field, or Show Mode: the search is cleared and the cue-type
    /// row is back.
    func closeFind() {
        guard find.isOpen else { return }
        let hadFocus = findBar.fieldHasFocus || (window?.firstResponder as? NSView)?.isDescendant(of: findBar) == true
        let hidden = find.toolbarWasHidden
        find = FindState()
        findBar.clear()
        findBar.isHidden = true
        cueToolbar.isHidden = hidden
        if hadFocus { window?.makeFirstResponder(cueList.outlineView) }
    }

    /// Edit > Find Next / Find Previous. With the bar closed they open it, as Cmd-F does.
    func findNext(forward: Bool) {
        guard !isShowMode else { return NSSound.beep() }
        if find.isOpen { stepFind(forward: forward) } else { openFind() }
    }

    func setFindQuery(_ text: String) {
        find.query = text
        find.currentID = nil
        recomputeFind()
    }

    /// The store changed or another list was shown: the matches follow the list as it is now.
    func refreshFind() {
        guard find.isOpen else { return }
        recomputeFind()
    }

    private func recomputeFind() {
        find.countScan()
        find.matches = CueFinder.matches(find.query, in: cueList.roots)
        if let id = find.currentID, !find.matches.contains(id) { find.currentID = nil }
        findBar.setCount(
            found: find.matches.count, current: find.currentIndex,
            searching: !find.query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
    }

    /// Selects the next (previous) found cue, wrapping around, and shows it: its groups open and the
    /// list scrolls. The selection is made as a click would, so the playhead follows it.
    func stepFind(forward: Bool) {
        guard find.isOpen, !isShowMode else { return }
        recomputeFind()
        // A click elsewhere ends the walk: the next step starts from where the selection is now.
        let selected = cueList.selectedIDs()
        let current = selected == find.currentID.map({ [$0] }) ? find.currentIndex : nil
        guard
            let index = CueFinder.step(
                find.matches, current: current, forward: forward, anchor: selected.last,
                order: CueFinder.order(of: cueList.roots))
        else { return NSSound.beep() }
        find.currentID = find.matches[index]
        cueList.selectAsUser([find.matches[index]])
        findBar.setCount(found: find.matches.count, current: index, searching: true)
    }

    /// Select Found: every found cue at once.
    func selectFound() {
        guard find.isOpen, !isShowMode else { return }
        recomputeFind()
        guard !find.matches.isEmpty else { return NSSound.beep() }
        cueList.selectAsUser(find.matches)
    }
}
