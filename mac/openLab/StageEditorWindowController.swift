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

/// The stage editor (VID-01; QLab 5 manual pp. 227-229), one window per stage, opened by Edit on
/// the stage's row of the Video Outputs page. At the top the stage's name, layer, whether its
/// black stays up between cues, and its size. Below, its regions: a list (each region's letter on
/// its colour, the output route it is sent to, a remove button) with a pop-up that adds a region
/// on a route, and beside it the selected region's size and lower-left corner (measured from the
/// stage's centre, upwards) with its upper-right corner for reference. Under them the canvas
/// (`StageCanvasView`): the stage with its regions, a click selecting one.
///
/// It holds nothing the engine does not: every control is drawn from the model, every edit is one
/// engine command and one undo step; a value that would put a region outside the stage is refused
/// here with a message and the field shows the old value again.
@MainActor
final class StageEditorWindowController: NSWindowController, NSTextFieldDelegate, NSWindowDelegate {
    let store: WorkspaceStore
    let stageID: ModelID
    let nameField = NSTextField()
    let layerField = NSTextField()
    let blackCheck = NSButton(checkboxWithTitle: loc("settings.video.black"), target: nil, action: nil)
    let widthField = NSTextField()
    let heightField = NSTextField()
    /// The region list's rows, rebuilt on each change.
    let regionGrid = NSGridView(numberOfColumns: 3, rows: 0)
    private(set) var regionChips: [RegionChip] = []
    private(set) var regionRoutePopups: [NSPopUpButton] = []
    private(set) var regionRemoveButtons: [NSButton] = []
    let newRegionPopup = NSPopUpButton(frame: .zero, pullsDown: true)
    let regionWidthField = NSTextField()
    let regionHeightField = NSTextField()
    let lowerLeftXField = NSTextField()
    let lowerLeftYField = NSTextField()
    let upperRightLabel = NSTextField(labelWithString: "")
    let canvas = StageCanvasView()
    let messageLabel = NSTextField(wrappingLabelWithString: "")
    /// Says the stage is gone (removed; an undo brings it back here).
    let goneLabel = NSTextField(labelWithString: loc("settings.video.gone"))
    /// The selected region.
    private(set) var selected: ModelID?
    /// A region just added: selected once the model has it.
    private var selectWhenAdded: ModelID?
    private var subs: [AnyCancellable] = []
    /// Edits on their way, one after another (tests wait for them).
    private(set) var editing: Task<Void, Never>?
    /// What the controls were last drawn from.
    private var drawn: Drawn?
    /// Each field's text as last drawn or committed: a field still holding the caret with other
    /// text (typed, not committed) keeps it; one holding this text shows the model's value (an
    /// undo's, a refusal's), as after Return, which leaves the caret in the field.
    private var drawnText: [ObjectIdentifier: String] = [:]

    private struct Drawn: Equatable {
        var name: String
        var layer: Int
        var keepBlack: Bool
        var width: Int
        var height: Int
        var regions: [RegionRow]
        var routes: [String]
        var selected: ModelID?
    }

    static let minSize = NSSize(width: 600, height: 500)
    static let id = "stage-editor"

    init(store: WorkspaceStore, stage: ModelID) {
        self.store = store
        stageID = stage
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 720, height: 600),
            styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.identifier = NSUserInterfaceItemIdentifier(Self.id)
        window.isReleasedWhenClosed = false
        window.contentMinSize = Self.minSize
        super.init(window: window)
        window.delegate = self
        window.contentView = buildContent()
        window.center()
        subs.append(store.$model.sink { [weak self] _ in self?.renderSoon() })
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
        return l
    }

    private func number(_ field: NSTextField, _ id: String, _ label: String, width: CGFloat = 56) {
        field.setAX("\(Self.id)-\(id)", label: label)
        field.controlSize = .small
        field.font = .monospacedDigitSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
        field.alignment = .right
        field.delegate = self
        field.widthAnchor.constraint(equalToConstant: width).isActive = true
    }

    private func pair(_ a: NSTextField, _ sign: String, _ b: NSTextField) -> NSStackView {
        let by = NSTextField(labelWithString: sign)
        by.textColor = .secondaryLabelColor
        by.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        let s = NSStackView(views: [a, by, b])
        s.spacing = 4
        return s
    }

    private func buildContent() -> NSView {
        nameField.setAX("\(Self.id)-name", label: loc("stageEditor.name"))
        nameField.controlSize = .small
        nameField.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        nameField.delegate = self
        nameField.widthAnchor.constraint(equalToConstant: 240).isActive = true
        number(layerField, "layer", loc("settings.video.col.layer"), width: 44)
        number(widthField, "width", loc("settings.video.width"))
        number(heightField, "height", loc("settings.video.height"))
        blackCheck.setAX("\(Self.id)-black", label: loc("settings.video.black"))
        blackCheck.controlSize = .small
        blackCheck.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        blackCheck.target = self
        blackCheck.action = #selector(blackClicked)
        blackCheck.toolTip = loc("stageEditor.black.tip")
        let top = NSGridView(views: [
            [label("stageEditor.name"), nameField, label("stageEditor.layer"), layerField],
            [label("stageEditor.size"), pair(widthField, "×", heightField), NSGridCell.emptyContentView, blackCheck],
        ])
        top.rowAlignment = .firstBaseline
        top.rowSpacing = 8
        top.columnSpacing = 8
        top.column(at: 0).xPlacement = .trailing
        top.column(at: 2).xPlacement = .trailing
        top.cell(atColumnIndex: 3, rowIndex: 1).xPlacement = .leading
        // As wide as its fields, not the window.
        top.setContentHuggingPriority(.defaultHigh, for: .horizontal)

        regionGrid.rowSpacing = 4
        regionGrid.columnSpacing = 6
        regionGrid.yPlacement = .center
        regionGrid.column(at: 0).width = 24
        regionGrid.column(at: 2).width = 16
        regionGrid.setAX("\(Self.id)-regions")
        let listHolder = FlippedView()
        regionGrid.translatesAutoresizingMaskIntoConstraints = false
        listHolder.addSubview(regionGrid)
        let listScroll = NSScrollView()
        listScroll.hasVerticalScroller = true
        listScroll.autohidesScrollers = true
        listScroll.borderType = .bezelBorder
        listScroll.drawsBackground = false
        listHolder.translatesAutoresizingMaskIntoConstraints = false
        listScroll.documentView = listHolder
        NSLayoutConstraint.activate([
            listHolder.topAnchor.constraint(equalTo: listScroll.contentView.topAnchor),
            listHolder.leadingAnchor.constraint(equalTo: listScroll.contentView.leadingAnchor),
            listHolder.widthAnchor.constraint(equalTo: listScroll.contentView.widthAnchor),
            regionGrid.topAnchor.constraint(equalTo: listHolder.topAnchor, constant: 6),
            regionGrid.leadingAnchor.constraint(equalTo: listHolder.leadingAnchor, constant: 6),
            regionGrid.bottomAnchor.constraint(equalTo: listHolder.bottomAnchor, constant: -6),
            listScroll.widthAnchor.constraint(equalToConstant: 300),
            listScroll.heightAnchor.constraint(equalToConstant: 112),
        ])
        newRegionPopup.setAX("\(Self.id)-new-region", label: loc("stageEditor.newRegion"))
        newRegionPopup.controlSize = .small
        newRegionPopup.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        newRegionPopup.target = self
        newRegionPopup.action = #selector(newRegionChosen(_:))
        newRegionPopup.setContentHuggingPriority(.required, for: .horizontal)
        let list = NSStackView(views: [listScroll, newRegionPopup])
        list.orientation = .vertical
        list.alignment = .leading
        list.spacing = 6

        number(regionWidthField, "region-width", loc("stageEditor.regionWidth"))
        number(regionHeightField, "region-height", loc("stageEditor.regionHeight"))
        number(lowerLeftXField, "lower-left-x", loc("stageEditor.lowerLeftX"))
        number(lowerLeftYField, "lower-left-y", loc("stageEditor.lowerLeftY"))
        upperRightLabel.setAX("\(Self.id)-upper-right")
        upperRightLabel.font = .monospacedDigitSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
        upperRightLabel.textColor = .secondaryLabelColor
        let params = NSGridView(views: [
            [label("stageEditor.regionSize"), pair(regionWidthField, "×", regionHeightField)],
            [label("stageEditor.lowerLeft"), pair(lowerLeftXField, ",", lowerLeftYField)],
            [label("stageEditor.upperRight"), upperRightLabel],
        ])
        params.rowAlignment = .firstBaseline
        params.rowSpacing = 8
        params.columnSpacing = 8
        params.column(at: 0).xPlacement = .trailing
        let note = NSTextField(wrappingLabelWithString: loc("stageEditor.note"))
        note.font = .systemFont(ofSize: 11)
        note.textColor = .secondaryLabelColor
        note.preferredMaxLayoutWidth = 240
        let right = NSStackView(views: [params, note])
        right.orientation = .vertical
        right.alignment = .leading
        right.spacing = 10
        let middle = NSStackView(views: [list, right])
        middle.alignment = .top
        middle.spacing = 20

        canvas.setAX("\(Self.id)-canvas", label: loc("stageEditor.canvas"))
        canvas.onSelect = { [weak self] i in self?.select(index: i) }
        canvas.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        canvas.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        canvas.heightAnchor.constraint(greaterThanOrEqualToConstant: 160).isActive = true
        messageLabel.setAX("\(Self.id)-message")
        messageLabel.textColor = .systemRed
        goneLabel.setAX("\(Self.id)-gone")
        goneLabel.textColor = .systemOrange
        let divider = NSBox()
        divider.boxType = .separator
        let stack = NSStackView(views: [top, divider, middle, canvas, goneLabel, messageLabel])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        stack.edgeInsets = NSEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        for v in [divider, canvas] {
            v.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32).isActive = true
        }
        return stack
    }

    // MARK: Drawing

    private var stage: ModelObject? { store.model?.stages.first { $0.id == stageID } }
    private var routes: [ModelObject] { store.model?.routes ?? [] }

    /// The regions as the model holds them now.
    var regions: [RegionRow] { stage.map(RegionRow.of) ?? [] }

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

    /// Draws the window from the model: a field typed in and not committed keeps its text.
    func render() {
        let raw = stage?.raw
        let regions = self.regions
        if let id = selectWhenAdded, regions.contains(where: { $0.id == id }) {
            selected = id
            selectWhenAdded = nil
        }
        if selected == nil || !regions.contains(where: { $0.id == selected }) { selected = regions.first?.id }
        let (width, height) = stageSize
        let state = Drawn(
            name: raw?["name"]?.stringValue ?? "", layer: Int(raw?["layer"]?.doubleValue ?? 1),
            keepBlack: raw?["keep_black"] == .bool(true), width: width, height: height, regions: regions,
            routes: routes.map { "\($0.id.raw)\u{0}\($0.raw["name"]?.stringValue ?? "")" }, selected: selected)
        guard state != drawn else { return }
        let regionsChanged = drawn?.regions != state.regions || drawn?.routes != state.routes
        drawn = state
        let there = stage != nil
        goneLabel.isHidden = there
        window?.title = String(format: loc("stageEditor.title"), state.name)
        for control in [nameField, layerField, widthField, heightField] as [NSControl] { control.isEnabled = there }
        blackCheck.isEnabled = there
        newRegionPopup.isEnabled = there
        show(nameField, state.name)
        show(layerField, String(state.layer))
        show(widthField, String(width))
        show(heightField, String(height))
        blackCheck.state = state.keepBlack ? .on : .off
        if regionsChanged || regionChips.count != regions.count { buildRegionList(regions) }
        for (i, r) in regions.enumerated() { regionChips[i].isSelected = r.id == selected }
        fillNewRegionPopup()
        let index = regions.firstIndex { $0.id == selected }
        let region = index.map { regions[$0] }
        for f in [regionWidthField, regionHeightField, lowerLeftXField, lowerLeftYField] {
            f.isEnabled = region != nil
        }
        if let region {
            let corners = Self.corners(region.bounds, stageWidth: width, stageHeight: height)
            show(regionWidthField, String(region.bounds.width))
            show(regionHeightField, String(region.bounds.height))
            show(lowerLeftXField, Self.format(corners.lowerLeft.x))
            show(lowerLeftYField, Self.format(corners.lowerLeft.y))
            upperRightLabel.stringValue =
                "\(Self.format(corners.upperRight.x)), \(Self.format(corners.upperRight.y))"
        } else {
            for f in [regionWidthField, regionHeightField, lowerLeftXField, lowerLeftYField] { show(f, "") }
            upperRightLabel.stringValue = ""
        }
        canvas.stageSize = NSSize(width: width, height: height)
        canvas.regions = regions.enumerated().map { i, r in
            CanvasRegion(
                letter: RegionColour.letter(i), colour: RegionColour.of(i), route: routeName(r.route), bounds: r.bounds)
        }
        canvas.selected = index
    }

    /// Shows `text` in `field` unless the operator has typed something else there and not
    /// committed it.
    private func show(_ field: NSTextField, _ text: String) {
        let key = ObjectIdentifier(field)
        if let editor = field.currentEditor(), editor.string != drawnText[key] { return }
        field.stringValue = text
        field.currentEditor()?.string = text
        drawnText[key] = text
    }

    /// The region list's rows: letter on colour, route pop-up, remove.
    private func buildRegionList(_ regions: [RegionRow]) {
        while regionGrid.numberOfRows > 0 { regionGrid.removeRow(at: 0) }
        regionChips = []
        regionRoutePopups = []
        regionRemoveButtons = []
        for (i, r) in regions.enumerated() {
            let n = i + 1
            let letter = RegionColour.letter(i)
            let chip = RegionChip()
            chip.letter = letter
            chip.colour = RegionColour.of(i)
            chip.tag = i
            chip.target = self
            chip.action = #selector(chipClicked(_:))
            chip.setAX("\(Self.id)-region-\(n)", label: String(format: loc("stageEditor.region.ax"), letter))
            let popup = NSPopUpButton(frame: .zero, pullsDown: false)
            popup.controlSize = .small
            popup.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            let menu = NSMenu()
            var chosen: NSMenuItem?
            for route in routes {
                let item = NSMenuItem(title: route.raw["name"]?.stringValue ?? "", action: nil, keyEquivalent: "")
                item.representedObject = route.id.raw
                menu.addItem(item)
                if route.id == r.route { chosen = item }
            }
            if !routes.isEmpty { menu.addItem(.separator()) }
            let none = NSMenuItem(title: loc("stageEditor.noRoute"), action: nil, keyEquivalent: "")
            menu.addItem(none)
            popup.menu = menu
            popup.select(chosen ?? none)
            popup.tag = i
            popup.target = self
            popup.action = #selector(regionRouteChosen(_:))
            popup.setAX("\(Self.id)-region-route-\(n)", label: String(format: loc("stageEditor.regionRoute.ax"), letter))
            popup.widthAnchor.constraint(equalToConstant: 210).isActive = true
            let remove = NSButton(title: "", target: self, action: #selector(removeRegionClicked(_:)))
            remove.image = Icon.fieldClear.image(loc("settings.video.remove"), compact: true)
            remove.isBordered = false
            remove.imagePosition = .imageOnly
            remove.tag = i
            remove.setAX(
                "\(Self.id)-region-remove-\(n)", label: String(format: loc("stageEditor.removeRegion.ax"), letter))
            regionGrid.addRow(with: [chip, popup, remove])
            regionChips.append(chip)
            regionRoutePopups.append(popup)
            regionRemoveButtons.append(remove)
        }
    }

    /// The new-region pop-up: a region on each route, or on none.
    private func fillNewRegionPopup() {
        let menu = NSMenu()
        menu.addItem(withTitle: loc("stageEditor.newRegion"), action: nil, keyEquivalent: "")
        for route in routes {
            let item = NSMenuItem(
                title: String(format: loc("stageEditor.newRegionOn"), route.raw["name"]?.stringValue ?? ""),
                action: nil, keyEquivalent: "")
            item.representedObject = route.id.raw
            menu.addItem(item)
        }
        if !routes.isEmpty { menu.addItem(.separator()) }
        let none = NSMenuItem(title: loc("stageEditor.newRegionNone"), action: nil, keyEquivalent: "")
        none.representedObject = ""
        menu.addItem(none)
        newRegionPopup.menu = menu
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

    @objc private func newRegionChosen(_ sender: NSPopUpButton) {
        guard let object = sender.selectedItem?.representedObject as? String else { return }
        let typed = endTyping()
        let (width, height) = stageSize
        let id = ModelID(UUID().uuidString.lowercased())
        let index = regions.count
        selectWhenAdded = id
        let stage = stageID
        let route = object.isEmpty ? nil : ModelID(object)
        // A new region covers the whole stage (Q816).
        edit(after: typed) { [store] in
            await store.send(
                command: .addRegion(
                    stage: stage, index: index, id: id, route: route,
                    bounds: RegionBounds(x: 0, y: 0, width: width, height: height)))
        }
    }

    @objc private func regionRouteChosen(_ sender: NSPopUpButton) {
        let regions = self.regions
        guard regions.indices.contains(sender.tag) else { return }
        let route = (sender.selectedItem?.representedObject as? String).map { ModelID($0) }
        let typed = endTyping()
        let region = regions[sender.tag]
        guard route != region.route else { return }
        let stage = stageID
        edit(after: typed) { [store] in await store.send(command: .setRegionRoute(stage: stage, region.id, route: route)) }
    }

    @objc private func removeRegionClicked(_ sender: NSButton) {
        let regions = self.regions
        guard regions.indices.contains(sender.tag) else { return }
        let typed = endTyping()
        let region = regions[sender.tag]
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
            guard let layer = Int(text), (0...1000).contains(layer) else { return refuse(loc("settings.video.badLayer")) }
            setStage(["type": .string("layer"), "value": .int(Int64(layer))])
        case widthField, heightField:
            // A stage is 1 to 16384 pixels a side (QLab 5 manual p. 226); its regions follow it.
            guard let side = Int(text), (1...16384).contains(side) else { return refuse(loc("settings.video.badSize")) }
            let (w, h) = field === widthField ? (side, height) : (width, side)
            setStage(["type": .string("size"), "width": .int(Int64(w)), "height": .int(Int64(h))])
        case regionWidthField, regionHeightField, lowerLeftXField, lowerLeftYField:
            regionField(field, text: text, stageWidth: width, stageHeight: height)
        default:
            break
        }
    }

    /// A region's size or lower-left corner typed: the corner stays where it is for a size, the
    /// size for a corner. Refused here when the region would not lie wholly inside the stage.
    private func regionField(_ field: NSTextField, text: String, stageWidth: Int, stageHeight: Int) {
        guard let region = regions.first(where: { $0.id == selected }) else { return }
        var b = region.bounds
        let bottom = b.y + b.height
        switch field {
        case regionWidthField, regionHeightField:
            guard let side = Int(text), side >= 1 else { return refuse(loc("stageEditor.badNumber")) }
            if field === regionWidthField {
                b.width = side
            } else {
                b.height = side
                b.y = bottom - side
            }
        default:
            guard let v = Double(text), v.isFinite, abs(v) <= 100_000 else { return refuse(loc("stageEditor.badNumber")) }
            if field === lowerLeftXField {
                b.x = Int((v + Double(stageWidth) / 2).rounded())
            } else {
                b.y = Int((Double(stageHeight) / 2 - v).rounded()) - b.height
            }
        }
        guard b.inside(width: stageWidth, height: stageHeight) else {
            return refuse(String(format: loc("stageEditor.outside"), stageWidth, stageHeight))
        }
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
