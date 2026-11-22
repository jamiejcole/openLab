// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The I/O tab's Video cue rows (QLab 5, manual pp. 211-212): the file, the format its probe read,
/// the format of its sound (a pop-up of the audio tracks when the movie has several), the stage
/// pop-up (with "none"), a button opening the stage's monitor window and the stage's pixel size.
/// Held by the inspector. An Audio cue's I/O tab uses its file and sound-format rows and the patch.
@MainActor
final class VideoIOControls {
    let fileLabel = NSTextField(labelWithString: "")
    let chooseButton = NSButton(title: loc("inspector.io.choose"), target: nil, action: nil)
    let formatLabel = NSTextField(labelWithString: "")
    let stagePopup = NSPopUpButton()
    let stageSizeLabel = NSTextField(labelWithString: "")
    /// Opens the cue's stage's monitor window (QLab 5 manual p. 157); off with no stage.
    let monitorButton = NSButton(title: loc("inspector.io.monitor"), target: nil, action: nil)
    /// The sound's format: plain text for a movie with one audio track, the pop-up for one with several.
    let soundLabel = NSTextField(labelWithString: "")
    let trackPopup = NSPopUpButton()
    /// Beside the pop-up while the stored track is not in the file: which track plays instead.
    let trackNote = NSTextField(labelWithString: loc("inspector.io.trackMissingNote"))
    /// The titles of the track pop-up's items, as last built.
    var trackTitles: [String] = []
    /// The rows only a Video cue has: its picture format and stage (the grid's rows; hidden for an Audio cue).
    var videoRows: [NSGridRow] = []
    /// The sound-format row: shown only for a Video cue whose movie has sound.
    var soundRow: NSGridRow?
    /// The output patch's row: an Audio cue's, and a Video cue's whose movie has sound.
    var patchRow: NSGridRow?
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
        monitorButton.setAX("inspector-io-monitor", label: loc("inspector.io.monitor.ax"))
        monitorButton.controlSize = .small
        monitorButton.toolTip = loc("inspector.io.monitor.tip")
        monitorButton.setContentCompressionResistancePriority(.required, for: .horizontal)
        soundLabel.setAX("inspector-io-sound-format", label: loc("inspector.io.soundFormat"))
        soundLabel.lineBreakMode = .byTruncatingTail
        soundLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        trackPopup.setAX("inspector-io-audio-track", label: loc("inspector.io.soundFormat"))
        trackPopup.isHidden = true
        trackNote.setAX("inspector-io-audio-track-note")
        trackNote.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        trackNote.textColor = .secondaryLabelColor
        trackNote.lineBreakMode = .byTruncatingTail
        trackNote.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        trackNote.isHidden = true
    }

    /// A frame rate to three decimals, trailing zeros dropped: 25, 29.97, 23.976, 59.94.
    nonisolated static func fpsText(_ fps: Double) -> String {
        guard fps.isFinite, fps > 0 else { return "?" }
        var text = String(format: "%.3f", fps)
        while text.hasSuffix("0") { text.removeLast() }
        if text.hasSuffix(".") { text.removeLast() }
        return text
    }

    /// A sample rate in kHz: 48, 44.1, 88.2.
    nonisolated static func kHzText(_ rate: Int) -> String { fpsText(Double(rate) / 1000) }

    /// What the sound-format row says for one audio track: its codec, channel count and sample rate.
    /// A codec this build has no name for shows its four-character code.
    nonisolated static func soundText(_ t: AudioTrackFormat) -> String {
        soundText(codec: t.codec, channels: t.channels, sampleRate: t.sampleRate)
    }

    /// The same for an Audio cue's file, as the engine's probe read it.
    nonisolated static func soundText(_ f: AudioFileFormat) -> String {
        soundText(codec: f.codec, channels: f.channels, sampleRate: f.sampleRate)
    }

    private nonisolated static func soundText(codec: String, channels: Int, sampleRate: Int) -> String {
        let name = ProblemText.text(for: "inspector.io.audioCodec.\(codec)") ?? codec
        let count =
            channels == 1
            ? loc("inspector.io.channels.one") : String(format: loc("inspector.io.channels.many"), channels)
        return String(format: loc("inspector.io.soundFormatText"), name, count, kHzText(sampleRate))
    }

    /// What the format row says for a probed file: a movie's codec, pixel size and frame rate, or
    /// a still's format and pixel size.
    nonisolated static func formatText(_ p: PictureFormat) -> String {
        let size = String(format: loc("inspector.io.pixels"), p.width, p.height)
        if p.type == "still" {
            let name = p.format.map { loc("inspector.io.still.\($0)") } ?? ""
            return String(format: loc("inspector.io.stillFormat"), name, size)
        }
        let codec = p.codec.map { loc("inspector.io.codec.\($0)") } ?? ""
        let fps = p.fps.map(fpsText) ?? "?"
        return String(format: loc("inspector.io.videoFormat"), codec, size, fps)
    }
}

extension InspectorViewController {
    /// The I/O tab (QLab 5 has the same rows): for a Video cue its file, format and stage, and when
    /// its movie has sound the sound's format and the output patch; for an Audio cue its file, the
    /// file's format and the output patch.
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
        v.monitorButton.target = self
        v.monitorButton.action = #selector(monitorClicked)
        v.trackPopup.target = self
        v.trackPopup.action = #selector(audioTrackChosen)
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
        let labels = [
            "inspector.io.file", "inspector.io.format", "inspector.io.soundFormat", "inspector.io.stage",
            "inspector.io.patch",
        ].map(label)
        let grid = NSGridView(views: [
            [labels[0], line([v.fileLabel, v.chooseButton])],
            [labels[1], v.formatLabel],
            [labels[2], line([v.soundLabel, v.trackPopup, v.trackNote])],
            [labels[3], line([v.stagePopup, v.monitorButton, v.stageSizeLabel])],
            [labels[4], line([outputPatchPopup, editPatchButton])],
        ])
        grid.rowSpacing = 8
        grid.columnSpacing = 8
        grid.rowAlignment = .firstBaseline
        grid.column(at: 0).xPlacement = .trailing
        // The label column is as wide as its longest label; the grid would otherwise give it the
        // spare width.
        grid.column(at: 0).width = labels.map { ceil($0.fittingSize.width) }.max() ?? 0
        // The file row takes the tab's width, so a long path shows as much as fits.
        grid.cell(atColumnIndex: 1, rowIndex: 0).xPlacement = .fill
        v.videoRows = [grid.row(at: 1), grid.row(at: 3)]
        v.soundRow = grid.row(at: 2)
        v.patchRow = grid.row(at: 4)
        // The grid fills the tab's width: a long path takes the room there is, then gives up its middle.
        let content = NSView()
        grid.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(grid)
        NSLayoutConstraint.activate([
            grid.topAnchor.constraint(equalTo: content.topAnchor, constant: 8),
            grid.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 12),
            grid.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -12),
            grid.bottomAnchor.constraint(lessThanOrEqualTo: content.bottomAnchor, constant: -8),
        ])
        let item = NSTabViewItem(identifier: "io")
        item.label = loc("inspector.tab.io")
        item.view = content
        return item
    }

    /// The pop-up follows the model: every output patch by name, and for a Video cue a separator and
    /// "none" after them (its sound then has nowhere to play and the cue shows broken; an Audio cue
    /// always has a patch). Rebuilt only when the list changes, so an open menu is not disturbed. A
    /// cue whose patch is gone shows no choice.
    func renderIO(_ cue: Cue, model: WorkspaceModel) {
        guard cue.takesFile else { return }
        let video = cue.kindType == "video"
        let sounds = cue.sounds(store.mediaDurations[cue.id])
        for r in videoIO.videoRows { r.isHidden = !video }
        // An Audio cue's sound format is its file's; a Video cue's is its movie's, when it has sound.
        videoIO.soundRow?.isHidden = video && !sounds
        videoIO.patchRow?.isHidden = !sounds
        let patches = model.patches
        let names = patches.map { p -> String in
            let name = p.raw["name"]?.stringValue ?? ""
            return name.isEmpty ? loc("settings.audio.untitledPatch") : name
        }
        let ids = patches.map(\.id)
        if ids != outputPatchIDs || names != outputPatchNames || video != outputPatchHasNone {
            outputPatchIDs = ids
            outputPatchNames = names
            outputPatchHasNone = video
            outputPatchPopup.menu?.removeAllItems()
            for name in names {
                outputPatchPopup.menu?.addItem(NSMenuItem(title: name, action: nil, keyEquivalent: ""))
            }
            if video {
                outputPatchPopup.menu?.addItem(.separator())
                outputPatchPopup.menu?.addItem(
                    NSMenuItem(title: loc("inspector.io.patchNone"), action: nil, keyEquivalent: ""))
            }
        }
        renderFile(cue)
        if video { renderVideoIO(cue, model: model) } else { renderAudioFormat(cue) }
        let current = cue.raw["kind"]?["patch"]?.stringValue
        let chosen = outputPatchChoices.firstIndex(of: current ?? "") ?? -1
        if outputPatchPopup.indexOfSelectedItem != chosen { outputPatchPopup.selectItem(at: chosen) }
        outputPatchPopup.isEnabled = !isLocked
        // A patch that is gone or none is not one to edit (several cues: `renderBatchSound`).
        editPatchButton.isEnabled = !isLocked && outputPatchIDs.indices.contains(chosen) && !isBatch
    }

    /// What each item of the output-patch pop-up stands for, in item order: a patch's id, a value no
    /// cue has for the separator, and "" for none (a Video cue's pop-up only).
    var outputPatchChoices: [String] {
        outputPatchIDs.map(\.raw) + (outputPatchHasNone ? ["\u{2}", ""] : [])
    }

    /// The file row of an Audio or Video cue: its path as the Basics tab shows it, and Choose.
    private func renderFile(_ cue: Cue) {
        let v = videoIO
        let media = cue.raw["kind"]?["media"]?.stringValue ?? ""
        let path = CueRowMenu.mediaURL(media, showPath: store.document?.path)?.path ?? media
        v.fileLabel.stringValue = media.isEmpty ? loc("inspector.io.noFile") : path
        v.fileLabel.textColor = media.isEmpty ? .secondaryLabelColor : .labelColor
        v.fileLabel.toolTip = media.isEmpty ? nil : path
        v.chooseButton.isEnabled = !isLocked && !isBatch
    }

    /// An Audio cue's sound-format row: the codec, channels and rate the engine's probe read, or
    /// that the file is not read (missing, damaged or not probed yet).
    private func renderAudioFormat(_ cue: Cue) {
        let v = videoIO
        let media = cue.raw["kind"]?["media"]?.stringValue ?? ""
        let format = store.mediaDurations[cue.id]?.audioFile
        v.soundLabel.isHidden = false
        v.trackPopup.isHidden = true
        v.trackNote.isHidden = true
        v.soundLabel.textColor = format == nil ? .secondaryLabelColor : .labelColor
        v.soundLabel.stringValue =
            format.map(VideoIOControls.soundText) ?? (media.isEmpty ? "" : loc("inspector.io.formatUnknown"))
    }

    /// A Video cue's rows: the format the engine's probe read, the sound's format (the
    /// track pop-up when the movie has several audio tracks) and its stage.
    private func renderVideoIO(_ cue: Cue, model: WorkspaceModel) {
        let v = videoIO
        let media = cue.raw["kind"]?["media"]?.stringValue ?? ""
        let picture = store.mediaDurations[cue.id]?.picture
        v.formatLabel.textColor = picture == nil ? .secondaryLabelColor : .labelColor
        v.formatLabel.stringValue =
            picture.map(VideoIOControls.formatText) ?? (media.isEmpty ? "" : loc("inspector.io.formatUnknown"))
        renderSoundFormat(cue)
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
        // Watching is no edit: a locked cue's stage can be watched too.
        v.monitorButton.isEnabled = monitoredStage != nil && store.videoOutputs.video != false
    }

    /// The stage the I/O tab's monitor button opens: the one every shown Video cue is on, if it is
    /// in the workspace.
    var monitoredStage: ModelID? {
        let stages = Set(editCues.filter { $0.kindType == "video" }.map { $0.raw["kind"]?["stage"]?.stringValue })
        guard stages.count == 1, let raw = stages.first ?? nil else { return nil }
        return store.model?.stages.first { $0.id.raw == raw }?.id
    }

    /// Opens the shown cue's stage's monitor window, or brings it to the front; a refusal says why
    /// in the inspector.
    @objc func monitorClicked() {
        guard let stage = monitoredStage else { return }
        messageLabel.stringValue = ""
        Task { [weak self, store] in
            if case .failure(let e) = await store.setStageMonitor(stage, open: true) {
                self?.messageLabel.stringValue = e.detailText
            }
        }
    }

    /// The sound-format row (QLab 5 manual p. 211): one track is plain text; several are a pop-up,
    /// one numbered item per track, on the track the cue stores. A stored track the file does not
    /// have (the engine plays the first and lists a warning) is the pop-up's last item, named as
    /// missing and chosen, with a line beside it saying the first track plays; picking a track of
    /// the file puts it right. That holds for a file with one track too.
    private func renderSoundFormat(_ cue: Cue) {
        let v = videoIO
        let tracks = store.mediaDurations[cue.id]?.audioTracks ?? []
        let stored = Int(cue.raw["kind"]?["audio_track"]?.doubleValue ?? 0)
        let known = tracks.indices.contains(stored)
        let several = tracks.count > 1 || (!tracks.isEmpty && !known)
        v.soundLabel.isHidden = several
        v.trackPopup.isHidden = !several
        v.soundLabel.stringValue = tracks.first.map(VideoIOControls.soundText) ?? ""
        v.soundLabel.textColor = .labelColor
        var titles = tracks.enumerated().map { n, t in
            String(format: loc("inspector.io.trackItem"), n + 1, VideoIOControls.soundText(t))
        }
        let missing = !tracks.isEmpty && !known
        if missing { titles.append(String(format: loc("inspector.io.trackMissing"), stored + 1)) }
        v.trackNote.isHidden = !missing
        if titles != v.trackTitles {
            v.trackTitles = titles
            v.trackPopup.menu?.removeAllItems()
            for title in titles { v.trackPopup.menu?.addItem(NSMenuItem(title: title, action: nil, keyEquivalent: "")) }
        }
        let chosen = known ? stored : titles.count - 1
        if v.trackPopup.indexOfSelectedItem != chosen { v.trackPopup.selectItem(at: chosen) }
        v.trackPopup.isEnabled = !isLocked && !isBatch
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
        guard let model = store.model, let id = Self.sharedPatch(of: editCues, model: model) else { return }
        onEditPatch?(id)
    }

    /// A patch (or, for Video cues, none) chosen for the shown cues. A Video cue that had no patch
    /// and no levels of its own yet (made while the workspace had no patch) takes the patch's
    /// default levels with it, as a new cue would have: without them it would play silent.
    @objc func outputPatchChosen() {
        let i = outputPatchPopup.indexOfSelectedItem
        let choices = outputPatchChoices
        guard choices.indices.contains(i), choices[i] != "\u{2}" else { return }
        let patch = outputPatchIDs.indices.contains(i) ? outputPatchIDs[i] : nil
        let object = patch.flatMap { id in store.model?.patches.first { $0.id == id } }
        // A refusal (the engine's own check of the patch) shows the cues' patches again.
        sendEach(key: "patch") { cue in
            let kind = cue.raw["kind"]
            let video = cue.kindType == "video"
            guard cue.takesFile, patch != nil || video, patch?.raw != kind?["patch"]?.stringValue else { return nil }
            var fields: [String: JSONValue] = ["patch": patch.map { .string($0.raw) } ?? .null]
            if video, kind?["patch"]?.stringValue == nil, let object,
                let levels = kind.flatMap({ LevelSet(kind: $0) }), levels.outputs.isEmpty,
                levels.crosspoints.isEmpty, levels.unread.isEmpty
            {
                fields["levels"] = NewCueKind.defaultLevels(patch: object)
            }
            return .setProperty(cue.id, .kindFields(fields))
        }
    }

    /// An audio track chosen for the shown Video cue (stored as `audio_track`, counted from 0).
    @objc func audioTrackChosen() {
        let i = videoIO.trackPopup.indexOfSelectedItem
        // The item naming a missing track is not a track to pick.
        let tracks = cue.flatMap { store.mediaDurations[$0.id]?.audioTracks } ?? []
        guard tracks.indices.contains(i), !isBatch else { return render() }
        sendEach(key: "audioTrack") { cue in
            guard cue.kindType == "video", Int(cue.raw["kind"]?["audio_track"]?.doubleValue ?? 0) != i else {
                return nil
            }
            return .setProperty(cue.id, .kindFields(["audio_track": .int(Int64(i))]))
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
