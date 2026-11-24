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
    /// Beside the path while the file is missing or unusable: the note the Basics tab shows.
    let fileProblem = NSTextField(labelWithString: "")
    /// The file's clear and show-in-Finder buttons, the Basics tab's (glyph only). The file is chosen
    /// by a double-click on the path.
    let clearButton = NSButton(title: "", target: nil, action: nil)
    let revealButton = NSButton(title: "", target: nil, action: nil)
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
        fileProblem.isHidden = true
        filePath.showNote(loc("inspector.mixed"))
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
        filePath.configure(id: "inspector-io-file", label: loc("inspector.io.file")) {}
        fileProblem.setAX("inspector-io-file-problem")
        fileProblem.font = Self.valueFont
        fileProblem.textColor = .systemOrange
        fileProblem.lineBreakMode = .byTruncatingTail
        fileProblem.setContentHuggingPriority(.required, for: .horizontal)
        fileProblem.setContentCompressionResistancePriority(.defaultHigh, for: .horizontal)
        fileProblem.isHidden = true
        clearButton.setAX("inspector-io-clear", label: loc("inspector.target.clear"))
        clearButton.isBordered = false
        clearButton.image = Icon.fieldClear.image(loc("inspector.target.clear"), compact: true)
        clearButton.toolTip = loc("inspector.target.clear")
        clearButton.setContentHuggingPriority(.required, for: .horizontal)
        clearButton.setContentCompressionResistancePriority(.required, for: .horizontal)
        divider.boxType = .separator
        divider.setAX("inspector-io-divider")
        outputFormatLabel.font = Self.valueFont
        outputFormatLabel.setAX("inspector-io-output-format", label: loc("inspector.io.outputFormat"))
        outputFormatLabel.lineBreakMode = .byTruncatingTail
        outputFormatLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        outputFormatLabel.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        formatLabel.font = Self.valueFont
        formatLabel.setAX("inspector-io-format", label: loc("inspector.io.format"))
        formatLabel.lineBreakMode = .byTruncatingTail
        formatLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        formatLabel.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        stagePopup.setAX("inspector-io-stage", label: loc("inspector.io.stage"))
        Self.makeSmall(stagePopup)
        Self.makeSmall(trackPopup)
        Self.makeSmall(stageSizeLabel)
        stageSizeLabel.setAX("inspector-io-stage-size", label: loc("inspector.io.stageSize"))
        stageSizeLabel.textColor = .secondaryLabelColor
        monitorButton.setAX("inspector-io-monitor", label: loc("inspector.io.monitor.ax"))
        Self.makeSmall(monitorButton)
        monitorButton.toolTip = loc("inspector.io.monitor.tip")
        monitorButton.setContentCompressionResistancePriority(.required, for: .horizontal)
        soundLabel.font = Self.valueFont
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

    /// The small size every control and value of the tab is drawn in, as the reference's rows are.
    static let valueFont = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize)

    /// Draws a control in the tab's small size.
    static func makeSmall(_ control: NSControl) {
        control.controlSize = .small
        control.font = valueFont
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

    /// The patch's output format line: the output count of the device the patch plays on and the
    /// rate it runs at; either alone while the other is not known.
    nonisolated static func outputFormatText(outputs: Int?, sampleRate: UInt32?) -> String {
        let count = outputs.map { n in
            n == 1 ? loc("inspector.io.outputs.one") : String(format: loc("inspector.io.outputs.many"), n)
        }
        guard let sampleRate, sampleRate > 0 else { return count ?? "" }
        let rate = "\(kHzText(Int(sampleRate))) kHz"
        return count.map { String(format: loc("inspector.io.outputFormatText"), $0, kHzText(Int(sampleRate))) } ?? rate
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
        // The patch pop-up takes the row's spare width, with Edit at the far right.
        outputPatchPopup.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        outputPatchPopup.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        VideoIOControls.makeSmall(outputPatchPopup)
        VideoIOControls.makeSmall(editPatchButton)
        editPatchButton.setContentHuggingPriority(.required, for: .horizontal)
        editPatchButton.setContentCompressionResistancePriority(.required, for: .horizontal)
        editPatchButton.setAX(AX.inspectorEditPatch, label: loc("inspector.io.editPatch"))
        editPatchButton.target = self
        editPatchButton.action = #selector(editPatchClicked)
        let v = videoIO
        v.stagePopup.target = self
        v.stagePopup.action = #selector(stageChosen)
        v.stagePopup.widthAnchor.constraint(greaterThanOrEqualToConstant: 200).isActive = true
        v.monitorButton.target = self
        v.monitorButton.action = #selector(monitorClicked)
        v.trackPopup.target = self
        v.trackPopup.action = #selector(audioTrackChosen)
        func label(_ key: String) -> NSTextField {
            let l = NSTextField(labelWithString: loc(key))
            l.font = VideoIOControls.valueFont
            l.textColor = .secondaryLabelColor
            l.alignment = .right
            l.setContentHuggingPriority(.required, for: .horizontal)
            return l
        }
        func line(_ views: [NSView]) -> NSStackView {
            let s = NSStackView(views: views)
            s.spacing = 8
            s.alignment = .centerY
            // A row takes the tab's width; its first control stretches (the path, the patch pop-up).
            s.setHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
            return s
        }
        v.clearButton.target = self
        v.clearButton.action = #selector(clearTargetClicked)
        configureRevealButton(v.revealButton, id: "inspector-io-reveal")
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
            // Every row's control takes the tab's width: the path, and the patch pop-up up to Edit.
            g.column(at: 1).xPlacement = .fill
            return g
        }
        // The file, then its format: the Basics tab's path field with its clear and show-in-Finder buttons.
        let fileGrid = grid([
            [labels[0], line([v.filePath.box, v.fileProblem, v.clearButton, v.revealButton])],
            [labels[1], v.formatLabel],
            [labels[2], line([v.soundLabel, v.trackPopup, v.trackNote])],
            [labels[3], line([v.stagePopup, v.monitorButton, v.stageSizeLabel])],
        ])
        v.videoRows = [fileGrid.row(at: 1), fileGrid.row(at: 3)]
        v.soundRow = fileGrid.row(at: 2)
        let patchLine = line([outputPatchPopup, editPatchButton])
        patchLine.distribution = .fill
        // Under the rule: the patch with Edit, and the patch's output format.
        let outputGrid = grid([
            [labels[4], patchLine],
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
        let hasFile = !(cue.raw["kind"]?["media"]?.stringValue ?? "").isEmpty
        // A format row with no file has nothing to say, so it is left out.
        videoIO.videoRows.first?.isHidden = !video || !hasFile
        videoIO.videoRows.last?.isHidden = !video
        // An Audio cue's sound format is its file's; a Video cue's is its movie's, when it has sound.
        videoIO.soundRow?.isHidden = video ? !sounds : !hasFile
        videoIO.soundTitle.stringValue = loc(video ? "inspector.io.soundFormat" : "inspector.io.fileFormat")
        videoIO.showOutputSection(sounds)
        let patches = model.patches
        let names = patches.map { p -> String in
            let name = p.raw["name"]?.stringValue ?? ""
            return name.isEmpty ? loc("settings.audio.untitledPatch") : name
        }
        let ids = patches.map(\.id)
        if ids != outputPatchIDs || names != outputPatchNames || video != outputPatchHasNone {
            if ids != outputPatchIDs { readPatchDevices() }
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
        renderFile(cue, model: model)
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
    /// same items, from the home folder down), with its clear and show-in-Finder buttons; "No file" in its place
    /// while the cue has none.
    private func renderFile(_ cue: Cue, model: WorkspaceModel? = nil) {
        let v = videoIO
        let media = cue.raw["kind"]?["media"]?.stringValue ?? ""
        let url = media.isEmpty ? nil : CueRowMenu.mediaURL(media, showPath: store.document?.path)
        if url == nil { v.filePath.showNote(loc("inspector.io.noFile")) } else { v.filePath.show(url: url) }
        let problem = (store.problems[cue.id] ?? []).first {
            $0.type == "media_missing" || $0.type == "media_unsupported"
        }
        v.fileProblem.isHidden = problem == nil
        if let problem {
            v.fileProblem.stringValue = loc(
                problem.type == "media_missing" ? "inspector.target.fileMissing" : "inspector.target.fileUnsupported")
            v.fileProblem.toolTip = model.map { ProblemText.problem(problem, label: WarningRows.labeler($0)) }
        }
        v.clearButton.isEnabled = !isLocked && !isBatch && !media.isEmpty
        v.revealButton.isEnabled = url != nil && !isBatch
    }

    /// The output-format line under the patch (QLab 5 manual p. 177): the channels and sample rate of the
    /// audio device the patch the shown cues share plays on, on this Mac; "not connected" for a device
    /// that is missing or lost, "no device" for a patch with none here; empty for no patch, several, or one the show lacks, and before the engine has said.
    func renderOutputFormat(_ cues: [Cue], model: WorkspaceModel) {
        let label = videoIO.outputFormatLabel
        label.stringValue = ""
        label.textColor = .labelColor
        // A line with nothing to say leaves its row out, as the rule's section does with no sound.
        defer { videoIO.outputFormatRow?.isHidden = label.stringValue.isEmpty || videoIO.divider.isHidden }
        guard let id = Self.sharedPatch(of: cues, model: model), model.patches.contains(where: { $0.id == id }),
            let patchDevices
        else { return }
        let device = patchDevices[id]
        let open = device.flatMap { name in openOutputs.first { $0.name == name } }
        // A device the engine has lost (or failed to open), or one the patch names that is not here.
        if missingPatches.contains(id) || (open.map { $0.state == .lost || $0.state == .failed } ?? false) {
            label.stringValue = loc("inspector.io.outputFormatNotConnected")
            label.textColor = .secondaryLabelColor
            return
        }
        let channels =
            open.flatMap { $0.channels > 0 ? Int($0.channels) : nil } ?? device.flatMap { deviceChannels[$0] }
        let rate = open.flatMap { $0.sampleRate > 0 ? $0.sampleRate : nil }
        if channels == nil, rate == nil {
            label.stringValue = loc("inspector.io.outputFormatNoDevice")
            label.textColor = .secondaryLabelColor
        } else {
            label.stringValue = VideoIOControls.outputFormatText(outputs: channels, sampleRate: rate)
        }
    }

    /// Reads where each patch plays on this Mac (and each device's channels) from the engine; one read at
    /// a time, a request during it read again after. The line shows what it finds.
    func readPatchDevices() {
        guard patchReading == nil else {
            patchReadAgain = true
            return
        }
        patchReading = Task { [weak self] in
            repeat {
                guard let self else { return }
                self.patchReadAgain = false
                async let mappings = self.store.listPatchMappings()
                async let devices = self.store.listDevices()
                if case .success(let entries) = await mappings {
                    var list: [DeviceEntry] = []
                    if case .success(let ok) = await devices, case .devices(let found, _)? = ok.data { list = found }
                    self.applyPatchMappings(entries, devices: list)
                }
            } while self?.patchReadAgain == true
            self?.patchReading = nil
        }
    }

    /// Takes the engine's answer: a patch is on the device its resolution names, unless that is not
    /// here (unmapped, or its device is missing).
    func applyPatchMappings(_ entries: [PatchMappingEntry], devices: [DeviceEntry]) {
        var found: [ModelID: String] = [:]
        var missing: Set<ModelID> = []
        for e in entries {
            // A device the patch names that is not on this Mac's list, or is listed lost, is not connected;
            // one listed and only not open yet is the device (its channels come from the list).
            if let r = e.resolution, r.type == "device_missing", let name = r.json["device"]?.stringValue {
                let listed = devices.first { $0.name == name }
                if let listed, listed.state != .lost, listed.state != .failed {
                    found[e.patch] = name
                } else {
                    missing.insert(e.patch)
                }
                continue
            }
            guard let r = e.resolution, r.type != "unmapped", r.type != "device_missing",
                let name = r.json["device"]?.stringValue, !name.isEmpty
            else { continue }
            found[e.patch] = name
        }
        let channels = Dictionary(devices.map { ($0.name, $0.outputChannels) }, uniquingKeysWith: { a, _ in a })
        guard found != patchDevices || channels != deviceChannels || missing != missingPatches else { return }
        missingPatches = missing
        patchDevices = found
        deviceChannels = channels
        redrawOutputFormat()
    }

    /// Draws the output-format line again for the shown cues; nothing else on screen is redrawn, so
    /// a message the inspector is showing stays.
    func redrawOutputFormat() {
        guard isViewLoaded, let model = store.model else { return }
        renderOutputFormat(editCues, model: model)
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
