// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The I/O tab's Video cue rows (QLab 5, manual pp. 211-212): the file, the format its probe read,
/// the stage pop-up (with "none") and the stage's pixel size, and a line saying why the audio patch
/// is off. Held by the inspector; hidden for an Audio cue.
@MainActor
final class VideoIOControls {
    let fileLabel = NSTextField(labelWithString: "")
    let chooseButton = NSButton(title: loc("inspector.io.choose"), target: nil, action: nil)
    let formatLabel = NSTextField(labelWithString: "")
    let stagePopup = NSPopUpButton()
    let stageSizeLabel = NSTextField(labelWithString: "")
    let audioNote = NSTextField(labelWithString: loc("inspector.io.noVideoSound"))
    /// The rows only a Video cue has (the grid's rows; hidden for an Audio cue).
    var videoRows: [NSGridRow] = []
    var noteRow: NSGridRow?
    /// Item i of the stage pop-up is `stageIDs[i]`'s stage; nil is the separator or "none".
    var stageIDs: [ModelID?] = []
    var stageNames: [String] = []

    init() {
        fileLabel.setAX("inspector-io-file", label: loc("inspector.io.file"))
        fileLabel.lineBreakMode = .byTruncatingMiddle  // the file name at the end is the part that matters
        // A long path truncates; it must not hold the inspector, and so the window, wider.
        fileLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        fileLabel.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        chooseButton.setAX("inspector-io-choose", label: loc("inspector.io.choose"))
        chooseButton.controlSize = .small
        chooseButton.setContentCompressionResistancePriority(.required, for: .horizontal)
        formatLabel.setAX("inspector-io-format", label: loc("inspector.io.format"))
        formatLabel.lineBreakMode = .byTruncatingTail
        formatLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        stagePopup.setAX("inspector-io-stage", label: loc("inspector.io.stage"))
        stageSizeLabel.setAX("inspector-io-stage-size", label: loc("inspector.io.stageSize"))
        stageSizeLabel.textColor = .secondaryLabelColor
        audioNote.setAX("inspector-io-audio-note")
        audioNote.textColor = .secondaryLabelColor
        audioNote.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        audioNote.lineBreakMode = .byTruncatingTail
        audioNote.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
    }

    /// What the format row says for a probed file: a movie's codec, pixel size and frame rate, or
    /// a still's format and pixel size.
    static func formatText(_ p: PictureFormat) -> String {
        let size = String(format: loc("inspector.io.pixels"), p.width, p.height)
        if p.type == "still" {
            let name = p.format.map { loc("inspector.io.still.\($0)") } ?? ""
            return String(format: loc("inspector.io.stillFormat"), name, size)
        }
        let codec = p.codec.map { loc("inspector.io.codec.\($0)") } ?? ""
        let fps = p.fps.map { String(format: "%.3g", $0) } ?? "?"
        return String(format: loc("inspector.io.videoFormat"), codec, size, fps)
    }
}

extension InspectorViewController {
    /// The I/O tab (QLab 5 has the same rows): for a Video cue its file, format, stage and the audio
    /// patch (off until sound from video files plays, #645); for an Audio cue the output patch alone.
    func ioTab(row: (String, NSView) -> NSView, column: ([NSView], CGFloat?) -> NSStackView) -> NSTabViewItem {
        outputPatchPopup.setAX("inspector-output-patch", label: loc("inspector.io.patch"))
        outputPatchPopup.target = self
        outputPatchPopup.action = #selector(outputPatchChosen)
        outputPatchPopup.widthAnchor.constraint(greaterThanOrEqualToConstant: 200).isActive = true
        editPatchButton.setAX(AX.inspectorEditPatch, label: loc("inspector.io.editPatch"))
        editPatchButton.target = self
        editPatchButton.action = #selector(editPatchClicked)
        let v = videoIO
        v.chooseButton.target = self
        v.chooseButton.action = #selector(chooseClicked)
        v.stagePopup.target = self
        v.stagePopup.action = #selector(stageChosen)
        v.stagePopup.widthAnchor.constraint(greaterThanOrEqualToConstant: 200).isActive = true
        func label(_ key: String) -> NSTextField {
            let l = NSTextField(labelWithString: loc(key))
            l.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            l.textColor = .secondaryLabelColor
            l.alignment = .right
            return l
        }
        func line(_ views: [NSView]) -> NSStackView {
            let s = NSStackView(views: views)
            s.spacing = 8
            return s
        }
        let grid = NSGridView(views: [
            [label("inspector.io.file"), line([v.fileLabel, v.chooseButton])],
            [label("inspector.io.format"), v.formatLabel],
            [label("inspector.io.stage"), line([v.stagePopup, v.stageSizeLabel])],
            [label("inspector.io.patch"), line([outputPatchPopup, editPatchButton])],
            [NSGridCell.emptyContentView, v.audioNote],
        ])
        grid.rowSpacing = 8
        grid.columnSpacing = 8
        grid.rowAlignment = .firstBaseline
        grid.column(at: 0).xPlacement = .trailing
        v.videoRows = [grid.row(at: 0), grid.row(at: 1), grid.row(at: 2)]
        v.noteRow = grid.row(at: 4)
        let content = column([grid], nil)
        content.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 8, right: 12)
        // A long path or format gives up its middle; the grid fills the tab so it can.
        grid.widthAnchor.constraint(lessThanOrEqualTo: content.widthAnchor, constant: -24).isActive = true
        let item = NSTabViewItem(identifier: "io")
        item.label = loc("inspector.tab.io")
        item.view = content
        return item
    }

    /// The pop-up follows the model: every output patch by name. Rebuilt only when the list changes,
    /// so an open menu is not disturbed. A cue whose patch is gone shows no choice.
    func renderIO(_ cue: Cue, model: WorkspaceModel) {
        guard cue.takesFile else { return }
        let video = cue.kindType == "video"
        for r in videoIO.videoRows { r.isHidden = !video }
        videoIO.noteRow?.isHidden = !video
        let patches = model.patches
        let names = patches.map { p -> String in
            let name = p.raw["name"]?.stringValue ?? ""
            return name.isEmpty ? loc("settings.audio.untitledPatch") : name
        }
        let ids = patches.map(\.id)
        if ids != outputPatchIDs || names != outputPatchNames {
            outputPatchIDs = ids
            outputPatchNames = names
            outputPatchPopup.menu?.removeAllItems()
            for name in names {
                outputPatchPopup.menu?.addItem(NSMenuItem(title: name, action: nil, keyEquivalent: ""))
            }
        }
        if video {
            renderVideoIO(cue, model: model)
            return
        }
        let current = cue.raw["kind"]?["patch"]?.stringValue
        let chosen = patches.firstIndex { $0.id.raw == current } ?? -1
        if outputPatchPopup.indexOfSelectedItem != chosen { outputPatchPopup.selectItem(at: chosen) }
        outputPatchPopup.isEnabled = !isLocked
        // One cue's patch: with several cues shown, or a patch that is gone, there is none to edit.
        editPatchButton.isEnabled = !isLocked && chosen >= 0 && !isBatch
    }

    /// A Video cue's rows: its file and the format the engine's probe read, its stage, and the audio
    /// patch shown off (a Video cue plays no sound yet: #645).
    private func renderVideoIO(_ cue: Cue, model: WorkspaceModel) {
        let v = videoIO
        let media = cue.raw["kind"]?["media"]?.stringValue ?? ""
        let path = CueRowMenu.mediaURL(media, showPath: store.document?.path)?.path ?? media
        v.fileLabel.stringValue = media.isEmpty ? loc("inspector.target.noFile") : path
        v.fileLabel.textColor = media.isEmpty ? .secondaryLabelColor : .labelColor
        v.fileLabel.toolTip = media.isEmpty ? nil : path
        v.chooseButton.isEnabled = !isLocked && !isBatch
        let picture = store.mediaDurations[cue.id]?.picture
        v.formatLabel.stringValue =
            picture.map(VideoIOControls.formatText) ?? (media.isEmpty ? "" : loc("inspector.io.formatUnknown"))
        let stages = model.stages
        let names = stages.map { $0.raw["name"]?.stringValue ?? "" }
        let ids: [ModelID?] = stages.map(\.id) + [nil, nil]
        if ids != v.stageIDs || names != v.stageNames {
            v.stageNames = names
            v.stageIDs = ids
            v.stagePopup.menu?.removeAllItems()
            for name in names { v.stagePopup.menu?.addItem(NSMenuItem(title: name, action: nil, keyEquivalent: "")) }
            v.stagePopup.menu?.addItem(.separator())
            v.stagePopup.menu?.addItem(NSMenuItem(title: loc("inspector.io.stageNone"), action: nil, keyEquivalent: ""))
        }
        let current = cue.raw["kind"]?["stage"]?.stringValue
        // None is the last item; a stage that is gone shows no choice.
        let chosen = current == nil ? v.stageIDs.count - 1 : stages.firstIndex { $0.id.raw == current } ?? -1
        if v.stagePopup.indexOfSelectedItem != chosen { v.stagePopup.selectItem(at: chosen) }
        v.stagePopup.isEnabled = !isLocked
        v.stageSizeLabel.stringValue = Self.stageSize(stages.first { $0.id.raw == current })
        if outputPatchPopup.indexOfSelectedItem != -1 { outputPatchPopup.selectItem(at: -1) }
        outputPatchPopup.isEnabled = false
        editPatchButton.isEnabled = false
    }

    /// A stage's pixel size as the I/O tab shows it beside the pop-up; empty for none.
    static func stageSize(_ stage: ModelObject?) -> String {
        guard let stage, let w = stage.raw["width"]?.doubleValue, let h = stage.raw["height"]?.doubleValue else {
            return ""
        }
        return String(format: loc("inspector.io.pixels"), Int(w), Int(h))
    }

    /// The patch editor of the shown cue's patch (QLab 5 manual p. 189: the I/O tab's edit button).
    @objc func editPatchClicked() {
        guard let raw = cue?.raw["kind"]?["patch"]?.stringValue, case let id = ModelID(raw),
            store.model?.patches.contains(where: { $0.id == id }) == true
        else { return }
        onEditPatch?(id)
    }

    @objc func outputPatchChosen() {
        let i = outputPatchPopup.indexOfSelectedItem
        guard outputPatchIDs.indices.contains(i) else { return }
        let id = outputPatchIDs[i]
        // A refusal (the engine's own check of the patch) shows the cues' patches again.
        sendEach(key: "patch") { cue in
            guard cue.kindType == "audio", id.raw != (cue.raw["kind"]?["patch"]?.stringValue ?? "") else { return nil }
            return .setProperty(cue.id, .kindFields(["patch": .string(id.raw)]))
        }
    }

    /// A stage (or none) chosen for the shown Video cues; none leaves them broken until a stage is
    /// chosen (QLab 5 p. 212: a cue on no stage does not play).
    @objc func stageChosen() {
        let i = videoIO.stagePopup.indexOfSelectedItem
        guard videoIO.stageIDs.indices.contains(i), i != videoIO.stageIDs.count - 2 else { return }
        let stage = videoIO.stageIDs[i]
        sendEach(key: "stage") { cue in
            guard cue.kindType == "video", cue.raw["kind"]?["stage"]?.stringValue != stage?.raw else { return nil }
            return .setProperty(cue.id, .stage(stage))
        }
    }
}
