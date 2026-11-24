// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The I/O tab's Video cue rows (QLab 5, manual pp. 211-212): the file, the format its probe read,
/// the format of its sound (a pop-up of the audio tracks when the movie has several), the stage
/// pop-up (with "none"), a button opening the stage's monitor window and the stage's pixel size.
/// Held by the inspector. An Audio cue's I/O tab uses its file and sound-format rows and the patch.
@MainActor
final class VideoIOControls {
    /// The file as the Basics tab draws it: folders and name with their icons, from the home folder down.
    let filePath = TargetPathControl()
    /// Stands where `filePath` is hidden: no file, or several cues whose files differ.
    let fileLabel = NSTextField(labelWithString: "")
    /// The file's clear and choose buttons, the Basics tab's (glyph only).
    let clearButton = NSButton(title: "", target: nil, action: nil)
    let chooseButton = NSButton(title: loc("inspector.io.choose"), target: nil, action: nil)
    /// The title of the sound-format row: "Format" for an Audio cue (the file's), "Sound format" for a movie's.
    let soundTitle = NSTextField(labelWithString: "")
    /// The full-width rule between the file's rows and the output's.
    let divider = NSBox()
    /// The patch's output format under the patch row: its output count and the sample rate.
    let outputFormatLabel = NSTextField(labelWithString: "")
    var outputFormatRow: NSGridRow?
    /// The grid holding the patch and output-format rows (hidden with the divider for a cue with no sound).
    var outputSection: NSView?
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

    /// Several cues whose files differ: "mixed" where the path would be.
    func showMixedFile() {
        filePath.isHidden = true
        fileLabel.isHidden = false
        fileLabel.stringValue = loc("inspector.mixed")
        fileLabel.textColor = .secondaryLabelColor
        fileLabel.toolTip = nil
    }

    /// Shows or hides everything under the divider: the patch, its output format and the rule itself.
    func showOutputSection(_ shown: Bool) {
        patchRow?.isHidden = !shown
        outputFormatRow?.isHidden = !shown
        outputSection?.isHidden = !shown
        divider.isHidden = !shown
    }
    /// Item i of the stage pop-up is `stageIDs[i]`'s stage; nil is the separator or "none".
    var stageIDs: [ModelID?] = []
    var stageNames: [String] = []

    init() {
        filePath.setAX("inspector-io-file", label: loc("inspector.io.file"))
        filePath.pathStyle = .standard
        filePath.isEditable = false
        filePath.controlSize = .small
        filePath.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        filePath.isHidden = true
        // A long path gives up its middle folders; it must not hold the inspector, and so the window, wider.
        filePath.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        filePath.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        InspectorViewController.prefersWidth(filePath)
        fileLabel.setAX("inspector-io-file-note")
        fileLabel.lineBreakMode = .byTruncatingTail
        fileLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        fileLabel.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        clearButton.setAX("inspector-io-clear", label: loc("inspector.target.clear"))
        clearButton.isBordered = false
        clearButton.image = Icon.fieldClear.image(loc("inspector.target.clear"), compact: true)
        clearButton.toolTip = loc("inspector.target.clear")
        clearButton.setContentHuggingPriority(.required, for: .horizontal)
        clearButton.setContentCompressionResistancePriority(.required, for: .horizontal)
        chooseButton.setAX("inspector-io-choose", label: loc("inspector.target.choose"))
        chooseButton.image = Icon.fieldChoose.image(loc("inspector.target.choose"), compact: true)
        chooseButton.imagePosition = .imageOnly
        chooseButton.isBordered = false
        chooseButton.toolTip = loc("inspector.target.choose")
        chooseButton.setContentCompressionResistancePriority(.required, for: .horizontal)
        divider.boxType = .separator
        divider.setAX("inspector-io-divider")
        outputFormatLabel.setAX("inspector-io-output-format", label: loc("inspector.io.outputFormat"))
        outputFormatLabel.lineBreakMode = .byTruncatingTail
        outputFormatLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
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

    /// The patch's output format line: how many outputs it has and the rate the engine mixes at
    /// (the clock device's); the count alone while no rate is known.
    nonisolated static func outputFormatText(outputs: Int, sampleRate: UInt32?) -> String {
        let count =
            outputs == 1
            ? loc("inspector.io.outputs.one") : String(format: loc("inspector.io.outputs.many"), outputs)
        guard let sampleRate, sampleRate > 0 else { return count }
        return String(format: loc("inspector.io.outputFormatText"), count, kHzText(Int(sampleRate)))
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
        v.clearButton.target = self
        v.clearButton.action = #selector(clearTargetClicked)
        v.filePath.onDoubleClick = { [weak self] in self?.chooseClicked() }
        v.soundTitle.stringValue = loc("inspector.io.soundFormat")
        v.soundTitle.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        v.soundTitle.textColor = .secondaryLabelColor
        v.soundTitle.alignment = .right
        let labels = [
            label("inspector.io.file"), label("inspector.io.format"), v.soundTitle, label("inspector.io.stage"),
            label("inspector.io.patch"), label("inspector.io.outputFormat"),
        ]
        // Both grids share one label column width, so the labels and the controls line up across the rule.
        let labelWidth = labels.map { ceil($0.fittingSize.width) }.max() ?? 0
        func grid(_ rows: [[NSView]]) -> NSGridView {
            let g = NSGridView(views: rows)
            g.rowSpacing = 8
            g.columnSpacing = 8
            g.rowAlignment = .firstBaseline
            g.column(at: 0).xPlacement = .trailing
            // The label column is as wide as its longest label; the grid would otherwise give it the
            // spare width.
            g.column(at: 0).width = labelWidth
            return g
        }
        // The file, then its format: the Basics tab's path control with its clear and choose buttons.
        let fileGrid = grid([
            [labels[0], line([v.filePath, v.fileLabel, v.clearButton, v.chooseButton])],
            [labels[1], v.formatLabel],
            [labels[2], line([v.soundLabel, v.trackPopup, v.trackNote])],
            [labels[3], line([v.stagePopup, v.monitorButton, v.stageSizeLabel])],
        ])
        // The file row takes the tab's width, so a long path shows as much as fits.
        fileGrid.cell(atColumnIndex: 1, rowIndex: 0).xPlacement = .fill
        v.videoRows = [fileGrid.row(at: 1), fileGrid.row(at: 3)]
        v.soundRow = fileGrid.row(at: 2)
        // Under the rule: the patch with Edit, and the patch's output format.
        let outputGrid = grid([
            [labels[4], line([outputPatchPopup, editPatchButton])],
            [labels[5], v.outputFormatLabel],
        ])
        v.patchRow = outputGrid.row(at: 0)
        v.outputFormatRow = outputGrid.row(at: 1)
        // The grids sit 12 pt in from the tab's sides; the rule runs edge to edge.
        func padded(_ g: NSGridView) -> NSView {
            let holder = NSView()
            g.translatesAutoresizingMaskIntoConstraints = false
            holder.addSubview(g)
            NSLayoutConstraint.activate([
                g.topAnchor.constraint(equalTo: holder.topAnchor),
                g.bottomAnchor.constraint(equalTo: holder.bottomAnchor),
                g.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: 12),
                g.trailingAnchor.constraint(equalTo: holder.trailingAnchor, constant: -12),
            ])
            return holder
        }
        let top = padded(fileGrid)
        let bottom = padded(outputGrid)
        v.outputSection = bottom
        let stack = NSStackView(views: [top, v.divider, bottom])
        stack.orientation = .vertical
        stack.alignment = .width
        stack.spacing = 12
        stack.setHuggingPriority(.defaultLow, for: .horizontal)
        // The stack fills the tab's width: a long path takes the room there is, then gives up its middle.
        let content = NSView()
        stack.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: content.topAnchor, constant: 8),
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            stack.bottomAnchor.constraint(lessThanOrEqualTo: content.bottomAnchor, constant: -8),
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
        videoIO.soundTitle.stringValue = loc(video ? "inspector.io.soundFormat" : "inspector.io.fileFormat")
        videoIO.showOutputSection(sounds)
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
        renderOutputFormat([cue], model: model)
        // A patch that is gone or none is not one to edit (several cues: `renderBatchSound`).
        editPatchButton.isEnabled = !isLocked && outputPatchIDs.indices.contains(chosen) && !isBatch
    }

    /// What each item of the output-patch pop-up stands for, in item order: a patch's id, a value no
    /// cue has for the separator, and "" for none (a Video cue's pop-up only).
    var outputPatchChoices: [String] {
        outputPatchIDs.map(\.raw) + (outputPatchHasNone ? ["\u{2}", ""] : [])
    }

    /// The file row of an Audio or Video cue: the folder-and-file path control the Basics tab draws (the
    /// same items, from the home folder down), with its clear and choose buttons; "No file" in its place
    /// while the cue has none.
    private func renderFile(_ cue: Cue) {
        let v = videoIO
        let media = cue.raw["kind"]?["media"]?.stringValue ?? ""
        let url = media.isEmpty ? nil : CueRowMenu.mediaURL(media, showPath: store.document?.path)
        v.filePath.isHidden = url == nil
        v.fileLabel.isHidden = url != nil
        if let url, v.filePath.url != url {
            v.filePath.url = url
            v.filePath.setFullItems(Self.shownPathItems(v.filePath.pathItems, of: url))
        } else if url == nil {
            v.filePath.url = nil
        }
        v.filePath.toolTip = url?.path
        v.fileLabel.stringValue = loc("inspector.io.noFile")
        v.fileLabel.textColor = .secondaryLabelColor
        v.fileLabel.toolTip = nil
        v.clearButton.isEnabled = !isLocked && !isBatch && !media.isEmpty
        v.chooseButton.isEnabled = !isLocked && !isBatch
    }

    /// The output-format line under the patch: the output count of the patch the shown cues share
    /// and the sample rate the engine mixes at; empty for no patch, several, or one the show lacks.
    func renderOutputFormat(_ cues: [Cue], model: WorkspaceModel) {
        let patch = Self.sharedPatch(of: cues, model: model).flatMap { id in model.patches.first { $0.id == id } }
        let outputs = patch?.raw["outputs"]?.arrayValue?.count
        videoIO.outputFormatLabel.stringValue =
            outputs.map { VideoIOControls.outputFormatText(outputs: $0, sampleRate: clockRate) } ?? ""
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
