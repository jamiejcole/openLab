// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Edit > Paste Cue Properties (ED-06; QLab 5, manual p. 154): a cue copied, other cues selected, a
/// sheet of the copied cue's properties to set on them. Off in Show Mode, which opens no sheet.
extension WorkspaceWindowController {
    /// The item can be chosen: Edit Mode, a cue on the pasteboard, cues selected, no sheet up.
    var canPasteProperties: Bool {
        !isShowMode && pasteSheet == nil && window?.attachedSheet == nil && !cueList.selectedIDs().isEmpty
            && editor.clipboardCue != nil
    }

    /// Opens the sheet for `cues` (the selection when it was chosen: the playhead may move it while
    /// the sheet is up, and the paste is for what the operator chose).
    func promptPasteProperties(onto cues: [ModelID]) {
        guard !isShowMode, pasteSheet == nil, let window, window.attachedSheet == nil, let source = editor.clipboardCue,
            let model = store.model
        else { return NSSound.beep() }
        let targets = cues.compactMap { model.cue($0) }
        let offered = PasteProperty.offered(from: source, to: targets)
        guard !targets.isEmpty, !offered.isEmpty else { return NSSound.beep() }
        let name = [source.number, source.displayName].compactMap { $0 }.filter { !$0.isEmpty }
            .joined(separator: " \u{00B7} ")
        let label = name.isEmpty ? CueKindStyle.kindLabel(source.kindType) : name
        let title =
            targets.count == 1
            ? String(format: loc("paste.title.one"), label) : String(format: loc("paste.title"), label, targets.count)
        let all = Set(offered.flatMap(\.properties))
        let sheet = PastePropertiesSheet(
            title: title, offered: offered, ticked: PastePropertiesSheet.remembered(pasteDefaults).intersection(all))
        pasteSheet = sheet
        sheet.onDone = { [weak self] ticked in
            guard let self else { return }
            pasteSheet = nil
            guard let ticked else { return }
            PastePropertiesSheet.remember(ticked, offered: all, in: pasteDefaults)
            Task { await self.editor.pasteProperties(ticked, from: source, onto: cues) }
        }
        if let presentSheet { presentSheet(sheet.window) } else { window.beginSheet(sheet.window) }
    }
}
