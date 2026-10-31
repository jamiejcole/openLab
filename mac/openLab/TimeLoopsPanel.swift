// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The time and loops tab of an Audio cue (AUD-11, AUD-12, AUD-14; QLab surface
/// `inspector.audio.time-loops`): start and end trim fields and how many times the cue plays on
/// the left, the file's waveform with draggable trim handles filling the rest, zoom buttons
/// under it. Edits are reported as `Change`s; the inspector turns them into model commands.
/// Peaks come from the engine (`waveform` request) and are kept per file and zoom.
@MainActor
final class TimeLoopsPanel: NSObject, NSTextFieldDelegate {
    enum Change: Equatable {
        case start(Double)
        /// nil: the end of the file.
        case end(Double?)
        /// 0: loop until devamped (infinite).
        case playCount(Int)
    }

    /// Peaks asked for: one file, one stretch of it, at one width.
    struct PeaksKey: Hashable {
        let cue: ModelID
        let media: String
        let start: Double
        let end: Double
        let buckets: Int
    }

    static let cacheLimit = 16
    static let maxPlayCount = 1_000_000
    /// The narrowest stretch a zoom shows, seconds.
    static let minSpan = 0.1

    let view = NSStackView()
    let startField = NSTextField()
    let endField = NSTextField()
    let countField = NSTextField()
    let playRadio = NSButton(radioButtonWithTitle: loc("timeLoops.play"), target: nil, action: nil)
    let loopRadio = NSButton(radioButtonWithTitle: loc("timeLoops.loop"), target: nil, action: nil)
    let slicesNote = NSTextField(wrappingLabelWithString: loc("timeLoops.slicesNote"))
    let waveform = WaveformView()
    let zoomOutButton = NSButton()
    let zoomInButton = NSButton()

    var onChange: ((Change) -> Void)?
    /// A typed value was refused here (its `Localizable.strings` key); the field shows the model's again.
    var onInvalid: ((String) -> Void)?
    /// Fetches peaks (the store's `waveform`).
    var loadPeaks: ((ModelID, Double, Double, Int) async -> Result<WaveformPeaks, EngineError>)?
    var isLocked = false {
        didSet {
            applyLock()
            if !isLocked, oldValue { reload() }
        }
    }
    /// The tab is the one on screen. Peaks are fetched only then.
    var isActive = false {
        didSet { if isActive, !oldValue { reload() } }
    }

    private(set) var cache: [PeaksKey: WaveformPeaks] = [:]
    private var cacheOrder: [PeaksKey] = []
    /// The file shown: its cue, reference and length.
    private var file: (cue: ModelID, media: String, duration: Double)?
    private var kind: JSONValue = .null
    private var generation = 0
    /// The fetch under way (tests wait on it).
    private(set) var loading: Task<Void, Never>?

    override init() {
        super.init()
        startField.setAX(AX.timeLoopsStart)
        endField.setAX(AX.timeLoopsEnd)
        countField.setAX(AX.timeLoopsCount)
        playRadio.setAX(AX.timeLoopsPlay)
        loopRadio.setAX(AX.timeLoopsLoop)
        waveform.setAX(AX.timeLoopsWaveform)
        zoomOutButton.setAX(AX.timeLoopsZoomOut)
        zoomInButton.setAX(AX.timeLoopsZoomIn)
        for f in [startField, endField, countField] {
            f.delegate = self
            f.usesSingleLineMode = true
            f.alignment = .right
        }
        startField.widthAnchor.constraint(equalToConstant: 100).isActive = true
        endField.widthAnchor.constraint(equalToConstant: 100).isActive = true
        countField.widthAnchor.constraint(equalToConstant: 50).isActive = true
        for r in [playRadio, loopRadio] {
            r.target = self
            r.action = #selector(radioChanged(_:))
        }
        for (b, symbol, key, action) in [
            (zoomOutButton, "minus.magnifyingglass", "timeLoops.zoomOut", #selector(zoomOut)),
            (zoomInButton, "plus.magnifyingglass", "timeLoops.zoomIn", #selector(zoomIn)),
        ] {
            b.bezelStyle = .texturedRounded
            b.image = NSImage(systemSymbolName: symbol, accessibilityDescription: loc(key))
            b.toolTip = loc(key)
            b.target = self
            b.action = action
        }
        slicesNote.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        slicesNote.textColor = .secondaryLabelColor
        slicesNote.preferredMaxLayoutWidth = 170
        waveform.onTrimChanging = { [weak self] handle, t in
            guard let self else { return }
            (handle == .start ? startField : endField).stringValue = CueTime.format(t)
        }
        waveform.onTrimCommit = { [weak self] handle, t in self?.commitDrag(handle, t) }
        waveform.onResize = { [weak self] in self?.reload() }
        layout()
    }

    private func layout() {
        func row(_ title: String, _ control: NSView) -> NSView {
            let label = NSTextField(labelWithString: title)
            label.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            label.textColor = .secondaryLabelColor
            label.alignment = .right
            label.widthAnchor.constraint(equalToConstant: 60).isActive = true
            let s = NSStackView(views: [label, control])
            s.spacing = 6
            return s
        }
        let playRow = NSStackView(views: [playRadio, countField, NSTextField(labelWithString: loc("timeLoops.times"))])
        playRow.spacing = 6
        let left = NSStackView(views: [
            row(loc("timeLoops.start"), startField), row(loc("timeLoops.end"), endField), playRow, loopRadio,
            slicesNote,
        ])
        left.orientation = .vertical
        left.alignment = .leading
        left.spacing = 8
        left.widthAnchor.constraint(equalToConstant: 180).isActive = true
        let spacer = NSView()
        spacer.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let strip = NSStackView(views: [spacer, zoomOutButton, zoomInButton])
        strip.spacing = 4
        waveform.translatesAutoresizingMaskIntoConstraints = false
        waveform.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        waveform.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        waveform.heightAnchor.constraint(greaterThanOrEqualToConstant: 80).isActive = true
        let right = NSStackView(views: [waveform, strip])
        right.orientation = .vertical
        right.alignment = .leading
        right.spacing = 4
        waveform.widthAnchor.constraint(equalTo: right.widthAnchor).isActive = true
        strip.widthAnchor.constraint(equalTo: right.widthAnchor).isActive = true
        right.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        view.orientation = .horizontal
        view.alignment = .top
        view.spacing = 12
        view.addArrangedSubview(left)
        view.addArrangedSubview(right)
        right.heightAnchor.constraint(equalTo: view.heightAnchor).isActive = true
    }

    // MARK: Showing

    var startTime: Double { kind["start_time"]?.doubleValue ?? 0 }
    var endTime: Double? { kind["end_time"]?.doubleValue }
    var playCount: Int { Int(kind["last_slice_play_count"]?.doubleValue ?? 1) }
    var hasSlices: Bool { !(kind["slices"]?.arrayValue ?? []).isEmpty }

    /// Shows an Audio cue's `kind`; `duration` is its file's length, nil while not known. A field
    /// being typed in keeps its text.
    func show(cue: ModelID, kind: JSONValue, duration: Double?) {
        self.kind = kind
        let media = kind["media"]?.stringValue ?? ""
        let newFile = duration.flatMap { d in media.isEmpty || d <= 0 ? nil : (cue, media, d) }
        let fileChanged =
            newFile?.0 != file?.cue || newFile?.1 != file?.media || newFile?.2 != file?.duration
        file = newFile
        if startField.currentEditor() == nil { startField.stringValue = CueTime.format(startTime) }
        if endField.currentEditor() == nil {
            endField.stringValue = (endTime ?? duration).map(CueTime.format) ?? ""
        }
        let looping = playCount == 0
        if countField.currentEditor() == nil { countField.stringValue = String(max(1, looping ? 1 : playCount)) }
        playRadio.state = looping ? .off : .on
        loopRadio.state = looping ? .on : .off
        slicesNote.isHidden = !hasSlices
        if waveform.dragging == nil {
            waveform.trimStart = startTime
            waveform.trimEnd = endTime
        }
        if fileChanged {
            waveform.duration = file?.duration
            waveform.peaks = nil
            waveform.playhead = nil
            setVisible(start: 0, end: file?.duration ?? 1)
            waveform.message = loc(media.isEmpty ? "timeLoops.noFile" : "timeLoops.notRead")
            reload()
        }
        applyLock()
    }

    /// Where the cue plays in the file, from its elapsed action time: past the trimmed length it
    /// wraps while it loops (with slices this is the first pass only: an approximation).
    func showProgress(elapsed: Double?) {
        guard let elapsed, let file else {
            waveform.playhead = nil
            return
        }
        let end = endTime ?? file.duration
        let length = max(end - startTime, WaveformView.minLength)
        let t = playCount == 1 || hasSlices ? min(elapsed, length) : elapsed.truncatingRemainder(dividingBy: length)
        waveform.playhead = startTime + t
    }

    private func applyLock() {
        let sliced = hasSlices
        for c in [startField, endField] as [NSControl] { c.isEnabled = !isLocked }
        // With slices each slice has its own count (QLab: edited on the slice markers).
        for c in [playRadio, loopRadio] as [NSControl] { c.isEnabled = !isLocked && !sliced }
        countField.isEnabled = !isLocked && !sliced && playCount != 0
        waveform.isLocked = isLocked
        waveform.window?.invalidateCursorRects(for: waveform)
    }

    // MARK: Zoom and peaks

    var visible: (start: Double, end: Double) { (waveform.scale.start, waveform.scale.end) }

    private func setVisible(start: Double, end: Double) {
        waveform.scale = WaveformScale(start: start, end: end, width: waveform.bounds.width)
    }

    @objc func zoomIn() { zoom(by: 0.5) }
    @objc func zoomOut() { zoom(by: 2) }

    /// Narrows or widens the visible stretch about its middle, within the file.
    func zoom(by factor: Double) {
        guard let file else { return }
        let (a, b) = visible
        let span = min(file.duration, max(Self.minSpan, (b - a) * factor))
        let mid = (a + b) / 2
        let start = min(max(0, mid - span / 2), max(0, file.duration - span))
        setVisible(start: start, end: start + span)
        reload()
    }

    /// The peaks for what is visible at the view's width: from the cache, or fetched. Only while
    /// the tab is on screen and not in Show Mode: reading a waveform decodes the whole file, which
    /// a show in progress must not pay for a click in the cue list.
    func reload() {
        guard let file, isActive, !isLocked else { return }
        let (a, b) = visible
        let scaleFactor = waveform.window?.backingScaleFactor ?? 2
        let width = Int(waveform.bounds.width * scaleFactor)
        let key = PeaksKey(
            cue: file.cue, media: file.media, start: a, end: b, buckets: min(8192, max(1, width > 0 ? width : 1024)))
        generation += 1
        if let hit = cache[key] {
            waveform.peaks = hit
            return
        }
        guard let loadPeaks else { return }
        let gen = generation
        loading = Task { [weak self] in
            let result = await loadPeaks(key.cue, key.start, key.end, key.buckets)
            guard let self, gen == self.generation else { return }
            switch result {
            case .success(let peaks):
                self.remember(key, peaks)
                self.waveform.peaks = peaks
            case .failure:
                self.waveform.peaks = nil
                self.waveform.message = loc("timeLoops.unavailable")
            }
        }
    }

    private func remember(_ key: PeaksKey, _ peaks: WaveformPeaks) {
        if cache[key] == nil { cacheOrder.append(key) }
        cache[key] = peaks
        while cacheOrder.count > Self.cacheLimit { cache[cacheOrder.removeFirst()] = nil }
    }

    // MARK: Edits

    /// A handle let go somewhere new (the view compares with where it was taken, so a second drag
    /// before the engine answered the first still counts).
    private func commitDrag(_ handle: WaveformView.Handle, _ t: Double?) {
        guard !isLocked else { return }
        switch handle {
        case .start: if let t { onChange?(.start(t)) }
        case .end: onChange?(.end(t))
        }
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        guard !isLocked, let field = obj.object as? NSTextField else { return }
        let text = field.stringValue.trimmingCharacters(in: .whitespaces)
        if field === countField {
            guard let n = Int(text), (1...Self.maxPlayCount).contains(n) else {
                return refuse(field, "timeLoops.invalid.count")
            }
            if n != playCount { onChange?(.playCount(n)) }
            return
        }
        let end = endTime ?? file?.duration
        if field === endField, text.isEmpty {
            if endTime != nil { onChange?(.end(nil)) }
            return
        }
        guard let t = CueTime.parse(text) else { return refuse(field, "inspector.invalid.time") }
        if field === startField {
            // Until the file's length is known there is nothing to check a start against: the
            // engine would take one past the end and the cue would play nothing.
            guard let end else { return refuse(field, "timeLoops.invalid.unknownLength") }
            if t > end - WaveformView.minLength { return refuse(field, "timeLoops.invalid.trim") }
            if t != startTime { onChange?(.start(t)) }
            field.stringValue = CueTime.format(t)
        } else if field === endField {
            if t < startTime + WaveformView.minLength { return refuse(field, "timeLoops.invalid.trim") }
            // At or past the file's end: the end of the file.
            let value: Double? = file.map { t >= $0.duration } == true ? nil : t
            if value != endTime { onChange?(.end(value)) }
            field.stringValue = CueTime.format(value ?? t)
        }
    }

    private func refuse(_ field: NSTextField, _ key: String) {
        onInvalid?(key)
        if field === startField {
            field.stringValue = CueTime.format(startTime)
        } else if field === endField {
            field.stringValue = (endTime ?? file?.duration).map(CueTime.format) ?? ""
        } else {
            field.stringValue = String(max(1, playCount))
        }
    }

    @objc private func radioChanged(_ sender: NSButton) {
        guard !isLocked, !hasSlices else { return }
        if sender === loopRadio {
            if playCount != 0 { onChange?(.playCount(0)) }
        } else if playCount == 0 {
            onChange?(.playCount(max(1, Int(countField.stringValue) ?? 1)))
        }
    }
}
