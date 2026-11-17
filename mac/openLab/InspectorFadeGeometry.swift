// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// A Fade cue's picture values (VID-05; QLab 5 manual pp. 242-243): what it does to a Video target,
/// as the kind's `geometry` holds it, each its default when the kind lacks it (the engine's).
/// A value moves only while its box is ticked; an unticked one keeps its number for later.
struct FadeGeometryValues: Equatable {
    var fadeOpacity = false
    var opacity: Double = 1
    var fadeTranslation = false
    var translation = CGPoint.zero
    var fadeScale = false
    var scale = CGPoint(x: 1, y: 1)
    var scaleLocked = true
    var fadeRotation = false
    var rotation: Double = 0

    init() {}

    init(kind: JSONValue?) {
        guard let g = kind?["geometry"] else { return }
        func flag(_ key: String, _ fallback: Bool) -> Bool {
            if case .bool(let b)? = g[key] { return b }
            return fallback
        }
        func pair(_ key: String, _ fallback: CGPoint) -> CGPoint {
            CGPoint(x: g[key]?["x"]?.doubleValue ?? fallback.x, y: g[key]?["y"]?.doubleValue ?? fallback.y)
        }
        fadeOpacity = flag("fade_opacity", false)
        opacity = g["opacity"]?.doubleValue ?? 1
        fadeTranslation = flag("fade_translation", false)
        translation = pair("translation", .zero)
        fadeScale = flag("fade_scale", false)
        scale = pair("scale", CGPoint(x: 1, y: 1))
        scaleLocked = flag("scale_locked", true)
        fadeRotation = flag("fade_rotation", false)
        rotation = g["rotation"]?.doubleValue ?? 0
    }

    /// Where the Fade leaves a picture whose geometry is `from` (the engine's rule): absolute, at
    /// the ticked values; relative, opacity and scale times them and position and rotation plus
    /// them, each within its range (a multiplied value at 0 stays 0).
    @MainActor func applied(to from: VideoGeometry, relative: Bool) -> VideoGeometry {
        var g = from
        func times(_ a: Double, _ b: Double, _ r: ClosedRange<Double>) -> Double { a == 0 ? 0 : (a * b).clamped(to: r) }
        let scaleRange = GeometryControls.scaleRange.range
        let positionRange = GeometryControls.position.range
        if fadeOpacity { g.opacity = relative ? times(from.opacity, opacity, 0...1) : opacity }
        if fadeTranslation {
            g.translation =
                relative
                ? CGPoint(
                    x: (from.translation.x + translation.x).clamped(to: positionRange),
                    y: (from.translation.y + translation.y).clamped(to: positionRange))
                : translation
        }
        if fadeScale {
            g.scale =
                relative
                ? CGPoint(x: times(from.scale.x, scale.x, scaleRange), y: times(from.scale.y, scale.y, scaleRange))
                : scale
        }
        if fadeRotation {
            g.rotation =
                relative ? (from.rotation + rotation).clamped(to: GeometryControls.rotationRange.range) : rotation
        }
        return g
    }
}

/// The Fade cue's Geometry tab, laid out as QLab's (UI map `inspector.fade.geometry`): a column on
/// the left with the stop flag, how the values are read and Preview at its foot; a tick box per
/// value with its fields and a reset; the target's stage on the right with its picture where the
/// Fade leaves it. Each change goes out through `onEdit` as the parts of `geometry` it changed.
@MainActor
final class FadeGeometryPanel: NSObject, NSTextFieldDelegate {
    let view = NSView()
    let opacityCheck = NSButton(checkboxWithTitle: loc("inspector.geometry.opacity"), target: nil, action: nil)
    let translationCheck = NSButton(checkboxWithTitle: loc("inspector.geometry.position"), target: nil, action: nil)
    let scaleCheck = NSButton(checkboxWithTitle: loc("inspector.geometry.scale"), target: nil, action: nil)
    let rotationCheck = NSButton(checkboxWithTitle: loc("inspector.geometry.rotation"), target: nil, action: nil)
    let opacityField = GeometryField()
    let xField = GeometryField()
    let yField = GeometryField()
    let scaleXField = GeometryField()
    let scaleYField = GeometryField()
    let rotationField = GeometryField()
    let scaleLock = NSButton()
    let opacityReset = GeometryControls.resetButton("opacity")
    let positionReset = GeometryControls.resetButton("position")
    let scaleReset = GeometryControls.resetButton("scale")
    let rotationReset = GeometryControls.resetButton("rotation")
    let stopCheck = NSButton(checkboxWithTitle: loc("fade.stopTarget"), target: nil, action: nil)
    let modePopup = NSPopUpButton()
    let previewButton = NSButton(title: loc("inspector.preview"), target: nil, action: nil)
    let thumbnail = GeometryThumbnail()
    /// Says that a target filling its stage draws only the opacity of these values.
    let fillsStageNote = NSTextField(wrappingLabelWithString: loc("inspector.fadeGeometry.fillsStage"))

    /// Sends the parts of `geometry` an edit changed.
    var onEdit: (([String: JSONValue]) -> Void)?
    /// The stop flag or the mode changed (as on the Curve and Levels tabs).
    var onChange: ((FadeSettingsPanel.Change) -> Void)?
    /// Says why a typed value was not taken.
    var onRefuse: ((String) -> Void)?
    private(set) var shown = FadeGeometryValues()
    private var relative = false
    /// The field being typed in: a redraw leaves its text alone.
    private var typing: NSTextField?
    var isLocked = false { didSet { applyLock() } }

    var checks: [NSButton] { [opacityCheck, translationCheck, scaleCheck, rotationCheck] }
    var numberFields: [NSTextField] { [opacityField, xField, yField, scaleXField, scaleYField, rotationField] }

    override init() {
        super.init()
        let ids: [(NSControl, String, String)] = [
            (opacityCheck, "fade-geometry-opacity-tick", "inspector.fadeGeometry.tick.opacity"),
            (translationCheck, "fade-geometry-position-tick", "inspector.fadeGeometry.tick.position"),
            (scaleCheck, "fade-geometry-scale-tick", "inspector.fadeGeometry.tick.scale"),
            (rotationCheck, "fade-geometry-rotation-tick", "inspector.fadeGeometry.tick.rotation"),
            (opacityField, "fade-geometry-opacity", "inspector.geometry.field.opacity"),
            (xField, "fade-geometry-x", "inspector.geometry.field.x"),
            (yField, "fade-geometry-y", "inspector.geometry.field.y"),
            (scaleXField, "fade-geometry-scale-x", "inspector.geometry.field.scale-x"),
            (scaleYField, "fade-geometry-scale-y", "inspector.geometry.field.scale-y"),
            (rotationField, "fade-geometry-rotation", "inspector.geometry.field.rotation"),
            (scaleLock, "fade-geometry-scale-lock", "inspector.geometry.scaleLock"),
            (stopCheck, "fade-geometry-stop-target", "fade.stopTarget"),
            (modePopup, "fade-geometry-mode", "fade.mode"),
            (previewButton, "fade-geometry-preview", "inspector.preview"),
        ]
        for (c, id, label) in ids {
            c.setAX(id, label: loc(label))
            c.controlSize = .small
            c.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        }
        for (f, key) in zip(numberFields, ["opacity", "x", "y", "scale-x", "scale-y", "rotation"]) {
            f.toolTip = loc("inspector.geometry.field.\(key)")
            f.delegate = self
            f.alignment = .right
            f.font = .monospacedDigitSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
            f.widthAnchor.constraint(equalToConstant: 50).isActive = true
            f.setContentCompressionResistancePriority(.required, for: .horizontal)
        }
        // The reset buttons are told apart from the Video cue's own.
        for (b, what) in [
            (opacityReset, "opacity"), (positionReset, "position"), (scaleReset, "scale"),
            (rotationReset, "rotation"),
        ] {
            b.setAX("fade-geometry-\(what)-reset", label: loc("inspector.geometry.reset.\(what)"))
        }
        scaleLock.bezelStyle = .inline
        scaleLock.isBordered = false
        scaleLock.imagePosition = .imageOnly
        scaleLock.setButtonType(.toggle)
        scaleLock.toolTip = loc("inspector.geometry.scaleLock")
        for key in FadeSettingsPanel.modes {
            modePopup.menu?.addItem(NSMenuItem(title: loc("fade.mode.\(key)"), action: nil, keyEquivalent: ""))
        }
        thumbnail.setAX("fade-geometry-thumbnail", label: loc("inspector.fadeGeometry.thumbnail"))
        thumbnail.setAccessibilityRole(.image)
        // Shows where the Fade leaves the picture; never dragged.
        thumbnail.isLocked = true
        for c in checks + [scaleLock, opacityReset, positionReset, scaleReset, rotationReset] as [NSControl] {
            c.target = self
            c.action = #selector(chosen(_:))
        }
        stopCheck.target = self
        stopCheck.action = #selector(stopChanged)
        modePopup.target = self
        modePopup.action = #selector(modeChanged)
        layOut()
    }

    private func layOut() {
        func line(_ views: [NSView]) -> NSStackView {
            let s = NSStackView(views: views)
            s.spacing = 4
            return s
        }
        let percent = NSTextField(labelWithString: loc("inspector.geometry.percent"))
        percent.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        percent.textColor = .secondaryLabelColor
        let grid = NSGridView(views: [
            [opacityCheck, line([opacityField, percent, opacityReset])],
            [translationCheck, line([xField, yField, positionReset])],
            [scaleCheck, line([scaleXField, scaleLock, scaleYField, scaleReset])],
            [rotationCheck, line([rotationField, rotationReset])],
        ])
        grid.rowSpacing = 8
        grid.columnSpacing = 8
        grid.rowAlignment = .firstBaseline
        grid.setContentHuggingPriority(.required, for: .horizontal)
        grid.setContentCompressionResistancePriority(.required, for: .horizontal)
        // The foot of the left column, as on the Levels tab: the mode and Preview side by side.
        let foot = NSStackView(views: [modePopup, previewButton])
        foot.spacing = 6
        foot.distribution = .fillEqually
        let spacer = NSView()
        spacer.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .vertical)
        let side = NSStackView(views: [spacer, stopCheck, foot])
        side.orientation = .vertical
        side.alignment = .leading
        side.spacing = 8
        let fields = GeometryFieldsView()
        grid.translatesAutoresizingMaskIntoConstraints = false
        fields.addSubview(grid)
        fillsStageNote.setAX("fade-geometry-fills-stage")
        fillsStageNote.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        fillsStageNote.textColor = .secondaryLabelColor
        fillsStageNote.translatesAutoresizingMaskIntoConstraints = false
        // Wraps within the fields' width; never widens them.
        fillsStageNote.preferredMaxLayoutWidth = 280
        fillsStageNote.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        fields.addSubview(fillsStageNote)
        let scroll = NSScrollView()
        scroll.setAX("fade-geometry-scroll")
        scroll.documentView = fields
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        scroll.borderType = .noBorder
        fields.translatesAutoresizingMaskIntoConstraints = false
        for v in [side, scroll, thumbnail] as [NSView] {
            v.translatesAutoresizingMaskIntoConstraints = false
            view.addSubview(v)
        }
        let clip = scroll.contentView
        // As wide as the fields need when there is room, so the thumbnail takes the rest.
        let fit = scroll.widthAnchor.constraint(equalTo: fields.widthAnchor)
        fit.priority = .defaultHigh
        thumbnail.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        thumbnail.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        thumbnail.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .vertical)
        NSLayoutConstraint.activate([
            side.topAnchor.constraint(equalTo: view.topAnchor, constant: 8),
            side.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -8),
            side.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 12),
            side.widthAnchor.constraint(equalToConstant: LevelsPanel.sideWidth),
            foot.widthAnchor.constraint(equalTo: side.widthAnchor),
            fields.topAnchor.constraint(equalTo: clip.topAnchor),
            fields.leadingAnchor.constraint(equalTo: clip.leadingAnchor),
            grid.topAnchor.constraint(equalTo: fields.topAnchor, constant: 10),
            grid.leadingAnchor.constraint(equalTo: fields.leadingAnchor, constant: 4),
            grid.trailingAnchor.constraint(equalTo: fields.trailingAnchor, constant: -4),
            fillsStageNote.topAnchor.constraint(equalTo: grid.bottomAnchor, constant: 10),
            fillsStageNote.leadingAnchor.constraint(equalTo: grid.leadingAnchor),
            fillsStageNote.trailingAnchor.constraint(equalTo: grid.trailingAnchor),
            fillsStageNote.bottomAnchor.constraint(lessThanOrEqualTo: fields.bottomAnchor, constant: -8),
            scroll.topAnchor.constraint(equalTo: view.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: side.trailingAnchor, constant: 12),
            scroll.widthAnchor.constraint(lessThanOrEqualTo: fields.widthAnchor),
            fit,
            thumbnail.topAnchor.constraint(equalTo: view.topAnchor, constant: 8),
            thumbnail.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -8),
            thumbnail.leadingAnchor.constraint(equalTo: scroll.trailingAnchor, constant: 16),
            thumbnail.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -12),
            thumbnail.widthAnchor.constraint(greaterThanOrEqualToConstant: GeometryControls.thumbnailLeast),
        ])
    }

    // MARK: Showing

    /// Shows a Fade's `kind` aimed at a picture of geometry `target` (its own; nil: no Video target)
    /// on a stage of `stage` pixels (nil: none) whose file is `picture` pixels (nil: not probed). A
    /// field being typed in keeps its text.
    func show(kind: JSONValue, target: VideoGeometry?, stage: CGSize?, picture: CGSize?) {
        let v = FadeGeometryValues(kind: kind)
        shown = v
        relative = kind["mode"]?.stringValue == "relative"
        for (check, on) in zip(checks, [v.fadeOpacity, v.fadeTranslation, v.fadeScale, v.fadeRotation]) {
            check.state = on ? .on : .off
        }
        let texts: [(NSTextField, String)] = [
            (opacityField, GeometryControls.number(v.opacity * 100)),
            (xField, GeometryControls.number(v.translation.x)), (yField, GeometryControls.number(v.translation.y)),
            (scaleXField, GeometryControls.number(v.scale.x)), (scaleYField, GeometryControls.number(v.scale.y)),
            (rotationField, GeometryControls.number(v.rotation)),
        ]
        for (f, text) in texts where f !== typing && f.stringValue != text { f.stringValue = text }
        // An unticked value is kept but dimmed: the Fade does not move it.
        let ticked: [NSTextField: Bool] = [
            opacityField: v.fadeOpacity, xField: v.fadeTranslation, yField: v.fadeTranslation,
            scaleXField: v.fadeScale, scaleYField: v.fadeScale, rotationField: v.fadeRotation,
        ]
        for f in numberFields { f.textColor = ticked[f] == true ? .controlTextColor : .secondaryLabelColor }
        scaleLock.state = v.scaleLocked ? .on : .off
        scaleLock.image = NSImage(
            systemSymbolName: v.scaleLocked ? "lock" : "lock.open",
            accessibilityDescription: loc("inspector.geometry.scaleLock"))
        stopCheck.state = kind["stop_target_when_done"] == .bool(true) ? .on : .off
        modePopup.selectItem(at: FadeSettingsPanel.modes.firstIndex(of: kind["mode"]?.stringValue ?? "") ?? 0)
        thumbnail.stage = stage ?? CGSize(width: 1920, height: 1080)
        thumbnail.hasStage = stage != nil
        thumbnail.picture = picture
        thumbnail.geometry = v.applied(to: target ?? VideoGeometry(), relative: relative)
        // A target filling its stage ignores position, scale and rotation, as an edit of them.
        fillsStageNote.isHidden = target?.fillStage != true
        applyLock()
    }

    private func applyLock() {
        for c in checks + numberFields + [scaleLock, opacityReset, positionReset, scaleReset, rotationReset]
            + [stopCheck, modePopup] as [NSControl]
        {
            c.isEnabled = !isLocked
        }
    }

    // MARK: Editing

    func controlTextDidBeginEditing(_ obj: Notification) { typing = obj.object as? NSTextField }

    func controlTextDidEndEditing(_ obj: Notification) {
        guard let f = obj.object as? NSTextField else { return }
        if typing === f { typing = nil }
        commit(f)
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard selector == #selector(NSResponder.cancelOperation(_:)), let f = control as? NSTextField else {
            return control.endTyping(by: selector)
        }
        // Esc puts the shown value back.
        typing = nil
        redraw()
        f.currentEditor()?.selectAll(nil)
        return true
    }

    private func redraw() {
        let texts: [(NSTextField, Double)] = [
            (opacityField, shown.opacity * 100), (xField, shown.translation.x), (yField, shown.translation.y),
            (scaleXField, shown.scale.x), (scaleYField, shown.scale.y), (rotationField, shown.rotation),
        ]
        for (f, v) in texts { f.stringValue = GeometryControls.number(v) }
    }

    /// The range a field takes, as the engine enforces it (the Video cue's ranges).
    func range(of f: NSTextField) -> GeometryControls.FieldRange? {
        switch f {
        case opacityField: return GeometryControls.opacityRange
        case xField, yField: return GeometryControls.position
        case scaleXField, scaleYField: return GeometryControls.scaleRange
        case rotationField: return GeometryControls.rotationRange
        default: return nil
        }
    }

    /// Sends what `f` holds when it differs from what is shown; text that is not a number, or a
    /// number outside the field's range, is not sent and the shown value comes back.
    func commit(_ f: NSTextField) {
        guard !isLocked else { return }
        let text = f.stringValue.trimmingCharacters(in: .whitespaces)
        let g = shown
        guard let v = Double(text), v.isFinite else {
            if !text.isEmpty { onRefuse?(loc("inspector.geometry.notANumber")) }
            redraw()
            return
        }
        if let range = range(of: f), !range.allows(v) {
            onRefuse?(loc(range.message))
            redraw()
            return
        }
        func pair(_ p: CGPoint) -> JSONValue { VideoGeometry.pairJSON(p) }
        var parts: [String: JSONValue] = [:]
        switch f {
        // Whole percents, as QLab takes them (p. 243).
        case opacityField where v.rounded() / 100 != g.opacity: parts["opacity"] = .double(v.rounded() / 100)
        case xField where v != g.translation.x: parts["translation"] = .object(["x": .double(v)])
        case yField where v != g.translation.y: parts["translation"] = .object(["y": .double(v)])
        case scaleXField where v != g.scale.x || (g.scaleLocked && v != g.scale.y):
            parts["scale"] = g.scaleLocked ? pair(CGPoint(x: v, y: v)) : .object(["x": .double(v)])
        case scaleYField where v != g.scale.y || (g.scaleLocked && v != g.scale.x):
            parts["scale"] = g.scaleLocked ? pair(CGPoint(x: v, y: v)) : .object(["y": .double(v)])
        case rotationField where v != g.rotation: parts["rotation"] = .double(v)
        default: break
        }
        if parts.isEmpty { return redraw() }
        onEdit?(parts)
    }

    @objc func chosen(_ sender: NSControl) {
        guard !isLocked else { return }
        let g = shown
        let d = FadeGeometryValues()
        var parts: [String: JSONValue] = [:]
        switch sender {
        case opacityCheck: parts["fade_opacity"] = .bool(opacityCheck.state == .on)
        case translationCheck: parts["fade_translation"] = .bool(translationCheck.state == .on)
        case scaleCheck: parts["fade_scale"] = .bool(scaleCheck.state == .on)
        case rotationCheck: parts["fade_rotation"] = .bool(rotationCheck.state == .on)
        case scaleLock: parts["scale_locked"] = .bool(!g.scaleLocked)
        case opacityReset where g.opacity != d.opacity: parts["opacity"] = .double(d.opacity)
        case positionReset where g.translation != d.translation:
            parts["translation"] = VideoGeometry.pairJSON(d.translation)
        case scaleReset where g.scale != d.scale: parts["scale"] = VideoGeometry.pairJSON(d.scale)
        case rotationReset where g.rotation != d.rotation: parts["rotation"] = .double(d.rotation)
        default: break
        }
        if parts.isEmpty { return }
        onEdit?(parts)
    }

    @objc private func stopChanged() {
        guard !isLocked else { return }
        onChange?(.stopTarget(stopCheck.state == .on))
    }

    @objc private func modeChanged() {
        let i = modePopup.indexOfSelectedItem
        guard !isLocked, FadeSettingsPanel.modes.indices.contains(i) else { return }
        onChange?(.mode(FadeSettingsPanel.modes[i]))
    }
}

extension InspectorViewController {
    /// The Fade cue's Geometry tab (`fadeGeometry`), shown for a Fade aimed at a Video cue or at
    /// nothing yet (as QLab shows every tab a Fade can have until it has a target).
    func fadeGeometryTab() -> NSTabViewItem {
        let p = fadeGeometry
        p.onEdit = { [weak self] parts in self?.sendFadeGeometry(parts) }
        p.onChange = { [weak self] change in self?.sendFade(change) }
        p.onRefuse = { [weak self] message in self?.showRefusal(message) }
        p.previewButton.target = self
        p.previewButton.action = #selector(previewClicked)
        let item = NSTabViewItem(identifier: "fadeGeometry")
        item.label = loc("inspector.tab.geometry")
        item.view = p.view
        return item
    }

    /// A Fade cue's picture values, with its target's stage and picture drawn where the Fade leaves
    /// it (the target's own values where nothing is ticked).
    func renderFadeGeometry(_ cue: Cue, model: WorkspaceModel) {
        guard cue.kindType == "fade" else { return }
        let kind = cue.raw["kind"] ?? .null
        let target = kind["target"]?.stringValue.flatMap { model.cue(ModelID($0)) }.flatMap {
            $0.kindType == "video" ? $0 : nil
        }
        let stageID = target?.raw["kind"]?["stage"]?.stringValue
        let stage = model.stages.first { $0.id.raw == stageID }.flatMap { s -> CGSize? in
            guard let w = s.raw["width"]?.doubleValue, let h = s.raw["height"]?.doubleValue else { return nil }
            return CGSize(width: w, height: h)
        }
        let picture = target.flatMap { store.mediaDurations[$0.id]?.picture }.map {
            CGSize(width: $0.width, height: $0.height)
        }
        fadeGeometry.show(
            kind: kind, target: target.map { VideoGeometry(kind: $0.raw["kind"]) }, stage: stage, picture: picture)
        fadeGeometry.isLocked = isLocked
    }

    /// Sends the parts of the shown Fade's `geometry` an edit changed: one undo step each.
    func sendFadeGeometry(_ parts: [String: JSONValue]) {
        guard let cue = cueForKindEdit, cue.kindType == "fade" else { return }
        messageLabel.stringValue = ""
        send(
            .setProperty(cue.id, .kindFields(["geometry": .object(parts)])), cue: cue.id,
            key: "fadeGeometry.\(parts.keys.sorted().joined(separator: ","))", revert: [],
            refusedAs: loc("inspector.geometry.refused"))
        // Shown with the edit before the engine answers, so the next field typed builds on it.
        if let cue = cueForKindEdit, let model = store.model { renderFadeGeometry(cue, model: model) }
    }
}
