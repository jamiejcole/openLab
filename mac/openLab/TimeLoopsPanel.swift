// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The time and loops tab of an Audio cue (AUD-10, AUD-11, AUD-12, AUD-14; QLab surface
/// `inspector.audio.time-loops`): start and end trim fields and how many times the cue plays on
/// the left, the file's waveform with draggable trims, slice markers and fades filling the rest,
/// and under it the slice buttons, the built-in fade switch with its curve, and zoom. Edits are
/// reported as `Change`s; the inspector turns them into model commands. Peaks come from the engine
/// (`waveform` request) and are kept per file and zoom.
@MainActor
final class TimeLoopsPanel: NSObject, NSTextFieldDelegate {
    enum Change: Equatable {
        case start(Double)
        /// nil: the end of the file.
        case end(Double?)
        /// 0: loop until devamped (infinite). With slices, the last slice's count.
        case playCount(Int)
        /// A new marker; the slice before it plays once.
        case addSlice(Double)
        /// The marker at `from` moved to `to`.
        case moveSlice(from: Double, to: Double)
        /// The marker at this time deleted.
        case removeSlice(Double)
        case clearSlices
        /// The play count of the slice the marker at this time ends.
        case sliceCount(Double, Int)
        /// Built-in fade lengths (0: none) and the one curve of both; nil keeps what the cue has.
        case fades(fadeIn: Double?, fadeOut: Double?, curve: String?)
    }

    /// A slice marker: where it is in the file, and the play count of the slice it ends (0: until
    /// devamped).
    struct SliceMark: Equatable {
        var time: Double
        var playCount: Int
    }

    /// `kind` with `change` made to it. The inspector applies it to the cue as the model has it
    /// plus any edit not yet answered, never to a copy the tab kept: a drag or a quick second edit
    /// changes only what it touched, and an edit made meanwhile elsewhere (another client, OSC,
    /// undo) stays. A marker that is no longer there, or a place too near another, changes
    /// nothing.
    static func apply(_ change: Change, to kind: JSONValue) -> JSONValue {
        var kind = kind
        var marks = marks(of: kind)
        let start = kind["start_time"]?.doubleValue ?? 0
        let end = kind["end_time"]?.doubleValue ?? .infinity
        let index = { (t: Double) in marks.firstIndex { abs($0.time - t) < 1e-9 } }
        /// Whether `t` may hold a marker given the others in `others`.
        func fits(_ t: Double, among others: [SliceMark]) -> Bool {
            let near = WaveformView.minLength - 1e-9
            return t - start >= near && end - t >= near && others.allSatisfy { abs($0.time - t) >= near }
        }
        switch change {
        case .start(let t):
            kind.set("start_time", .double(t))
            return kind
        case .end(let e):
            kind.set("end_time", e.map { .double($0) } ?? .null)
            return kind
        case .playCount(let n):
            kind.set("last_slice_play_count", .int(Int64(n)))
            return kind
        case .addSlice(let t):
            guard marks.count < maxSliceMarks, fits(t, among: marks) else { return kind }
            marks.insert(SliceMark(time: t, playCount: 1), at: marks.firstIndex { $0.time > t } ?? marks.count)
        case .moveSlice(let from, let to):
            guard let i = index(from) else { return kind }
            var others = marks
            others.remove(at: i)
            // Only between the same neighbours: a move never reorders the slices' counts.
            let before = i > 0 ? marks[i - 1].time : -Double.infinity
            let after = i + 1 < marks.count ? marks[i + 1].time : Double.infinity
            guard to > before, to < after, fits(to, among: others) else { return kind }
            marks[i].time = to
        case .removeSlice(let t):
            guard let i = index(t) else { return kind }
            marks.remove(at: i)
        case .clearSlices: marks = []
        case .sliceCount(let t, let n):
            guard let i = index(t) else { return kind }
            marks[i].playCount = n
        case .fades(let fadeIn, let fadeOut, let curve):
            for (key, length) in [("fade_in", fadeIn), ("fade_out", fadeOut)] {
                var spec = kind[key] ?? .object(["duration": .double(0), "curve": .string("linear")])
                if let length { spec.set("duration", .double(length)) }
                if let curve { spec.set("curve", .string(curve)) }
                kind.set(key, spec)
            }
            return kind
        }
        kind.set(
            "slices",
            .array(marks.map { .object(["time": .double($0.time), "play_count": .int(Int64($0.playCount))]) }))
        return kind
    }

    static func marks(of kind: JSONValue) -> [SliceMark] {
        (kind["slices"]?.arrayValue ?? []).compactMap { m in
            guard let t = m["time"]?.doubleValue, let n = m["play_count"]?.doubleValue else { return nil }
            return SliceMark(time: t, playCount: Int(n))
        }
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
    /// The audio engine plays at most 256 slices, one more when a start part way in splits a loop
    /// (`MAX_SLICES`): more markers would leave a cue that cannot play.
    static let maxSliceMarks = 254
    /// The fade each way that switching the built-in fades on gives, at most a quarter of the cue.
    static let defaultFade = 1.0
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
    let deleteSlicesButton = NSButton(title: loc("timeLoops.deleteSlices"), target: nil, action: nil)
    let addSliceButton = NSButton(title: loc("timeLoops.addSlice"), target: nil, action: nil)
    let fadeCheck = NSButton(checkboxWithTitle: loc("timeLoops.fades"), target: nil, action: nil)
    let fadeCurvePopup = NSPopUpButton()
    /// Edits one slice's play count in place, over its number on the waveform.
    let sliceCountField = NSTextField()
    /// The slice whose count `sliceCountField` edits (its index when the edit began) and the
    /// marker that ends it (nil: the last slice).
    private(set) var editingSlice: Int?
    private var editingMark: Double?

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
    private var shownCue: ModelID?
    private var kind: JSONValue = .null
    private var generation = 0
    /// The fetch under way (tests wait on it).
    private(set) var loading: Task<Void, Never>?
    /// The generation of the read out; one at a time for the panel.
    private var inFlight: Int?
    /// A zoom or pan asked for other peaks while a read was out.
    private(set) var reloadPending = false

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
        deleteSlicesButton.setAX(AX.timeLoopsDeleteSlices)
        addSliceButton.setAX(AX.timeLoopsAddSlice)
        fadeCheck.setAX(AX.timeLoopsFades)
        fadeCurvePopup.setAX(AX.timeLoopsFadeCurve)
        sliceCountField.setAX(AX.timeLoopsSliceCount)
        for f in [startField, endField, countField, sliceCountField] {
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
        for (b, action) in [
            (deleteSlicesButton, #selector(deleteSlices)), (addSliceButton, #selector(addSlice)),
            (fadeCheck, #selector(fadeToggled)),
        ] {
            b.target = self
            b.action = action
        }
        for b in [deleteSlicesButton, addSliceButton] { b.bezelStyle = .texturedRounded }
        for key in FadeSettingsPanel.curves {
            fadeCurvePopup.menu?.addItem(NSMenuItem(title: loc("fade.curve.\(key)"), action: nil, keyEquivalent: ""))
        }
        fadeCurvePopup.target = self
        fadeCurvePopup.action = #selector(fadeCurveChanged)
        sliceCountField.isHidden = true
        sliceCountField.alignment = .center
        waveform.addSubview(sliceCountField)
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
        waveform.onSliceCommit = { [weak self] from, to in
            guard let self, !isLocked else { return }
            onChange?(to.map { .moveSlice(from: from, to: $0) } ?? .removeSlice(from))
        }
        waveform.onFadesCommit = { [weak self] fadeIn, fadeOut in
            guard let self, !isLocked else { return }
            onChange?(.fades(fadeIn: fadeIn, fadeOut: fadeOut, curve: nil))
        }
        waveform.onDragEnded = { [weak self] in self?.syncWaveform() }
        waveform.onInsertion = { [weak self] in self?.applyLock() }
        waveform.onEditCount = { [weak self] i in self?.editSliceCount(i) }
        waveform.onResize = { [weak self] in self?.reload() }
        waveform.onZoom = { [weak self] factor, anchor in self?.zoom(by: factor, anchor: anchor) }
        waveform.onPan = { [weak self] fraction in
            guard let self else { return }
            self.pan(bySeconds: -fraction * (self.visible.end - self.visible.start))
        }
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
        let strip = NSStackView(views: [
            deleteSlicesButton, addSliceButton, fadeCheck, fadeCurvePopup, spacer, zoomOutButton, zoomInButton,
        ])
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
    var hasSlices: Bool { !sliceMarks.isEmpty }
    var sliceMarks: [SliceMark] { Self.marks(of: kind) }
    var fadeIn: Double { kind["fade_in"]?["duration"]?.doubleValue ?? 0 }
    var fadeOut: Double { kind["fade_out"]?["duration"]?.doubleValue ?? 0 }
    /// The built-in fades' curve: the fade-in's, which the tab sets for both.
    var fadeCurve: String { kind["fade_in"]?["curve"]?.stringValue ?? "linear" }
    /// The built-in fades are on while either has a length.
    var hasFades: Bool { fadeIn > 0 || fadeOut > 0 }

    /// Shows an Audio cue's `kind`; `duration` is its file's length, nil while not known. A field
    /// being typed in keeps its text.
    func show(cue: ModelID, kind: JSONValue, duration: Double?) {
        self.kind = kind
        let media = kind["media"]?.stringValue ?? ""
        let newFile = duration.flatMap { d in media.isEmpty || d <= 0 ? nil : (cue, media, d) }
        let fileChanged =
            newFile?.0 != file?.cue || newFile?.1 != file?.media || newFile?.2 != file?.duration
        file = newFile
        // An insertion point or count edit belongs to the cue it was made on.
        if cue != shownCue {
            shownCue = cue
            waveform.insertion = nil
            endSliceCountEdit()
        }
        if startField.currentEditor() == nil { startField.stringValue = CueTime.format(startTime) }
        if endField.currentEditor() == nil {
            endField.stringValue = (endTime ?? duration).map(CueTime.format) ?? ""
        }
        let looping = playCount == 0
        if countField.currentEditor() == nil { countField.stringValue = String(max(1, looping ? 1 : playCount)) }
        playRadio.state = looping ? .off : .on
        loopRadio.state = looping ? .on : .off
        slicesNote.isHidden = !hasSlices
        if waveform.dragging == nil { syncWaveform() }
        fadeCheck.state = hasFades ? .on : .off
        fadeCurvePopup.selectItem(at: FadeSettingsPanel.curves.firstIndex(of: fadeCurve) ?? 0)
        if fileChanged {
            waveform.duration = file?.duration
            waveform.peaks = nil
            waveform.playhead = nil
            waveform.insertion = nil
            endSliceCountEdit()
            setVisible(start: 0, end: file?.duration ?? 1)
            waveform.message = loc(media.isEmpty ? "timeLoops.noFile" : "timeLoops.notRead")
            reload()
        }
        applyLock()
    }

    /// The waveform shows the cue as last shown here. While a drag is under way the view keeps its
    /// own handle; when the drag ends it is shown afresh (`onDragEnded`), so nothing the drag
    /// kept outlives it.
    private func syncWaveform() {
        waveform.trimStart = startTime
        waveform.trimEnd = endTime
        let marks = sliceMarks
        waveform.sliceMarks = marks.map(\.time)
        waveform.sliceCounts = marks.map(\.playCount) + [playCount]
        waveform.fadesShown = hasFades
        waveform.fadeIn = fadeIn
        waveform.fadeOut = fadeOut
        waveform.fadeCurve = fadeCurve
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
        deleteSlicesButton.isEnabled = !isLocked && sliced
        addSliceButton.isEnabled = !isLocked && waveform.insertion != nil
        fadeCheck.isEnabled = !isLocked
        fadeCurvePopup.isEnabled = !isLocked && hasFades
        if isLocked { endSliceCountEdit() }
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

    /// Narrows or widens the visible stretch, within the file, keeping the time at `anchor` (a
    /// fraction of the view's width; the middle by default) where it is.
    func zoom(by factor: Double, anchor: Double = 0.5) {
        guard let viewport = viewport?.zoomed(by: factor, anchor: anchor) else { return }
        show(viewport)
    }

    /// Moves the visible stretch along the file by `seconds`, stopping at its start and end.
    func pan(bySeconds seconds: Double) {
        guard let viewport = viewport?.panned(by: seconds) else { return }
        show(viewport)
    }

    private var viewport: WaveformViewport? {
        file.map { WaveformViewport(start: visible.start, end: visible.end, duration: $0.duration) }
    }

    private func show(_ viewport: WaveformViewport) {
        guard viewport.start != visible.start || viewport.end != visible.end else { return }
        setVisible(start: viewport.start, end: viewport.end)
        reload()
    }

    /// The peaks for what is visible at the view's width: from the cache, or fetched. Only while
    /// the tab is on screen and not in Show Mode: reading a waveform decodes the whole file, which
    /// a show in progress must not pay for a click in the cue list.
    func reload() { ask(busyRetries: 0) }

    /// How many times a refusal for too many waveforms waiting is asked again before giving up.
    static let busyRetryLimit = 3

    private func ask(busyRetries: Int) {
        // Whatever read is out is for something no longer asked for, even when nothing is asked now.
        generation += 1
        guard let file, isActive, !isLocked else { return }
        let (a, b) = visible
        let scaleFactor = waveform.window?.backingScaleFactor ?? 2
        let width = Int(waveform.bounds.width * scaleFactor)
        let key = PeaksKey(
            cue: file.cue, media: file.media, start: a, end: b, buckets: min(8192, max(1, width > 0 ? width : 1024)))
        if let hit = cache[key] {
            reloadPending = false
            waveform.peaks = hit
            return
        }
        guard let loadPeaks else { return }
        // Each read decodes a whole file and the engine queues only a few: one is out for the whole
        // panel, and of the asks made meanwhile (a pinch, arrowing through cues) only the latest is
        // kept and made when it comes back. The result of a read nobody asks for now is dropped.
        if inFlight != nil {
            reloadPending = true
            return
        }
        reloadPending = false
        let gen = generation
        inFlight = gen
        loading = Task { [weak self] in
            let result = await loadPeaks(key.cue, key.start, key.end, key.buckets)
            guard let self else { return }
            self.inFlight = nil
            if self.reloadPending {
                self.reloadPending = false
                self.ask(busyRetries: 0)
                return
            }
            guard gen == self.generation else { return }
            switch result {
            case .success(let peaks):
                self.remember(key, peaks)
                self.waveform.peaks = peaks
            case .failure(.engine(.commandRefused, _)) where busyRetries < Self.busyRetryLimit:
                // The engine's queue was full of reads that nobody waits for now; it drains.
                self.ask(busyRetries: busyRetries + 1)
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
        case .slice, .fadeIn, .fadeOut: break
        }
    }

    /// A new marker at the insertion point. The slice before it plays once and the one after keeps
    /// the count the split slice had, as QLab does.
    @objc func addSlice() {
        guard !isLocked, let t = waveform.insertion else { return }
        let marks = sliceMarks
        guard marks.count < Self.maxSliceMarks else { return refuseSlice("timeLoops.invalid.tooManySlices") }
        let end = endTime ?? file?.duration ?? .infinity
        let bounds = [startTime, end] + marks.map(\.time)
        guard bounds.allSatisfy({ abs($0 - t) >= WaveformView.minLength - 1e-9 }), t > startTime, t < end else {
            return refuseSlice("timeLoops.invalid.slice")
        }
        onChange?(.addSlice(t))
    }

    @objc func deleteSlices() {
        guard !isLocked, hasSlices else { return }
        onChange?(.clearSlices)
    }

    private func refuseSlice(_ key: String) { onInvalid?(key) }

    /// The built-in fades on (each way `defaultFade`, at most a quarter of the cue) or off (no
    /// length; the curve is kept).
    @objc private func fadeToggled() {
        guard !isLocked else { return }
        if fadeCheck.state == .on, !hasFades {
            let length = (endTime ?? file?.duration).map { $0 - startTime } ?? 4 * Self.defaultFade
            let d = WaveformView.clampRounded(min(Self.defaultFade, length / 4), 0, Self.defaultFade)
            onChange?(.fades(fadeIn: d, fadeOut: d, curve: nil))
        } else if fadeCheck.state == .off, hasFades {
            onChange?(.fades(fadeIn: 0, fadeOut: 0, curve: nil))
        }
        // The cue as shown (the inspector shows it again with the edit).
        fadeCheck.state = hasFades ? .on : .off
    }

    @objc private func fadeCurveChanged() {
        guard !isLocked else { return }
        let curve = FadeSettingsPanel.curves[max(0, fadeCurvePopup.indexOfSelectedItem)]
        let other = kind["fade_out"]?["curve"]?.stringValue ?? curve
        if curve != fadeCurve || curve != other { onChange?(.fades(fadeIn: nil, fadeOut: nil, curve: curve)) }
    }

    /// Opens the in-place editor on slice `index`'s play count (the last slice's is the cue's
    /// `last_slice_play_count`).
    func editSliceCount(_ index: Int) {
        let marks = sliceMarks
        guard !isLocked, !marks.isEmpty, index >= 0, index <= marks.count else { return }
        // One edit at a time: a field editor still open is closed without its text.
        endSliceCountEdit()
        editingSlice = index
        editingMark = index < marks.count ? marks[index].time : nil
        let count = index < marks.count ? marks[index].playCount : playCount
        sliceCountField.stringValue = count == 0 ? "\u{221E}" : String(count)
        sliceCountField.frame = waveform.countRect(index)
        sliceCountField.isHidden = false
        waveform.window?.makeFirstResponder(sliceCountField)
    }

    private func endSliceCountEdit() {
        editingSlice = nil
        editingMark = nil
        sliceCountField.abortEditing()
        sliceCountField.isHidden = true
    }

    /// A slice's typed play count: a whole number of plays, or a letter (or ∞) to loop until
    /// devamped, as QLab takes one. 0 is refused: openLab cannot skip a slice. The slice is the one
    /// the marker found when the edit began ends, wherever it is now.
    private func commitSliceCount(_ text: String) {
        guard editingSlice != nil else { return }
        let mark = editingMark
        endSliceCountEdit()
        let n: Int
        if let typed = Int(text), (1...Self.maxPlayCount).contains(typed) {
            n = typed
        } else if text == "\u{221E}" || text.first?.isLetter == true {
            n = 0
        } else {
            return refuseSlice("timeLoops.invalid.sliceCount")
        }
        if let mark {
            onChange?(.sliceCount(mark, n))
        } else if n != playCount {
            onChange?(.playCount(n))
        }
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        guard !isLocked, let field = obj.object as? NSTextField else { return }
        let text = field.stringValue.trimmingCharacters(in: .whitespaces)
        if field === sliceCountField { return commitSliceCount(text) }
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
            if let first = sliceMarks.first, t > first.time - WaveformView.minLength {
                return refuse(field, "timeLoops.invalid.trimSlices")
            }
            if t != startTime { onChange?(.start(t)) }
            field.stringValue = CueTime.format(t)
        } else if field === endField {
            if t < startTime + WaveformView.minLength { return refuse(field, "timeLoops.invalid.trim") }
            if let last = sliceMarks.last, t < last.time + WaveformView.minLength {
                return refuse(field, "timeLoops.invalid.trimSlices")
            }
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
