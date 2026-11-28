// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// One region of a stage as the stage editor shows it.
struct RegionRow: Equatable {
    var id: ModelID
    var route: ModelID?
    var bounds: RegionBounds

    /// A stage's regions as the model holds them (docs/file-format.md §4.8).
    static func of(_ stage: ModelObject) -> [RegionRow] {
        (stage.raw["regions"]?.arrayValue ?? []).compactMap { r in
            guard let id = r["id"]?.stringValue else { return nil }
            let n = { (key: String) in Int(r[key]?.doubleValue ?? 0) }
            return RegionRow(
                id: ModelID(id), route: r["route"]?.stringValue.map { ModelID($0) },
                bounds: RegionBounds(x: n("x"), y: n("y"), width: n("width"), height: n("height")))
        }
    }
}

/// The stage editor (VID-01; QLab 5 manual pp. 227-229, UI map `window.video-stage-editor.*.demo`), a
/// window per stage opened by Edit on the stage's row of the Video Outputs page (or beside a Video
/// cue's stage pop-up). At the top the stage's name, its layer (a number, or a word for the top and
/// bottom), whether its black stays up between cues, and its size as text with a button that opens
/// the resize dialog. Below, its regions: a table (each region's letter, colour, a thumbnail of the
/// stage with the region lit, the output route it is sent to, a remove button) beside a box with the
/// selected region's size (a lock links width and height), lower-left corner (measured from the
/// stage's centre, upwards) and upper-right corner; then a pop-up that adds a region on a route; the
/// canvas (`StageCanvasView`, in a scroll view: fitted, or at a scale the pop-up at the foot picks);
/// at the foot the box showing the guides of the routes the stage uses.
///
/// It holds nothing the engine does not: every control is drawn from the model, every edit is one
/// engine command and one undo step; a value that would put a region outside the stage is refused
/// here with a message and the field shows the old value again. The window closes when its stage
/// is removed.
@MainActor
final class StageEditorWindowController: NSWindowController, NSTextFieldDelegate, NSWindowDelegate {
    let store: WorkspaceStore
    let stageID: ModelID
    let nameField = NSTextField()
    let layerField = NSTextField()
    let layerStepper = NSStepper()
    let blackCheck = NSButton(checkboxWithTitle: loc("settings.video.black"), target: nil, action: nil)
    /// The stage's size, read only: the resize dialog changes it.
    let sizeLabel = NSTextField(labelWithString: "")
    let resizeButton = NSButton(title: loc("stageEditor.resize"), target: nil, action: nil)
    /// Shows the alignment guides on every route the stage's regions use (QLab 5 manual p. 230):
    /// the same state as those routes' boxes on Output Routing, mixed when only some show them.
    let guidesCheck = NSButton(checkboxWithTitle: loc("stageEditor.guides"), target: nil, action: nil)
    /// The region table's rows, rebuilt when the regions or routes change.
    let regionList = NSStackView()
    private(set) var regionRows: [RegionRowView] = []
    private(set) var regionLetters: [NSTextField] = []
    /// Each row's colour dot (a click selects the region).
    private(set) var regionChips: [RegionChip] = []
    private(set) var regionThumbnails: [RegionThumbnail] = []
    private(set) var regionRoutePopups: [NSPopUpButton] = []
    private(set) var regionRemoveButtons: [NSButton] = []
    let newRegionPopup = NSPopUpButton(frame: .zero, pullsDown: true)
    private var shownNewRegionItems: [String]?
    /// The selected region's box: its heading names it, or says none is selected.
    let regionHeading = NSTextField(labelWithString: "")
    let regionWidthField = NSTextField()
    let regionHeightField = NSTextField()
    /// Links the region's width and height: typing one sets the other in the same shape. A view
    /// aid, not saved and no undo step.
    let aspectLock = NSButton(title: "", target: nil, action: nil)
    let lowerLeftXField = NSTextField()
    let lowerLeftYField = NSTextField()
    let upperRightXLabel = NSTextField(labelWithString: "")
    let upperRightYLabel = NSTextField(labelWithString: "")
    let canvas = StageCanvasView()
    let canvasScroll = NSScrollView()
    /// Fit, or a scale of the stage's pixels in points.
    let scalePopup = NSPopUpButton(frame: .zero, pullsDown: false)
    let messageLabel = NSTextField(wrappingLabelWithString: "")
    /// The selected region.
    private(set) var selected: ModelID?
    /// A region just added: selected once the model has it.
    private var selectWhenAdded: ModelID?
    /// The region the four region fields were drawn for: what is typed in them goes to it, or to
    /// nothing once it is gone, never to whichever region is selected by then.
    private var fieldsRegion: ModelID?
    private var subs: [AnyCancellable] = []
    /// Edits on their way, one after another (tests wait for them).
    private(set) var editing: Task<Void, Never>?
    /// What the controls were last drawn from.
    private var drawn: Drawn?
    /// Each field's text as last drawn or committed: a field still holding the caret with other
    /// text (typed, not committed) keeps it; one holding this text shows the model's value (an
    /// undo's, a refusal's), as after Return, which leaves the caret in the field.
    private var drawnText: [ObjectIdentifier: String] = [:]
    /// The stage was in the workspace at some draw: its going since closes the window.
    private var seenStage = false
    /// Told when the window closes because its stage went (the settings window forgets it).
    var onGone: (() -> Void)?
    /// Whether the resize dialog is run on screen (a scenario or test drives it off screen).
    var presentsDialogs = true
    /// The resize dialog while it is up.
    private(set) var resizeDialog: ResizeStageDialog?

    private struct Drawn: Equatable {
        var name: String
        var layer: Int
        var keepBlack: Bool
        var width: Int
        var height: Int
        var regions: [RegionRow]
        var routes: [String]
        var selected: ModelID?
        var guides: NSControl.StateValue
        var guided: Bool
        var locked: Bool
    }

    static let minSize = NSSize(width: 620, height: 480)
    /// The selected region's box: its labels, two number fields and the lock between them.
    static let regionBoxWidth: CGFloat = 300
    static let id = "stage-editor"
    /// The scale pop-up's choices after Fit, in percent of a stage pixel per point.
    static let scales: [Int] = [10, 25, 50, 75, 100, 150, 200, 300, 400]

    init(store: WorkspaceStore, stage: ModelID) {
        self.store = store
        stageID = stage
        let window = PanicKeyWindow(
            contentRect: NSRect(x: 0, y: 0, width: 900, height: 700),
            styleMask: [.titled, .closable, .resizable, .miniaturizable], backing: .buffered, defer: false)
        window.identifier = NSUserInterfaceItemIdentifier(Self.id)
        window.isReleasedWhenClosed = false
        window.contentMinSize = Self.minSize
        super.init(window: window)
        window.delegate = self
        window.contentView = buildContent()
        window.center()
        subs.append(store.$model.sink { [weak self] _ in self?.renderSoon() })
        subs.append(store.$videoOutputs.sink { [weak self] _ in self?.renderSoon() })
        canvasScroll.contentView.postsFrameChangedNotifications = true
        NotificationCenter.default.publisher(for: NSView.frameDidChangeNotification, object: canvasScroll.contentView)
            .sink { [weak self] _ in MainActor.assumeIsolated { self?.layoutCanvas() } }
            .store(in: &subs)
        render()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Brings the window up with no field focused.
    func present() {
        showWindow(nil)
        window?.makeFirstResponder(nil)
        render()
    }

    // MARK: Building

    private func label(_ key: String) -> NSTextField {
        let l = NSTextField(labelWithString: loc(key))
        l.alignment = .right
        l.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        l.setContentHuggingPriority(.required, for: .horizontal)
        l.setContentCompressionResistancePriority(.required, for: .horizontal)
        return l
    }

    private func number(_ field: NSTextField, _ id: String, _ label: String, width: CGFloat = 60) {
        field.setAX("\(Self.id)-\(id)", label: label)
        field.controlSize = .small
        field.font = .monospacedDigitSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
        field.alignment = .right
        field.delegate = self
        field.widthAnchor.constraint(equalToConstant: width).isActive = true
    }

    private func value(_ label: NSTextField, _ id: String) {
        label.setAX("\(Self.id)-\(id)")
        label.font = .monospacedDigitSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
        label.alignment = .right
        label.widthAnchor.constraint(equalToConstant: 60).isActive = true
    }

    private func small(_ control: NSControl) {
        control.controlSize = .small
        control.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
    }

    private func buildContent() -> NSView {
        // Top: name, layer and keep-black; then the size and the resize button.
        nameField.setAX("\(Self.id)-name", label: loc("stageEditor.name"))
        small(nameField)
        nameField.delegate = self
        nameField.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        nameField.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(250), for: .horizontal)
        nameField.widthAnchor.constraint(greaterThanOrEqualToConstant: 120).isActive = true
        number(layerField, "layer", loc("settings.video.col.layer"), width: 56)
        layerField.alignment = .left
        layerField.toolTip = loc("stageEditor.layer.tip")
        layerStepper.setAX("\(Self.id)-layer-stepper", label: loc("settings.video.col.layer"))
        layerStepper.controlSize = .small
        layerStepper.minValue = Double(StageLayer.bottom)
        layerStepper.maxValue = Double(StageLayer.top)
        layerStepper.increment = 1
        layerStepper.valueWraps = false
        layerStepper.target = self
        layerStepper.action = #selector(layerStepped)
        blackCheck.setAX("\(Self.id)-black", label: loc("settings.video.black"))
        small(blackCheck)
        blackCheck.target = self
        blackCheck.action = #selector(blackClicked)
        blackCheck.toolTip = loc("stageEditor.black.tip")
        let layerPair = NSStackView(views: [layerField, layerStepper])
        layerPair.spacing = 2
        let nameRow = NSStackView(views: [label("stageEditor.name"), nameField, label("stageEditor.layer"), layerPair])
        nameRow.spacing = 8
        nameRow.setCustomSpacing(16, after: nameField)
        nameRow.addView(blackCheck, in: .trailing)
        nameRow.setCustomSpacing(16, after: layerPair)
        sizeLabel.setAX("\(Self.id)-size", label: loc("stageEditor.size"))
        sizeLabel.font = .monospacedDigitSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
        sizeLabel.isSelectable = false
        resizeButton.setAX("\(Self.id)-resize", label: loc("stageEditor.resize"))
        small(resizeButton)
        resizeButton.bezelStyle = .rounded
        resizeButton.target = self
        resizeButton.action = #selector(resizeClicked)
        resizeButton.toolTip = loc("stageEditor.resize.tip")
        let sizeRow = NSStackView(views: [label("stageEditor.size"), sizeLabel, resizeButton])
        sizeRow.spacing = 8
        sizeRow.setCustomSpacing(12, after: sizeLabel)

        // The region table.
        regionList.orientation = .vertical
        regionList.alignment = .leading
        regionList.spacing = 0
        regionList.setAX("\(Self.id)-regions")
        let listHolder = FlippedView()
        regionList.translatesAutoresizingMaskIntoConstraints = false
        listHolder.addSubview(regionList)
        let listScroll = NSScrollView()
        listScroll.hasVerticalScroller = true
        listScroll.autohidesScrollers = true
        listScroll.borderType = .bezelBorder
        listScroll.drawsBackground = false
        listHolder.translatesAutoresizingMaskIntoConstraints = false
        listScroll.documentView = listHolder
        listScroll.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        NSLayoutConstraint.activate([
            listHolder.topAnchor.constraint(equalTo: listScroll.contentView.topAnchor),
            listHolder.leadingAnchor.constraint(equalTo: listScroll.contentView.leadingAnchor),
            listHolder.widthAnchor.constraint(equalTo: listScroll.contentView.widthAnchor),
            regionList.topAnchor.constraint(equalTo: listHolder.topAnchor),
            regionList.leadingAnchor.constraint(equalTo: listHolder.leadingAnchor),
            regionList.trailingAnchor.constraint(equalTo: listHolder.trailingAnchor),
            regionList.bottomAnchor.constraint(equalTo: listHolder.bottomAnchor),
            listScroll.widthAnchor.constraint(greaterThanOrEqualToConstant: 220),
            listScroll.heightAnchor.constraint(equalToConstant: 128),
        ])

        // The selected region's box.
        regionHeading.setAX("\(Self.id)-region-heading")
        regionHeading.font = .systemFont(ofSize: NSFont.smallSystemFontSize, weight: .semibold)
        number(regionWidthField, "region-width", loc("stageEditor.regionWidth"))
        number(regionHeightField, "region-height", loc("stageEditor.regionHeight"))
        number(lowerLeftXField, "lower-left-x", loc("stageEditor.lowerLeftX"))
        number(lowerLeftYField, "lower-left-y", loc("stageEditor.lowerLeftY"))
        value(upperRightXLabel, "upper-right-x")
        value(upperRightYLabel, "upper-right-y")
        aspectLock.setAX("\(Self.id)-aspect-lock", label: loc("stageEditor.aspectLock"))
        aspectLock.setButtonType(.pushOnPushOff)
        aspectLock.isBordered = false
        aspectLock.imagePosition = .imageOnly
        aspectLock.toolTip = loc("stageEditor.aspectLock.tip")
        aspectLock.target = self
        aspectLock.action = #selector(lockClicked)
        aspectLock.widthAnchor.constraint(equalToConstant: 18).isActive = true
        let params = NSGridView(views: [
            [label("stageEditor.regionSize"), regionWidthField, aspectLock, regionHeightField],
            [label("stageEditor.lowerLeft"), lowerLeftXField, NSGridCell.emptyContentView, lowerLeftYField],
            [label("stageEditor.upperRight"), upperRightXLabel, NSGridCell.emptyContentView, upperRightYLabel],
        ])
        params.rowAlignment = .firstBaseline
        params.rowSpacing = 8
        params.columnSpacing = 6
        params.column(at: 0).xPlacement = .trailing
        params.column(at: 2).xPlacement = .center
        params.cell(atColumnIndex: 2, rowIndex: 0).yPlacement = .center
        params.cell(atColumnIndex: 2, rowIndex: 0).rowAlignment = .none
        let boxStack = NSStackView(views: [regionHeading, params])
        boxStack.orientation = .vertical
        boxStack.alignment = .leading
        boxStack.spacing = 10
        boxStack.edgeInsets = NSEdgeInsets(top: 8, left: 10, bottom: 10, right: 10)
        let regionBox = NSBox()
        regionBox.boxType = .custom
        regionBox.cornerRadius = 6
        regionBox.borderWidth = 1
        regionBox.borderColor = .separatorColor
        regionBox.fillColor = .clear
        regionBox.contentViewMargins = .zero
        regionBox.contentView = boxStack
        regionBox.setAX("\(Self.id)-region-box")
        // The box keeps its own width; the table takes the rest of the row, as QLab 5's does.
        regionBox.widthAnchor.constraint(equalToConstant: Self.regionBoxWidth).isActive = true
        regionBox.setContentCompressionResistancePriority(.required, for: .horizontal)
        let middle = NSStackView(views: [listScroll, regionBox])
        middle.alignment = .top
        middle.spacing = 12
        listScroll.heightAnchor.constraint(equalTo: regionBox.heightAnchor).isActive = true

        newRegionPopup.setAX("\(Self.id)-new-region", label: loc("stageEditor.newRegion"))
        small(newRegionPopup)
        newRegionPopup.target = self
        newRegionPopup.action = #selector(newRegionChosen(_:))
        newRegionPopup.setContentHuggingPriority(.required, for: .horizontal)

        // The canvas: fitted to the scroll view, or at a scale (then scrolled when larger).
        canvas.setAX("\(Self.id)-canvas", label: loc("stageEditor.canvas"))
        canvas.onSelect = { [weak self] i in self?.select(index: i) }
        canvas.translatesAutoresizingMaskIntoConstraints = true
        canvasScroll.documentView = canvas
        canvasScroll.hasVerticalScroller = true
        canvasScroll.hasHorizontalScroller = true
        canvasScroll.autohidesScrollers = true
        canvasScroll.drawsBackground = true
        canvasScroll.backgroundColor = .underPageBackgroundColor
        canvasScroll.borderType = .noBorder
        canvasScroll.setAX("\(Self.id)-canvas-scroll")
        canvasScroll.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        canvasScroll.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        canvasScroll.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .vertical)
        canvasScroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 120).isActive = true

        // The foot: guides on the left, the scale on the right.
        guidesCheck.setAX("\(Self.id)-guides", label: loc("stageEditor.guides"))
        small(guidesCheck)
        guidesCheck.allowsMixedState = true
        guidesCheck.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(700), for: .horizontal)
        guidesCheck.target = self
        guidesCheck.action = #selector(guidesClicked)
        guidesCheck.toolTip = loc("stageEditor.guides.tip")
        scalePopup.setAX("\(Self.id)-scale", label: loc("stageEditor.scale"))
        small(scalePopup)
        scalePopup.addItem(withTitle: loc("stageEditor.scale.fit"))
        scalePopup.lastItem?.representedObject = 0
        for p in Self.scales {
            scalePopup.addItem(withTitle: String(format: loc("stageEditor.scale.percent"), p))
            scalePopup.lastItem?.representedObject = p
        }
        scalePopup.target = self
        scalePopup.action = #selector(scaleChosen)
        scalePopup.setContentHuggingPriority(.required, for: .horizontal)
        scalePopup.setContentCompressionResistancePriority(.required, for: .horizontal)
        let foot = NSStackView(views: [guidesCheck])
        foot.addView(scalePopup, in: .trailing)
        foot.spacing = 12

        messageLabel.setAX("\(Self.id)-message")
        messageLabel.textColor = .systemRed
        messageLabel.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        let divider = NSBox()
        divider.boxType = .separator
        let footDivider = NSBox()
        footDivider.boxType = .separator
        let stack = NSStackView(views: [
            nameRow, sizeRow, divider, middle, newRegionPopup, canvasScroll, messageLabel, footDivider, foot,
        ])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        stack.edgeInsets = NSEdgeInsets(top: 14, left: 16, bottom: 10, right: 16)
        stack.setCustomSpacing(0, after: canvasScroll)
        stack.setCustomSpacing(4, after: messageLabel)
        for v in [nameRow, divider, middle, footDivider, foot] {
            v.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32).isActive = true
        }
        // The canvas runs to the window's sides, as a stage editor's does.
        canvasScroll.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        messageLabel.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32).isActive = true
        return stack
    }

    // MARK: Drawing

    private var stage: ModelObject? { store.model?.stages.first { $0.id == stageID } }
    private var routes: [ModelObject] { store.model?.routes ?? [] }

    /// The regions as the model holds them now.
    var regions: [RegionRow] { stage.map(RegionRow.of) ?? [] }

    /// The routes the stage's regions are sent to, in route order, each once (a region with no
    /// route, or one the workspace lacks, adds none).
    var guidedRoutes: [ModelID] {
        let used = Set(regions.compactMap(\.route))
        return routes.map(\.id).filter { used.contains($0) }
    }

    /// The guides box's state: on when every route of the stage shows its guides, off when none
    /// does, mixed between.
    private var guidesState: NSControl.StateValue {
        let shown = guidedRoutes.map { store.videoOutputs.route($0)?.guides ?? false }
        if shown.isEmpty || !shown.contains(true) { return .off }
        return shown.contains(false) ? .mixed : .on
    }

    private func renderSoon() {
        DispatchQueue.main.async { [weak self] in MainActor.assumeIsolated { self?.render() } }
    }

    private func routeName(_ id: ModelID?) -> String? {
        guard let id else { return nil }
        return routes.first { $0.id == id }?.raw["name"]?.stringValue
    }

    /// The stage's size.
    private var stageSize: (Int, Int) {
        (Int(stage?.raw["width"]?.doubleValue ?? 0), Int(stage?.raw["height"]?.doubleValue ?? 0))
    }

    /// The layer as its field shows it: a word for the top and the bottom, else the number.
    static func layerText(_ layer: Int) -> String {
        switch layer {
        case StageLayer.top: return loc("stageEditor.layer.top")
        case StageLayer.bottom: return loc("stageEditor.layer.bottom")
        default: return String(layer)
        }
    }

    /// A layer typed: a whole number from the bottom to the top, or either word; nil otherwise.
    static func layer(typed text: String) -> Int? {
        let t = text.trimmingCharacters(in: .whitespaces)
        if t.caseInsensitiveCompare(loc("stageEditor.layer.top")) == .orderedSame { return StageLayer.top }
        if t.caseInsensitiveCompare(loc("stageEditor.layer.bottom")) == .orderedSame { return StageLayer.bottom }
        guard let n = Int(t), (StageLayer.bottom...StageLayer.top).contains(n) else { return nil }
        return n
    }

    /// Draws the window from the model: a field typed in and not committed keeps its text.
    func render() {
        guard let raw = stage?.raw else {
            // The stage is gone (removed here or by another client): its editor goes with it, and
            // an undo that brings the stage back does not reopen it.
            if seenStage, store.model != nil { closeGone() }
            return
        }
        seenStage = true
        let regions = self.regions
        if let id = selectWhenAdded, regions.contains(where: { $0.id == id }) {
            selected = id
            selectWhenAdded = nil
        }
        if selected == nil || !regions.contains(where: { $0.id == selected }) { selected = regions.first?.id }
        let (width, height) = stageSize
        let state = Drawn(
            name: raw["name"]?.stringValue ?? "", layer: Int(raw["layer"]?.doubleValue ?? 1),
            keepBlack: raw["keep_black"] == .bool(true), width: width, height: height, regions: regions,
            routes: routes.map { "\($0.id.raw)\u{0}\(VideoSettingsPane.routeTitle($0, outputs: store.videoOutputs))" },
            selected: selected, guides: guidesState, guided: !guidedRoutes.isEmpty, locked: aspectLock.state == .on)
        guard state != drawn else { return }
        let regionsChanged = drawn?.regions != state.regions || drawn?.routes != state.routes
        let sizeChanged = drawn?.width != width || drawn?.height != height
        drawn = state
        window?.title = String(format: loc("stageEditor.title"), state.name)
        guidesCheck.isEnabled = state.guided
        guidesCheck.state = state.guides
        show(nameField, state.name)
        show(layerField, Self.layerText(state.layer))
        layerStepper.integerValue = state.layer
        sizeLabel.stringValue = String(format: loc("settings.video.resolution"), String(width), String(height))
        blackCheck.state = state.keepBlack ? .on : .off
        aspectLock.image = (state.locked ? Icon.aspectLocked : Icon.aspectUnlocked).image(
            loc("stageEditor.aspectLock"), compact: true)
        if regionsChanged || regionRows.count != regions.count { buildRegionList(regions) }
        for (i, r) in regions.enumerated() {
            regionChips[i].isSelected = r.id == selected
            regionRows[i].isSelected = r.id == selected
            regionThumbnails[i].stageSize = NSSize(width: width, height: height)
            regionThumbnails[i].bounds_ = r.bounds
        }
        fillNewRegionPopup()
        let index = regions.firstIndex { $0.id == selected }
        let region = index.map { regions[$0] }
        let regionFields = [regionWidthField, regionHeightField, lowerLeftXField, lowerLeftYField]
        for f in regionFields { f.isEnabled = region != nil }
        aspectLock.isEnabled = region != nil
        if fieldsRegion != region?.id {
            // Another region (or none): what was typed for the old one is dropped, not committed.
            for f in regionFields {
                f.abortEditing()
                drawnText[ObjectIdentifier(f)] = nil
            }
            fieldsRegion = region?.id
        }
        if let region, let index {
            regionHeading.stringValue = String(format: loc("stageEditor.regionHeading"), RegionColour.letter(index))
            let corners = Self.corners(region.bounds, stageWidth: width, stageHeight: height)
            show(regionWidthField, String(region.bounds.width))
            show(regionHeightField, String(region.bounds.height))
            show(lowerLeftXField, Self.format(corners.lowerLeft.x))
            show(lowerLeftYField, Self.format(corners.lowerLeft.y))
            upperRightXLabel.stringValue = Self.format(corners.upperRight.x)
            upperRightYLabel.stringValue = Self.format(corners.upperRight.y)
        } else {
            // No region selected: every field shows a dash and takes nothing.
            regionHeading.stringValue = loc("stageEditor.noRegion")
            for f in regionFields { show(f, Self.dash) }
            upperRightXLabel.stringValue = Self.dash
            upperRightYLabel.stringValue = Self.dash
        }
        canvas.stageName = state.name
        canvas.stageSize = NSSize(width: width, height: height)
        canvas.regions = regions.enumerated().map { i, r in
            CanvasRegion(
                letter: RegionColour.letter(i), colour: RegionColour.of(i), route: routeName(r.route), bounds: r.bounds)
        }
        canvas.selected = index
        if sizeChanged { layoutCanvas() }
    }

    /// What an empty region field shows.
    static let dash = "—"

    /// Shows `text` in `field` unless the operator has typed something else there and not
    /// committed it.
    private func show(_ field: NSTextField, _ text: String) {
        let key = ObjectIdentifier(field)
        if let editor = field.currentEditor(), editor.string != drawnText[key] { return }
        field.stringValue = text
        field.currentEditor()?.string = text
        drawnText[key] = text
    }

    /// The region table's rows: letter, colour dot, thumbnail, route pop-up, remove.
    private func buildRegionList(_ regions: [RegionRow]) {
        for row in regionRows { row.removeFromSuperview() }
        regionRows = []
        regionLetters = []
        regionChips = []
        regionThumbnails = []
        regionRoutePopups = []
        regionRemoveButtons = []
        let (width, height) = stageSize
        for (i, r) in regions.enumerated() {
            let n = i + 1
            let letter = RegionColour.letter(i)
            let letterLabel = NSTextField(labelWithString: letter)
            letterLabel.font = .systemFont(ofSize: NSFont.smallSystemFontSize, weight: .semibold)
            letterLabel.alignment = .center
            letterLabel.widthAnchor.constraint(equalToConstant: 18).isActive = true
            letterLabel.setAX("\(Self.id)-region-letter-\(n)")
            let chip = RegionChip()
            chip.letter = letter
            chip.colour = RegionColour.of(i)
            chip.tag = i
            chip.target = self
            chip.action = #selector(chipClicked(_:))
            chip.setAX("\(Self.id)-region-\(n)", label: String(format: loc("stageEditor.region.ax"), letter))
            let thumbnail = RegionThumbnail()
            thumbnail.colour = RegionColour.of(i)
            thumbnail.stageSize = NSSize(width: width, height: height)
            thumbnail.bounds_ = r.bounds
            thumbnail.setAX(
                "\(Self.id)-region-thumbnail-\(n)", label: String(format: loc("stageEditor.thumbnail.ax"), letter))
            let popup = NSPopUpButton(frame: .zero, pullsDown: false)
            small(popup)
            let menu = NSMenu()
            let none = NSMenuItem(title: loc("stageEditor.noRoute"), action: nil, keyEquivalent: "")
            menu.addItem(none)
            if !routes.isEmpty { menu.addItem(.separator()) }
            var chosen = none
            for route in routes {
                let item = NSMenuItem(
                    title: VideoSettingsPane.routeTitle(route, outputs: store.videoOutputs), action: nil,
                    keyEquivalent: "")
                item.representedObject = route.id.raw
                menu.addItem(item)
                if route.id == r.route { chosen = item }
            }
            popup.menu = menu
            popup.select(chosen)
            popup.tag = i
            popup.identifier = NSUserInterfaceItemIdentifier(r.id.raw)
            popup.target = self
            popup.action = #selector(regionRouteChosen(_:))
            popup.setAX(
                "\(Self.id)-region-route-\(n)", label: String(format: loc("stageEditor.regionRoute.ax"), letter))
            popup.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
            popup.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(250), for: .horizontal)
            popup.widthAnchor.constraint(greaterThanOrEqualToConstant: 80).isActive = true
            let remove = NSButton(title: "", target: self, action: #selector(removeRegionClicked(_:)))
            remove.image = Icon.fieldClear.image(loc("settings.video.remove"), compact: true)
            remove.isBordered = false
            remove.imagePosition = .imageOnly
            remove.tag = i
            remove.identifier = NSUserInterfaceItemIdentifier(r.id.raw)
            remove.setAX(
                "\(Self.id)-region-remove-\(n)", label: String(format: loc("stageEditor.removeRegion.ax"), letter))
            let row = RegionRowView(views: [letterLabel, chip, thumbnail, popup, remove])
            row.tag = i
            row.onClick = { [weak self] in self?.select(index: i) }
            regionList.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: regionList.widthAnchor).isActive = true
            regionRows.append(row)
            regionLetters.append(letterLabel)
            regionChips.append(chip)
            regionThumbnails.append(thumbnail)
            regionRoutePopups.append(popup)
            regionRemoveButtons.append(remove)
        }
    }

    /// The new-region pop-up: a region on each route, or on none. Rebuilt only when its items
    /// change, so a pick is not dropped by a redraw that lands while the menu is open.
    private func fillNewRegionPopup() {
        let titles = routes.map { VideoSettingsPane.routeTitle($0, outputs: store.videoOutputs) }
        let key = zip(routes, titles).map { "\($0.id.raw)\u{0}\($1)" }
        guard key != shownNewRegionItems else { return }
        shownNewRegionItems = key
        let menu = NSMenu()
        menu.addItem(withTitle: loc("stageEditor.newRegion"), action: nil, keyEquivalent: "")
        for (route, title) in zip(routes, titles) {
            let item = NSMenuItem(
                title: String(format: loc("stageEditor.newRegionOn"), title), action: nil, keyEquivalent: "")
            item.representedObject = route.id.raw
            menu.addItem(item)
        }
        if !routes.isEmpty { menu.addItem(.separator()) }
        let none = NSMenuItem(title: loc("stageEditor.newRegionNone"), action: nil, keyEquivalent: "")
        none.representedObject = ""
        menu.addItem(none)
        newRegionPopup.menu = menu
    }

    /// The scale chosen at the foot: nil fits the stage to the canvas's room.
    var canvasScale: Int? {
        let p = scalePopup.selectedItem?.representedObject as? Int ?? 0
        return p > 0 ? p : nil
    }

    /// Sizes the canvas in its scroll view: the whole room when fitted, else the stage at the
    /// chosen scale (a stage pixel is that percent of a point) with its margin, scrolled when larger.
    func layoutCanvas() {
        let room = canvasScroll.contentView.bounds.size
        guard let p = canvasScale else {
            canvas.frame = NSRect(origin: .zero, size: room)
            return
        }
        let (w, h) = stageSize
        let m = StageCanvasView.margin * 2
        let size = NSSize(
            width: max(room.width, (CGFloat(w) * CGFloat(p) / 100).rounded() + m),
            height: max(room.height, (CGFloat(h) * CGFloat(p) / 100).rounded() + m))
        canvas.frame = NSRect(origin: .zero, size: size)
    }

    /// A region's lower-left and upper-right corners measured from the stage's centre, x to the
    /// right and y upwards (QLab 5 manual p. 229), in pixels (a half where the stage's size is odd).
    static func corners(_ b: RegionBounds, stageWidth: Int, stageHeight: Int) -> (
        lowerLeft: (x: Double, y: Double), upperRight: (x: Double, y: Double)
    ) {
        let cx = Double(stageWidth) / 2
        let cy = Double(stageHeight) / 2
        return (
            (Double(b.x) - cx, cy - Double(b.y + b.height)),
            (Double(b.x + b.width) - cx, cy - Double(b.y))
        )
    }

    /// A corner coordinate: whole numbers as such, a half with one decimal.
    static func format(_ v: Double) -> String {
        v == v.rounded() ? String(Int(v)) : String(format: "%.1f", v)
    }

    /// The stage went: the window closes and the settings window forgets it.
    private func closeGone() {
        seenStage = false
        resizeDialog?.cancel()
        for f in [nameField, layerField, regionWidthField, regionHeightField, lowerLeftXField, lowerLeftYField] {
            f.abortEditing()
        }
        close()
        onGone?()
    }

    // MARK: Selecting

    private func select(index: Int) {
        let regions = self.regions
        guard regions.indices.contains(index) else { return }
        endTyping()
        selected = regions[index].id
        render()
    }

    /// Ends the typing in a field, which commits it (before what the operator clicked next).
    /// True when typing ended: that edit's reason, if refused, must stand.
    @discardableResult
    private func endTyping() -> Bool {
        guard window?.firstResponder is NSText else { return false }
        window?.makeFirstResponder(nil)
        return true
    }

    @objc private func chipClicked(_ sender: RegionChip) { select(index: sender.tag) }

    @objc private func scaleChosen() {
        layoutCanvas()
        if canvasScale != nil {
            // The stage's centre in view, as a zoom does.
            let room = canvasScroll.contentView.bounds.size
            canvas.scroll(
                NSPoint(x: max(0, (canvas.frame.width - room.width) / 2), y: max(0, (canvas.frame.height - room.height) / 2)))
        }
    }

    @objc private func lockClicked() {
        endTyping()
        render()
    }

    // MARK: Editing

    /// Runs `edit` after the edits before it; a refusal shows why, and every field shows the
    /// model's value again. `typed` is true for a click that ended typing: the typed value's
    /// reason (shown now, or on its way from the engine) is not cleared by the click's edit.
    private func edit(after typed: Bool = false, _ body: @escaping @MainActor () async -> Result<OkBody, EngineError>) {
        if !typed { messageLabel.stringValue = "" }
        let previous = editing
        editing = Task { [weak self] in
            await previous?.value
            let result = await body()
            guard let self else { return }
            if case .failure(let e) = result {
                self.messageLabel.stringValue =
                    self.stage == nil ? loc("settings.video.gone") : e.detailText
            }
            self.drawn = nil
            self.render()
        }
    }

    /// Refuses a value here: says why and shows the model's values again.
    private func refuse(_ reason: String) {
        messageLabel.stringValue = reason
        drawn = nil
        render()
    }

    private func setStage(after typed: Bool = false, _ property: [String: JSONValue]) {
        let id = stageID
        edit(after: typed) { [store] in await store.send(command: .setStage(id, .object(property))) }
    }

    @objc private func blackClicked() {
        let on = blackCheck.state == .on
        let typed = endTyping()
        setStage(after: typed, ["type": .string("keep_black"), "value": .bool(on)])
    }

    @objc private func layerStepped() {
        let typed = endTyping()
        let layer = layerStepper.integerValue
        guard layer != drawn?.layer else { return }
        show(layerField, Self.layerText(layer))
        setStage(after: typed, ["type": .string("layer"), "value": .int(Int64(layer))])
    }

    /// Shows the guides on every route of the stage, unless all show them already: then hides
    /// them all (a mixed box shows the rest). Decided from what the routes show, whatever state
    /// the click left the box in; the box is then drawn from the engine.
    @objc private func guidesClicked() {
        let typed = endTyping()
        let routes = guidedRoutes
        let show = guidesState != .on
        guard !routes.isEmpty else { return }
        edit(after: typed) { [store] in await store.setRouteGuides(routes, show: show) }
    }

    /// Opens the resize dialog on the stage's size; its OK sends the new size and regions as one
    /// undo step, its Cancel nothing.
    @objc func resizeClicked() {
        let typed = endTyping()
        guard resizeDialog == nil else {
            resizeDialog?.window?.makeKeyAndOrderFront(nil)
            return
        }
        let (width, height) = stageSize
        let dialog = ResizeStageDialog(width: width, height: height, regions: regions)
        resizeDialog = dialog
        dialog.onDone = { [weak self] result in
            guard let self else { return }
            self.resizeDialog = nil
            guard let result else { return }
            let id = self.stageID
            let command: Command
            switch result.mode {
            case .scale:
                // The engine scales the regions with the stage (rounded, kept inside).
                command = .setStage(
                    id,
                    .object([
                        "type": .string("size"), "width": .int(Int64(result.width)),
                        "height": .int(Int64(result.height)),
                    ]))
            case .cropOrPad:
                command = .resizeStage(id, width: result.width, height: result.height, regions: result.regions)
            }
            self.edit(after: typed) { [store] in await store.send(command: command) }
        }
        if presentsDialogs, let parent = window {
            dialog.run(over: parent)
        }
    }

    @objc private func newRegionChosen(_ sender: NSPopUpButton) {
        guard let object = sender.selectedItem?.representedObject as? String else { return }
        let typed = endTyping()
        let (width, height) = stageSize
        let id = ModelID(UUID().uuidString.lowercased())
        let index = regions.count
        selectWhenAdded = id
        let stage = stageID
        let route = object.isEmpty ? nil : ModelID(object)
        // A new region covers the whole stage.
        edit(after: typed) { [store] in
            await store.send(
                command: .addRegion(
                    stage: stage, index: index, id: id, route: route,
                    bounds: RegionBounds(x: 0, y: 0, width: width, height: height)))
        }
    }

    @objc private func regionRouteChosen(_ sender: NSPopUpButton) {
        // The region the pop-up was drawn for, by the id it carries: a menu open while the regions
        // change still delivers its choice, and its tag may by then name another region.
        guard let region = regions.first(where: { $0.id.raw == sender.identifier?.rawValue }) else { return }
        let route = (sender.selectedItem?.representedObject as? String).map { ModelID($0) }
        let typed = endTyping()
        guard route != region.route else { return }
        let stage = stageID
        edit(after: typed) { [store] in
            await store.send(command: .setRegionRoute(stage: stage, region.id, route: route))
        }
    }

    @objc private func removeRegionClicked(_ sender: NSButton) {
        guard let region = regions.first(where: { $0.id.raw == sender.identifier?.rawValue }) else { return }
        let typed = endTyping()
        let stage = stageID
        edit(after: typed) { [store] in await store.send(command: .removeRegion(stage: stage, region.id)) }
    }

    func controlTextDidEndEditing(_ notification: Notification) {
        guard let field = notification.object as? NSTextField, stage != nil else { return }
        let key = ObjectIdentifier(field)
        let text = field.stringValue.trimmingCharacters(in: .whitespaces)
        let unchanged = field.stringValue == drawnText[key]
        // Committed: no longer typing, whatever the answer (the model's value shows next).
        drawnText[key] = field.stringValue
        guard !unchanged else { return }
        let (width, height) = stageSize
        switch field {
        case nameField:
            guard !text.isEmpty else { return refuse(loc("settings.video.badName")) }
            setStage(["type": .string("name"), "value": .string(field.stringValue)])
        case layerField:
            guard let layer = Self.layer(typed: text) else { return refuse(loc("settings.video.badLayer")) }
            setStage(["type": .string("layer"), "value": .int(Int64(layer))])
        case regionWidthField, regionHeightField, lowerLeftXField, lowerLeftYField:
            regionField(field, text: text, stageWidth: width, stageHeight: height)
        default:
            break
        }
    }

    /// A region's size or lower-left corner typed: the corner stays where it is for a size, the
    /// size for a corner. With the lock on, a width sets the height in the same shape and the
    /// other way round. Refused here when the region would not lie wholly inside the stage.
    private func regionField(_ field: NSTextField, text: String, stageWidth: Int, stageHeight: Int) {
        guard let region = regions.first(where: { $0.id == fieldsRegion }) else { return }
        var b = region.bounds
        let bottom = b.y + b.height
        let outside = String(format: loc("stageEditor.outside"), stageWidth, stageHeight)
        switch field {
        case regionWidthField, regionHeightField:
            // Any whole number of a pixel or more: `inside` says whether the region still fits, and
            // no sum here can overflow before it does.
            guard let side = Int(text), side >= 1, side <= MaxStagePixels.side else {
                return refuse(
                    Int(text).map { $0 >= 1 } == true ? outside : loc("stageEditor.badNumber"))
            }
            let locked = aspectLock.state == .on
            if field === regionWidthField {
                b.width = side
                if locked {
                    b.height = max(1, Int((Double(side) * Double(region.bounds.height) / Double(region.bounds.width)).rounded()))
                }
            } else {
                b.height = side
                if locked {
                    b.width = max(1, Int((Double(side) * Double(region.bounds.width) / Double(region.bounds.height)).rounded()))
                }
            }
            // The lower-left corner stays where it is.
            b.y = bottom - b.height
        default:
            guard let v = Double(text), v.isFinite, abs(v) <= 100_000 else {
                return refuse(loc("stageEditor.badNumber"))
            }
            if field === lowerLeftXField {
                b.x = Int((v + Double(stageWidth) / 2).rounded())
            } else {
                b.y = Int((Double(stageHeight) / 2 - v).rounded()) - b.height
            }
        }
        guard b.inside(width: stageWidth, height: stageHeight) else { return refuse(outside) }
        guard b != region.bounds else { return }
        let stage = stageID
        edit { [store] in await store.send(command: .setRegionBounds(stage: stage, region.id, b)) }
    }

    /// Return commits the field and keeps the window open; Esc drops what was typed.
    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        if selector == #selector(NSResponder.cancelOperation(_:)) {
            control.abortEditing()
            drawn = nil
            drawnText = [:]
            render()
            return true
        }
        guard selector == #selector(NSResponder.insertNewline(_:)) else { return false }
        window?.makeFirstResponder(nil)
        return true
    }
}

/// The largest stage side (docs/file-format.md §4.7).
enum MaxStagePixels {
    static let side = 16384
}

/// A row of the stage editor's region table: its controls in a line, the selected row drawn
/// highlighted; a click on it (not on a control) selects its region.
@MainActor
final class RegionRowView: NSStackView {
    var isSelected = false { didSet { needsDisplay = true } }
    var onClick: (() -> Void)?
    private var row = 0
    override var tag: Int {
        get { row }
        set { row = newValue }
    }

    convenience init(views: [NSView]) {
        self.init(frame: .zero)
        for v in views { addArrangedSubview(v) }
        orientation = .horizontal
        alignment = .centerY
        spacing = 8
        edgeInsets = NSEdgeInsets(top: 3, left: 6, bottom: 3, right: 6)
    }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        NSBezierPath.clip(bounds)
        if isSelected {
            NSColor.unemphasizedSelectedContentBackgroundColor.setFill()
            bounds.fill()
        }
    }

    override func mouseDown(with event: NSEvent) { onClick?() }

    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .row }
    override func isAccessibilitySelected() -> Bool { isSelected }
}

/// A small picture of the stage with one region lit in its colour (the region table's thumbnail).
@MainActor
final class RegionThumbnail: NSView {
    var stageSize = NSSize(width: 1920, height: 1080) { didSet { needsDisplay = true } }
    /// The region, in stage pixels from the top left.
    var bounds_ = RegionBounds(x: 0, y: 0, width: 1920, height: 1080) { didSet { needsDisplay = true } }
    var colour = NSColor.systemBlue { didSet { needsDisplay = true } }

    static let size = NSSize(width: 48, height: 28)

    override var intrinsicContentSize: NSSize { Self.size }
    nonisolated override var isFlipped: Bool { true }

    /// The stage's rectangle in the view: fitted, its shape kept, centred.
    var stageFrame: NSRect {
        guard stageSize.width > 0, stageSize.height > 0 else { return .zero }
        let scale = min(bounds.width / stageSize.width, bounds.height / stageSize.height)
        let w = stageSize.width * scale
        let h = stageSize.height * scale
        return NSRect(x: (bounds.width - w) / 2, y: (bounds.height - h) / 2, width: w, height: h)
    }

    /// The region's rectangle in the view.
    var regionFrame: NSRect {
        let s = stageFrame
        guard stageSize.width > 0, stageSize.height > 0 else { return .zero }
        let sx = s.width / stageSize.width
        let sy = s.height / stageSize.height
        return NSRect(
            x: s.minX + CGFloat(bounds_.x) * sx, y: s.minY + CGFloat(bounds_.y) * sy,
            width: CGFloat(bounds_.width) * sx, height: CGFloat(bounds_.height) * sy)
    }

    override func draw(_ dirtyRect: NSRect) {
        NSBezierPath.clip(bounds)
        let s = stageFrame
        NSColor(white: 0.9, alpha: 1).setFill()
        s.fill()
        colour.setFill()
        regionFrame.fill()
        NSColor(white: 0.45, alpha: 1).setStroke()
        let edge = NSBezierPath(rect: s.insetBy(dx: 0.5, dy: 0.5))
        edge.lineWidth = 1
        edge.stroke()
    }

    override func mouseDown(with event: NSEvent) { superview?.mouseDown(with: event) }

    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .image }
}
