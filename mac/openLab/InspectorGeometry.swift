// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// A Video cue's geometry as the inspector shows it (VID-04; QLab 5 manual pp. 212-214): the
/// cue's kind fields, each its default when the kind lacks it (the engine's defaults).
struct VideoGeometry: Equatable {
    var fillStage = true
    var fillStyle = "fit"
    var translation = CGPoint.zero
    var scale = CGPoint(x: 1, y: 1)
    var scaleLocked = true
    var rotation: Double = 0
    /// Top, bottom, left, right, in picture pixels.
    var crop: [Double] = [0, 0, 0, 0]
    var anchor = CGPoint.zero
    var opacity: Double = 1
    var layer: Double = 1000
    var smooth = true

    static let styles = ["fit", "fill", "stretch"]
    /// Kind fields an edit may give only some parts of: each typed part goes alone, so two typed
    /// before the engine answers both hold (core's `merge_kind_fields`). `geometry` is a Fade
    /// cue's picture values.
    static let partialFields: Set<String> = ["translation", "scale", "anchor", "crop", "geometry"]
    static let cropEdges = ["top", "bottom", "left", "right"]

    init() {}

    init(kind: JSONValue?) {
        guard let k = kind else { return }
        func pair(_ key: String, _ fallback: CGPoint) -> CGPoint {
            guard let x = k[key]?["x"]?.doubleValue, let y = k[key]?["y"]?.doubleValue else { return fallback }
            return CGPoint(x: x, y: y)
        }
        if case .bool(let b)? = k["fill_stage"] { fillStage = b }
        if let s = k["fill_style"]?.stringValue { fillStyle = s }
        translation = pair("translation", .zero)
        scale = pair("scale", CGPoint(x: 1, y: 1))
        if case .bool(let b)? = k["scale_locked"] { scaleLocked = b }
        rotation = k["rotation"]?.doubleValue ?? 0
        crop = Self.cropEdges.map { k["crop"]?[$0]?.doubleValue ?? 0 }
        anchor = pair("anchor", .zero)
        opacity = k["opacity"]?.doubleValue ?? 1
        layer = k["layer"]?.doubleValue ?? 1000
        if case .bool(let b)? = k["smooth"] { smooth = b }
    }

    static func pairJSON(_ p: CGPoint) -> JSONValue { .object(["x": .double(p.x), "y": .double(p.y)]) }

    static func cropJSON(_ c: [Double]) -> JSONValue {
        .object(Dictionary(uniqueKeysWithValues: zip(cropEdges, c.map { JSONValue.double($0) })))
    }

    /// Where the picture's corners fall on the stage (stage pixels from its top left, y down: top
    /// left, top right, bottom left, bottom right), as the stage host draws it; nil when nothing
    /// of it is drawn. The same rules as the engine's.
    func corners(picture: CGSize, stage: CGSize) -> [CGPoint]? {
        let (pw, ph, sw, sh) = (picture.width, picture.height, stage.width, stage.height)
        guard pw > 0, ph > 0, sw > 0, sh > 0 else { return nil }
        if fillStage {
            var (w, h) = (sw, sh)
            if fillStyle != "stretch" {
                let s = fillStyle == "fill" ? max(sw / pw, sh / ph) : min(sw / pw, sh / ph)
                (w, h) = (pw * s, ph * s)
            }
            let (x0, y0) = ((sw - w) / 2, (sh - h) / 2)
            return [
                CGPoint(x: x0, y: y0), CGPoint(x: x0 + w, y: y0), CGPoint(x: x0, y: y0 + h),
                CGPoint(x: x0 + w, y: y0 + h),
            ]
        }
        let (left, right) = (-pw / 2 + max(crop[2], 0), pw / 2 - max(crop[3], 0))
        let (bottom, top) = (-ph / 2 + max(crop[1], 0), ph / 2 - max(crop[0], 0))
        guard right > left, top > bottom else { return nil }
        let radians = rotation * .pi / 180
        let (sinR, cosR) = (sin(radians), cos(radians))
        func place(_ x: Double, _ y: Double) -> CGPoint {
            let (dx, dy) = ((x - anchor.x) * scale.x, (y - anchor.y) * scale.y)
            let (rx, ry) = (dx * cosR - dy * sinR, dx * sinR + dy * cosR)
            return CGPoint(
                x: sw / 2 + rx + anchor.x + translation.x, y: sh / 2 - (ry + anchor.y + translation.y))
        }
        return [place(left, top), place(right, top), place(left, bottom), place(right, bottom)]
    }

    /// Where the anchor falls on the stage (stage pixels from its top left, y down).
    func anchorPoint(stage: CGSize) -> CGPoint {
        CGPoint(
            x: stage.width / 2 + anchor.x + translation.x, y: stage.height / 2 - (anchor.y + translation.y))
    }
}

extension Comparable {
    func clamped(to range: ClosedRange<Self>) -> Self { min(max(self, range.lowerBound), range.upperBound) }
}

/// The thumbnail: the stage's outline and the picture placed on it, as the stage draws it. A drag
/// moves the picture (custom mode); the picture is drawn as its shape, never decoded here.
final class GeometryThumbnail: NSView {
    var geometry = VideoGeometry() { didSet { needsDisplay = true } }
    var stage = CGSize(width: 1920, height: 1080) { didSet { needsDisplay = true } }
    var picture: CGSize? { didSet { needsDisplay = true } }
    /// The cue is on a stage (false: on none, and the thumbnail says so instead of drawing one).
    var hasStage = true { didSet { needsDisplay = true } }
    var isLocked = false
    /// A drag moved the picture to this position (stage pixels); `ended` once at its release.
    var onDrag: ((CGPoint, _ ended: Bool) -> Void)?
    /// A drag begins (true) or ends (false, after its last `onDrag`).
    var onHold: ((Bool) -> Void)?
    private var dragFrom: (mouse: NSPoint, translation: CGPoint)?

    nonisolated override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { false }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    /// The stage drawn inside the view: shape kept, a margin round it so a picture moved off it
    /// can still be seen.
    var stageRect: NSRect {
        let inner = bounds.insetBy(dx: 12, dy: 12)
        guard stage.width > 0, stage.height > 0, inner.width > 0, inner.height > 0 else { return .zero }
        let s = min(inner.width / stage.width, inner.height / stage.height) * 0.8
        let (w, h) = (stage.width * s, stage.height * s)
        return NSRect(x: bounds.midX - w / 2, y: bounds.midY - h / 2, width: w, height: h)
    }

    /// The black the stage is drawn on: the largest box of the stage's shape in the view, centred,
    /// so the preview keeps the stage's shape however tall or narrow the view is (the rest of the
    /// view is left clear).
    var backdropRect: NSRect {
        guard stage.width > 0, stage.height > 0, bounds.width > 0, bounds.height > 0 else { return .zero }
        let s = min(bounds.width / stage.width, bounds.height / stage.height)
        let (w, h) = (stage.width * s, stage.height * s)
        return NSRect(x: bounds.midX - w / 2, y: bounds.midY - h / 2, width: w, height: h)
    }

    /// View points per stage pixel.
    private var pointsPerPixel: CGFloat {
        stage.width > 0 ? stageRect.width / stage.width : 0
    }

    private func viewPoint(_ p: CGPoint) -> NSPoint {
        let r = stageRect
        return NSPoint(x: r.minX + p.x * pointsPerPixel, y: r.minY + p.y * pointsPerPixel)
    }

    override func draw(_ dirtyRect: NSRect) {
        NSBezierPath.clip(bounds)
        NSColor.black.setFill()
        guard hasStage else {
            bounds.fill()
            let text = NSAttributedString(
                string: loc("inspector.geometry.noStage"),
                attributes: [
                    .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize),
                    .foregroundColor: NSColor.secondaryLabelColor,
                ])
            let size = text.size()
            text.draw(at: NSPoint(x: bounds.midX - size.width / 2, y: bounds.midY - size.height / 2))
            return
        }
        let backdrop = backdropRect
        backdrop.fill()
        NSBezierPath.clip(backdrop)
        let stageBox = stageRect
        NSColor(white: 0.12, alpha: 1).setFill()
        stageBox.fill()
        if let picture, let corners = geometry.corners(picture: picture, stage: stage) {
            let pts = corners.map(viewPoint)
            let shape = NSBezierPath()
            shape.move(to: pts[0])
            shape.line(to: pts[1])
            shape.line(to: pts[3])
            shape.line(to: pts[2])
            shape.close()
            let tint = NSColor.systemYellow
            // Faint where it falls off the stage, stronger on it, both by its opacity.
            tint.withAlphaComponent(0.25 * max(geometry.opacity, 0.15)).setFill()
            shape.fill()
            NSGraphicsContext.saveGraphicsState()
            NSBezierPath.clip(stageBox)
            tint.withAlphaComponent(0.55 * max(geometry.opacity, 0.15)).setFill()
            shape.fill()
            NSGraphicsContext.restoreGraphicsState()
            tint.setStroke()
            shape.lineWidth = 1
            shape.stroke()
            if !geometry.fillStage {
                let a = viewPoint(geometry.anchorPoint(stage: stage))
                let cross = NSBezierPath()
                cross.move(to: NSPoint(x: a.x - 5, y: a.y))
                cross.line(to: NSPoint(x: a.x + 5, y: a.y))
                cross.move(to: NSPoint(x: a.x, y: a.y - 5))
                cross.line(to: NSPoint(x: a.x, y: a.y + 5))
                NSColor.systemTeal.setStroke()
                cross.lineWidth = 1.5
                cross.stroke()
            }
        }
        NSColor.systemBlue.setStroke()
        let outline = NSBezierPath(rect: stageBox.insetBy(dx: 0.5, dy: 0.5))
        outline.lineWidth = 1
        outline.stroke()
    }

    /// A drag can move the picture: custom mode, a picture to move, not locked.
    var canDrag: Bool { !isLocked && hasStage && !geometry.fillStage && pointsPerPixel > 0 }

    override func mouseDown(with event: NSEvent) {
        guard canDrag else { return }
        endDrag()
        dragFrom = (convert(event.locationInWindow, from: nil), geometry.translation)
        onHold?(true)
    }

    override func mouseDragged(with event: NSEvent) {
        drag(to: convert(event.locationInWindow, from: nil), ended: false)
    }

    override func mouseUp(with event: NSEvent) { drag(to: convert(event.locationInWindow, from: nil), ended: true) }

    /// Moves the picture by the pointer's travel since the press (y up on the stage), whole pixels.
    func drag(to point: NSPoint, ended: Bool) {
        guard let from = dragFrom else { return }
        guard canDrag else {
            dragFrom = nil
            onHold?(false)
            return
        }
        let k = pointsPerPixel
        // Held within the position's range, so a long drag on a small thumbnail stops at the
        // limit instead of being refused.
        let r = GeometryControls.position.range
        let limit = CGFloat(r.lowerBound)...CGFloat(r.upperBound)
        let moved = CGPoint(
            x: (from.translation.x + (point.x - from.mouse.x) / k).rounded().clamped(to: limit),
            y: (from.translation.y - (point.y - from.mouse.y) / k).rounded().clamped(to: limit))
        if ended { dragFrom = nil }
        geometry.translation = moved
        onDrag?(moved, ended)
        if ended { onHold?(false) }
    }

    /// A press whose release is lost, the tab or window going, Show Mode: the drag ends where
    /// it is (under Show Mode's lock nothing more is sent).
    func endDrag() {
        guard dragFrom != nil else { return }
        dragFrom = nil
        onDrag?(geometry.translation, true)
        onHold?(false)
    }
}

/// A Geometry number field: taking the focus (Tab included) scrolls it into view when the tab
/// is too short for every row.
final class GeometryField: NSTextField {
    override func becomeFirstResponder() -> Bool {
        let took = super.becomeFirstResponder()
        if took { scrollToVisible(bounds) }
        return took
    }
}

/// What the Geometry tab's fields scroll over: top-down, as a form reads.
final class GeometryFieldsView: NSView {
    nonisolated override var isFlipped: Bool { true }
}

/// The Geometry tab's controls. Typed values and choices go out through `onEdit` as kind fields;
/// a thumbnail drag sends each move with one coalesce key, so a drag is one undo step.
@MainActor
final class GeometryControls: NSObject, NSTextFieldDelegate {
    let modePopup = NSPopUpButton()
    let stylePopup = NSPopUpButton()
    let layerField = GeometryField()
    let opacityField = GeometryField()
    let opacityReset = GeometryControls.resetButton("opacity")
    let smoothCheck = NSButton(checkboxWithTitle: loc("inspector.geometry.smooth"), target: nil, action: nil)
    let xField = GeometryField()
    let yField = GeometryField()
    let positionReset = GeometryControls.resetButton("position")
    let scaleXField = GeometryField()
    let scaleYField = GeometryField()
    let scaleLock = NSButton()
    let scaleReset = GeometryControls.resetButton("scale")
    let rotationField = GeometryField()
    let rotationReset = GeometryControls.resetButton("rotation")
    let cropFields: [NSTextField] = VideoGeometry.cropEdges.map { _ in GeometryField() }
    let cropReset = GeometryControls.resetButton("crop")
    let anchorXField = GeometryField()
    let anchorYField = GeometryField()
    let anchorReset = GeometryControls.resetButton("anchor")
    let thumbnail = GeometryThumbnail()
    /// The scroll view the fields sit in (set when the tab is built).
    weak var scroll: NSScrollView?
    /// The least width the thumbnail keeps, whatever the inspector's width.
    static let thumbnailLeast: CGFloat = 96
    /// The rows only custom mode has, and the style row only fill-stage mode has.
    var customRows: [NSGridRow] = []
    var styleRow: NSGridRow?

    /// Sends kind fields; `coalesce` joins a drag's moves into one undo step.
    var onEdit: (([String: JSONValue], _ coalesce: String?) -> Void)?
    /// Says why a typed value was not taken.
    var onRefuse: ((String) -> Void)?
    /// A thumbnail drag begins or ends: the inspector pins the cue it began on.
    var onHold: ((Bool) -> Void)? {
        get { thumbnail.onHold }
        set { thumbnail.onHold = newValue }
    }
    private(set) var shown = VideoGeometry()
    /// The field being typed in: a redraw leaves its text alone.
    private var typing: NSTextField?
    private var drags = 0
    var isLocked = false {
        didSet {
            for c in controls { c.isEnabled = !isLocked }
            thumbnail.isLocked = isLocked
            if isLocked { thumbnail.endDrag() }
        }
    }

    var numberFields: [NSTextField] {
        [layerField, opacityField, xField, yField, scaleXField, scaleYField, rotationField, anchorXField, anchorYField]
            + cropFields
    }

    var controls: [NSControl] {
        numberFields + [
            modePopup, stylePopup, smoothCheck, scaleLock, opacityReset, positionReset, scaleReset, rotationReset,
            cropReset, anchorReset,
        ]
    }

    static func resetButton(_ what: String) -> NSButton {
        let b = NSButton()
        b.bezelStyle = .inline
        b.isBordered = false
        b.image = NSImage(
            systemSymbolName: "arrow.counterclockwise",
            accessibilityDescription: loc("inspector.geometry.reset.\(what)"))
        b.imagePosition = .imageOnly
        b.toolTip = loc("inspector.geometry.reset.\(what)")
        b.setAX("inspector-geometry-\(what)-reset", label: loc("inspector.geometry.reset.\(what)"))
        b.setContentHuggingPriority(.required, for: .horizontal)
        b.setContentCompressionResistancePriority(.required, for: .horizontal)
        return b
    }

    override init() {
        super.init()
        modePopup.setAX("inspector-geometry-mode", label: loc("inspector.geometry.mode"))
        for key in ["fill", "custom"] {
            modePopup.addItem(withTitle: loc("inspector.geometry.mode.\(key)"))
        }
        stylePopup.setAX("inspector-geometry-style", label: loc("inspector.geometry.style"))
        for key in VideoGeometry.styles { stylePopup.addItem(withTitle: loc("inspector.geometry.style.\(key)")) }
        smoothCheck.setAX("inspector-geometry-smooth", label: loc("inspector.geometry.smooth"))
        scaleLock.setAX("inspector-geometry-scale-lock", label: loc("inspector.geometry.scaleLock"))
        scaleLock.bezelStyle = .inline
        scaleLock.isBordered = false
        scaleLock.imagePosition = .imageOnly
        scaleLock.setButtonType(.toggle)
        scaleLock.toolTip = loc("inspector.geometry.scaleLock")
        thumbnail.setAX("inspector-geometry-thumbnail", label: loc("inspector.geometry.thumbnail"))
        thumbnail.setAccessibilityElement(true)
        thumbnail.setAccessibilityRole(.image)
        let ids: [(NSTextField, String)] = [
            (layerField, "layer"), (opacityField, "opacity"), (xField, "x"), (yField, "y"), (scaleXField, "scale-x"),
            (scaleYField, "scale-y"), (rotationField, "rotation"), (anchorXField, "anchor-x"),
            (anchorYField, "anchor-y"),
        ]
        for (f, id) in ids {
            f.setAX("inspector-geometry-\(id)", label: loc("inspector.geometry.field.\(id)"))
            f.toolTip = loc("inspector.geometry.field.\(id)")
        }
        for (f, edge) in zip(cropFields, VideoGeometry.cropEdges) {
            f.setAX("inspector-geometry-crop-\(edge)", label: loc("inspector.geometry.crop.\(edge)"))
            f.toolTip = loc("inspector.geometry.crop.\(edge)")
        }
        for f in numberFields {
            f.delegate = self
            f.alignment = .right
            f.controlSize = .small
            f.font = .monospacedDigitSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
            f.widthAnchor.constraint(equalToConstant: 50).isActive = true
            f.setContentCompressionResistancePriority(.required, for: .horizontal)
        }
        for p in [modePopup, stylePopup] {
            p.controlSize = .small
            p.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        }
        smoothCheck.controlSize = .small
        smoothCheck.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        for c in controls where !(c is NSTextField) {
            c.target = self
            c.action = #selector(chosen(_:))
        }
        thumbnail.onDrag = { [weak self] moved, ended in self?.dragged(to: moved, ended: ended) }
    }

    // MARK: Showing

    /// Shows `g` for a picture of `picture` pixels (nil: not probed yet) on a stage of `stage`
    /// pixels (nil: none). A field being typed in keeps its text.
    func render(_ g: VideoGeometry, stage: CGSize?, picture: CGSize?) {
        shown = g
        modePopup.selectItem(at: g.fillStage ? 0 : 1)
        stylePopup.selectItem(at: VideoGeometry.styles.firstIndex(of: g.fillStyle) ?? 0)
        smoothCheck.state = g.smooth ? .on : .off
        scaleLock.state = g.scaleLocked ? .on : .off
        scaleLock.image = NSImage(
            systemSymbolName: g.scaleLocked ? "lock" : "lock.open",
            accessibilityDescription: loc("inspector.geometry.scaleLock"))
        let texts: [(NSTextField, String)] =
            [
                (layerField, Self.layerText(g.layer)), (opacityField, Self.number(g.opacity * 100)),
                (xField, Self.number(g.translation.x)), (yField, Self.number(g.translation.y)),
                (scaleXField, Self.number(g.scale.x)), (scaleYField, Self.number(g.scale.y)),
                (rotationField, Self.number(g.rotation)), (anchorXField, Self.number(g.anchor.x)),
                (anchorYField, Self.number(g.anchor.y)),
            ] + zip(cropFields, g.crop.map(Self.number)).map { ($0, $1) }
        for (f, text) in texts where f !== typing && f.stringValue != text { f.stringValue = text }
        for r in customRows { r.isHidden = g.fillStage }
        styleRow?.isHidden = !g.fillStage
        thumbnail.stage = stage ?? CGSize(width: 1920, height: 1080)
        thumbnail.hasStage = stage != nil
        thumbnail.picture = picture
        thumbnail.geometry = g
    }

    /// The range a field takes, as the engine enforces it (core `video_geometry.rs`; the app's
    /// `testEachGeometryRangeIsTheEnginesAtBothEnds` sends each end and just past it to the real
    /// engine, so the two cannot drift apart), and the line that says so when a value is not.
    struct FieldRange {
        let range: ClosedRange<Double>
        let whole: Bool
        let message: String

        func allows(_ v: Double) -> Bool { range.contains(v) && (!whole || v.rounded() == v) }
    }

    static let position = FieldRange(range: -65536...65536, whole: false, message: "inspector.geometry.range.position")
    static let scaleRange = FieldRange(range: -100...100, whole: false, message: "inspector.geometry.range.scale")
    static let rotationRange = FieldRange(range: -360...360, whole: false, message: "inspector.geometry.range.rotation")
    static let cropRange = FieldRange(range: 0...16384, whole: false, message: "inspector.geometry.range.crop")
    static let anchorRange = FieldRange(range: -65536...65536, whole: false, message: "inspector.geometry.range.anchor")
    /// In percent, as the field shows it (the engine's 0 to 1).
    static let opacityRange = FieldRange(range: 0...100, whole: false, message: "inspector.geometry.range.opacity")
    static let layerRange = FieldRange(range: 0...1000, whole: true, message: "inspector.geometry.range.layer")

    func range(of f: NSTextField) -> FieldRange? {
        switch f {
        case layerField: return Self.layerRange
        case opacityField: return Self.opacityRange
        case xField, yField: return Self.position
        case scaleXField, scaleYField: return Self.scaleRange
        case rotationField: return Self.rotationRange
        case anchorXField, anchorYField: return Self.anchorRange
        default: return cropFields.contains { $0 === f } ? Self.cropRange : nil
        }
    }

    /// A number as a field shows it: up to three decimals, trailing zeros dropped.
    nonisolated static func number(_ v: Double) -> String {
        guard v.isFinite else { return "" }
        var text = String(format: "%.3f", v)
        while text.hasSuffix("0") { text.removeLast() }
        if text.hasSuffix(".") { text.removeLast() }
        return text == "-0" ? "0" : text
    }

    /// The layer as its field shows it: the top and bottom layers by name (QLab 5 p. 213).
    nonisolated static func layerText(_ layer: Double) -> String {
        switch layer {
        case 1000: return loc("inspector.geometry.layer.top")
        case 0: return loc("inspector.geometry.layer.bottom")
        default: return number(layer)
        }
    }

    /// A typed layer: a whole number, or the top or bottom layer by name.
    nonisolated static func parseLayer(_ text: String) -> Double? {
        let t = text.trimmingCharacters(in: .whitespaces)
        if t.caseInsensitiveCompare(loc("inspector.geometry.layer.top")) == .orderedSame { return 1000 }
        if t.caseInsensitiveCompare(loc("inspector.geometry.layer.bottom")) == .orderedSame { return 0 }
        return Double(t)
    }

    // MARK: Editing

    func controlTextDidBeginEditing(_ obj: Notification) {
        typing = obj.object as? NSTextField
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        guard let f = obj.object as? NSTextField else { return }
        if typing === f { typing = nil }
        commit(f)
    }

    /// Esc puts the shown value back.
    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard selector == #selector(NSResponder.cancelOperation(_:)), let f = control as? NSTextField else {
            return false
        }
        typing = nil
        render(shown, stage: thumbnail.stage, picture: thumbnail.picture)
        f.currentEditor()?.selectAll(nil)
        return true
    }

    /// Sends what `f` holds when it differs from what is shown; text that is not a number is not
    /// sent and the shown value comes back. A value outside its field's range (`FieldRange`, the
    /// engine's) is not sent either: the message line says the range.
    func commit(_ f: NSTextField) {
        guard !isLocked else { return }
        let text = f.stringValue.trimmingCharacters(in: .whitespaces)
        let g = shown
        let value = f === layerField ? Self.parseLayer(text) : Double(text)
        guard let v = value, v.isFinite else {
            // An emptied field is no change; other text is said to be no number.
            if !text.isEmpty { onRefuse?(loc("inspector.geometry.notANumber")) }
            render(g, stage: thumbnail.stage, picture: thumbnail.picture)
            return
        }
        if let range = range(of: f), !range.allows(v) {
            onRefuse?(loc(range.message))
            render(g, stage: thumbnail.stage, picture: thumbnail.picture)
            return
        }
        var fields: [String: JSONValue] = [:]
        switch f {
        case layerField where v != g.layer:
            fields["layer"] = v.rounded() == v ? .int(Int64(v)) : .double(v)
        case opacityField where v / 100 != g.opacity:
            fields["opacity"] = .double(v / 100)
        case xField where v != g.translation.x:
            fields["translation"] = .object(["x": .double(v)])
        case yField where v != g.translation.y:
            fields["translation"] = .object(["y": .double(v)])
        case scaleXField where v != g.scale.x || (g.scaleLocked && v != g.scale.y):
            fields["scale"] = g.scaleLocked ? VideoGeometry.pairJSON(CGPoint(x: v, y: v)) : .object(["x": .double(v)])
        case scaleYField where v != g.scale.y || (g.scaleLocked && v != g.scale.x):
            fields["scale"] = g.scaleLocked ? VideoGeometry.pairJSON(CGPoint(x: v, y: v)) : .object(["y": .double(v)])
        case rotationField where v != g.rotation:
            fields["rotation"] = .double(v)
        case anchorXField where v != g.anchor.x:
            fields["anchor"] = .object(["x": .double(v)])
        case anchorYField where v != g.anchor.y:
            fields["anchor"] = .object(["y": .double(v)])
        default:
            if let i = cropFields.firstIndex(where: { $0 === f }), v != g.crop[i] {
                fields["crop"] = .object([VideoGeometry.cropEdges[i]: .double(v)])
            }
        }
        if fields.isEmpty {
            render(g, stage: thumbnail.stage, picture: thumbnail.picture)
            return
        }
        onEdit?(fields, nil)
    }

    @objc func chosen(_ sender: NSControl) {
        guard !isLocked else { return }
        let g = shown
        let d = VideoGeometry()
        var fields: [String: JSONValue] = [:]
        switch sender {
        case modePopup:
            let fill = modePopup.indexOfSelectedItem == 0
            if fill != g.fillStage { fields["fill_stage"] = .bool(fill) }
        case stylePopup:
            let i = stylePopup.indexOfSelectedItem
            if VideoGeometry.styles.indices.contains(i), VideoGeometry.styles[i] != g.fillStyle {
                fields["fill_style"] = .string(VideoGeometry.styles[i])
            }
        case smoothCheck:
            fields["smooth"] = .bool(smoothCheck.state == .on)
        case scaleLock:
            fields["scale_locked"] = .bool(!g.scaleLocked)
        case opacityReset where g.opacity != d.opacity:
            fields["opacity"] = .double(d.opacity)
        case positionReset where g.translation != d.translation:
            fields["translation"] = VideoGeometry.pairJSON(d.translation)
        case scaleReset where g.scale != d.scale:
            fields["scale"] = VideoGeometry.pairJSON(d.scale)
        case rotationReset where g.rotation != d.rotation:
            fields["rotation"] = .double(d.rotation)
        case cropReset where g.crop != d.crop:
            fields["crop"] = VideoGeometry.cropJSON(d.crop)
        case anchorReset where g.anchor != d.anchor:
            fields["anchor"] = VideoGeometry.pairJSON(d.anchor)
        default: break
        }
        if fields.isEmpty {
            render(g, stage: thumbnail.stage, picture: thumbnail.picture)
            return
        }
        onEdit?(fields, nil)
    }

    /// A thumbnail drag's move: the position fields follow at once, and every move of one drag
    /// shares a coalesce key, so the drag is one undo step.
    func dragged(to moved: CGPoint, ended: Bool) {
        guard !isLocked else { return }
        if shown.translation == moved, !ended { return }
        var g = shown
        g.translation = moved
        render(g, stage: thumbnail.stage, picture: thumbnail.picture)
        onEdit?(
            ["translation": VideoGeometry.pairJSON(moved)],
            "geometry:drag:\(InspectorViewController.launchToken):\(drags)")
        if ended { drags += 1 }
    }
}

extension InspectorViewController {
    /// The Geometry tab (QLab 5 manual pp. 212-214; UI map `inspector.video.geometry`): mode, style,
    /// layer, opacity and smoothing on the left; in custom mode the position, scale with its lock,
    /// rotation, crop and anchor beside them, each with a reset; the thumbnail fills the rest.
    func geometryTab() -> NSTabViewItem {
        let g = geometry
        g.onEdit = { [weak self] fields, coalesce in self?.sendGeometry(fields, coalesce: coalesce) }
        g.onRefuse = { [weak self] message in self?.showRefusal(message) }
        g.onHold = { [weak self] on in self?.gestureHold(on) }
        func label(_ key: String) -> NSTextField {
            let l = NSTextField(labelWithString: loc(key))
            l.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            l.textColor = .secondaryLabelColor
            l.alignment = .right
            return l
        }
        func line(_ views: [NSView]) -> NSStackView {
            let s = NSStackView(views: views)
            s.spacing = 4
            return s
        }
        let percent = label("inspector.geometry.percent")
        percent.alignment = .left
        let left = NSGridView(views: [
            [label("inspector.geometry.mode"), g.modePopup],
            [label("inspector.geometry.style"), g.stylePopup],
            [label("inspector.geometry.layer"), g.layerField],
            [label("inspector.geometry.opacity"), line([g.opacityField, percent, g.opacityReset])],
            [NSGridCell.emptyContentView, g.smoothCheck],
        ])
        g.styleRow = left.row(at: 1)
        // The grid sized its value column by the narrowest view and squeezed the pop-ups and the
        // opacity line; the column is as wide as its widest control needs.
        left.column(at: 1).width = 140
        let right = NSGridView(views: [
            [label("inspector.geometry.position"), line([g.xField, g.yField, g.positionReset])],
            [label("inspector.geometry.scale"), line([g.scaleXField, g.scaleLock, g.scaleYField, g.scaleReset])],
            [label("inspector.geometry.rotation"), line([g.rotationField, g.rotationReset])],
            [label("inspector.geometry.crop"), line(g.cropFields + [g.cropReset])],
            [label("inspector.geometry.anchor"), line([g.anchorXField, g.anchorYField, g.anchorReset])],
        ])
        g.customRows = (0..<right.numberOfRows).map { right.row(at: $0) }
        for grid in [left, right] {
            grid.rowSpacing = 6
            grid.columnSpacing = 6
            grid.rowAlignment = .firstBaseline
            grid.column(at: 0).xPlacement = .trailing
            grid.setContentHuggingPriority(.required, for: .horizontal)
            grid.setContentCompressionResistancePriority(.required, for: .horizontal)
        }
        g.thumbnail.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        g.thumbnail.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        g.thumbnail.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .vertical)
        // Fixed-size grids side by side, the thumbnail taking what is left; a short tab clips
        // the grids at the bottom, never at the side.
        // The fields scroll (as the Levels tab's matrix does) when the tab is shorter or narrower
        // than they are, so none is ever drawn outside the panel; the thumbnail keeps at least
        // `thumbnailLeast` points of the width, scaled down, and takes what the fields leave.
        let fields = GeometryFieldsView()
        for v in [left, right] as [NSView] {
            v.translatesAutoresizingMaskIntoConstraints = false
            fields.addSubview(v)
        }
        let scroll = NSScrollView()
        scroll.setAX("inspector-geometry-scroll")
        scroll.documentView = fields
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        scroll.borderType = .noBorder
        g.scroll = scroll
        let content = NSView()
        for v in [scroll, g.thumbnail] as [NSView] {
            v.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(v)
        }
        fields.translatesAutoresizingMaskIntoConstraints = false
        let clip = scroll.contentView
        // As wide as the fields need when there is room, so the thumbnail takes the rest.
        let fit = scroll.widthAnchor.constraint(equalTo: fields.widthAnchor)
        fit.priority = .defaultHigh
        NSLayoutConstraint.activate([
            fields.topAnchor.constraint(equalTo: clip.topAnchor),
            fields.leadingAnchor.constraint(equalTo: clip.leadingAnchor),
            left.topAnchor.constraint(equalTo: fields.topAnchor, constant: 10),
            left.leadingAnchor.constraint(equalTo: fields.leadingAnchor, constant: 12),
            left.bottomAnchor.constraint(lessThanOrEqualTo: fields.bottomAnchor, constant: -8),
            right.topAnchor.constraint(equalTo: fields.topAnchor, constant: 10),
            right.leadingAnchor.constraint(equalTo: left.trailingAnchor, constant: 18),
            right.trailingAnchor.constraint(equalTo: fields.trailingAnchor),
            right.bottomAnchor.constraint(lessThanOrEqualTo: fields.bottomAnchor, constant: -8),
            scroll.topAnchor.constraint(equalTo: content.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            scroll.widthAnchor.constraint(lessThanOrEqualTo: fields.widthAnchor),
            fit,
            g.thumbnail.topAnchor.constraint(equalTo: content.topAnchor, constant: 8),
            g.thumbnail.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -8),
            g.thumbnail.leadingAnchor.constraint(equalTo: scroll.trailingAnchor, constant: 16),
            g.thumbnail.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -12),
            g.thumbnail.widthAnchor.constraint(greaterThanOrEqualToConstant: GeometryControls.thumbnailLeast),
        ])
        for p in [g.modePopup, g.stylePopup] {
            p.widthAnchor.constraint(greaterThanOrEqualToConstant: 116).isActive = true
            p.setContentCompressionResistancePriority(.required, for: .horizontal)
        }
        for v in [g.smoothCheck, percent] as [NSView] {
            v.setContentCompressionResistancePriority(.required, for: .horizontal)
        }
        let item = NSTabViewItem(identifier: "geometry")
        item.label = loc("inspector.tab.geometry")
        item.view = content
        return item
    }

    /// A Video cue's geometry: the cue as edits see it (an unanswered edit included, so a drag
    /// shows where it is), its stage's size and its picture's, as probed.
    func renderGeometry(_ cue: Cue, model: WorkspaceModel) {
        guard cue.kindType == "video" else { return }
        let stageID = cue.raw["kind"]?["stage"]?.stringValue
        let stage = model.stages.first { $0.id.raw == stageID }.flatMap { s -> CGSize? in
            guard let w = s.raw["width"]?.doubleValue, let h = s.raw["height"]?.doubleValue else { return nil }
            return CGSize(width: w, height: h)
        }
        let picture = store.mediaDurations[cue.id]?.picture.map { CGSize(width: $0.width, height: $0.height) }
        geometry.render(VideoGeometry(kind: cue.raw["kind"]), stage: stage, picture: picture)
        geometry.isLocked = isLocked
    }

    /// Sends geometry fields for the shown Video cue, or during a thumbnail drag the cue the drag
    /// began on whatever is selected now: one undo step each, a drag's moves one.
    func sendGeometry(_ fields: [String: JSONValue], coalesce: String?) {
        guard let cue = cueForKindEdit, cue.kindType == "video" else { return }
        send(
            .setProperty(cue.id, .kindFields(fields)), cue: cue.id,
            key: "geometry.\(fields.keys.sorted().joined(separator: ","))", revert: [], coalesce: coalesce,
            refusedAs: loc("inspector.geometry.refused"))
        // Shown with the edit before the engine answers, so the next field typed (y after x)
        // builds on it, not on the value the model still holds.
        if let cue = cueForKindEdit, let model = store.model { renderGeometry(cue, model: model) }
    }
}
