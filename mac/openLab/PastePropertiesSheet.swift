// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The Paste Cue Properties sheet (ED-06; QLab 5, manual p. 154, surface `sheet.paste-cue-properties`):
/// the copied cue's properties by category, each with a tick box, and a box per category that ticks
/// or clears all of it. Apply (Return or Enter) hands back the ticked properties; Cancel (Esc) hands
/// back nil and nothing changes. It opens with the boxes as they were when it was last applied.
/// The keys are the sheet's own, whatever the system's keyboard setting: Up and Down move a
/// highlighted row, Space ticks or clears it (a category row: the whole category).
@MainActor
final class PastePropertiesSheet: NSObject {
    /// Where the ticked properties are kept between uses (per Mac, as QLab keeps its last set).
    static let defaultsKey = "PastePropertiesTicked"

    let window: NSWindow
    /// One line: a long cue name is cut, never wrapped over the list.
    let titleLabel = NSTextField(labelWithString: "")
    let listScroll = NSScrollView()
    let applyButton = NSButton(title: loc("paste.apply"), target: nil, action: nil)
    let cancelButton = NSButton(title: loc("edit.cancel"), target: nil, action: nil)
    private(set) var categoryChecks: [PasteProperty.Category: NSButton] = [:]
    private(set) var propertyChecks: [PasteProperty: NSButton] = [:]
    private let offered: [(category: PasteProperty.Category, properties: [PasteProperty])]
    /// Every row in list order: a category's box, then its properties'.
    private(set) var rows: [PasteSheetRow] = []
    /// The row Up, Down and Space act on.
    private(set) var highlighted = 0 {
        didSet { showHighlight() }
    }
    /// Called once: the ticked properties on Apply, nil on Cancel.
    var onDone: ((Set<PasteProperty>?) -> Void)?

    /// The properties ticked now.
    var ticked: Set<PasteProperty> { Set(propertyChecks.filter { $0.value.state == .on }.map(\.key)) }

    init(
        title: String, offered: [(category: PasteProperty.Category, properties: [PasteProperty])],
        ticked: Set<PasteProperty>
    ) {
        self.offered = offered
        window = PasteSheetWindow(
            contentRect: NSRect(x: 0, y: 0, width: 460, height: 440), styleMask: [.titled], backing: .buffered,
            defer: true)
        super.init()
        window.isReleasedWhenClosed = false
        window.setAccessibilityIdentifier("paste-properties-sheet")
        (window as? PasteSheetWindow)?.onKey = { [weak self] key in self?.handle(key) ?? false }
        build(title: title, ticked: ticked)
    }

    private func build(title: String, ticked: Set<PasteProperty>) {
        titleLabel.stringValue = title
        titleLabel.setAX("paste-properties-title")
        titleLabel.font = .systemFont(ofSize: NSFont.systemFontSize, weight: .semibold)
        titleLabel.lineBreakMode = .byTruncatingMiddle
        titleLabel.maximumNumberOfLines = 1
        titleLabel.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        titleLabel.toolTip = title
        let list = NSStackView()
        list.orientation = .vertical
        list.alignment = .leading
        list.spacing = 2
        list.edgeInsets = NSEdgeInsets(top: 6, left: 8, bottom: 6, right: 8)
        for (category, properties) in offered {
            let head = NSButton(
                checkboxWithTitle: loc(category.titleKey), target: self, action: #selector(categoryClicked))
            head.font = .boldSystemFont(ofSize: NSFont.systemFontSize)
            head.setAX("paste-category-\(category.rawValue)")
            categoryChecks[category] = head
            if let last = list.arrangedSubviews.last { list.setCustomSpacing(10, after: last) }
            add(head, indent: 0, to: list)
            for property in properties {
                let box = NSButton(
                    checkboxWithTitle: loc(property.titleKey), target: self, action: #selector(propertyClicked))
                box.setAX("paste-property-\(property.rawValue)")
                box.state = ticked.contains(property) ? .on : .off
                propertyChecks[property] = box
                add(box, indent: 20, to: list)
            }
        }
        refreshCategories()
        showHighlight()
        let flipped = FlippedView()
        flipped.addSubview(list)
        list.translatesAutoresizingMaskIntoConstraints = false
        let scroll = listScroll
        scroll.hasVerticalScroller = true
        // A list longer than the sheet shows its scroller whatever the system's scroll bar setting.
        scroll.scrollerStyle = .legacy
        scroll.autohidesScrollers = true
        scroll.borderType = .bezelBorder
        scroll.drawsBackground = false
        scroll.documentView = flipped
        flipped.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            list.topAnchor.constraint(equalTo: flipped.topAnchor),
            list.leadingAnchor.constraint(equalTo: flipped.leadingAnchor),
            list.trailingAnchor.constraint(equalTo: flipped.trailingAnchor),
            list.bottomAnchor.constraint(equalTo: flipped.bottomAnchor),
            flipped.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor),
        ])
        applyButton.setAX("paste-properties-apply")
        applyButton.keyEquivalent = "\r"
        applyButton.target = self
        applyButton.action = #selector(apply)
        cancelButton.setAX("paste-properties-cancel")
        cancelButton.keyEquivalent = "\u{1b}"
        cancelButton.target = self
        cancelButton.action = #selector(cancel)
        let spacer = NSView()
        spacer.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let buttons = NSStackView(views: [cancelButton, spacer, applyButton])
        for b in [cancelButton, applyButton] {
            b.widthAnchor.constraint(greaterThanOrEqualToConstant: 90).isActive = true
        }
        let content = NSView()
        for v in [titleLabel, scroll, buttons] {
            v.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(v)
        }
        NSLayoutConstraint.activate([
            titleLabel.topAnchor.constraint(equalTo: content.topAnchor, constant: 16),
            titleLabel.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            titleLabel.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            scroll.topAnchor.constraint(equalTo: titleLabel.bottomAnchor, constant: 10),
            scroll.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            scroll.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            buttons.topAnchor.constraint(equalTo: scroll.bottomAnchor, constant: 14),
            buttons.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 20),
            buttons.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -20),
            buttons.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -16),
            content.widthAnchor.constraint(equalToConstant: 460),
            content.heightAnchor.constraint(equalToConstant: 440),
        ])
        window.contentView = content
        // The sheet takes the keys itself (no box needs the focus).
        window.initialFirstResponder = nil
        for row in rows { row.box.refusesFirstResponder = true }
    }

    private func add(_ box: NSButton, indent: CGFloat, to list: NSStackView) {
        let row = PasteSheetRow(box: box, indent: indent)
        rows.append(row)
        list.addArrangedSubview(row)
        row.widthAnchor.constraint(equalTo: list.widthAnchor, constant: -16).isActive = true
    }

    private func showHighlight() {
        for (i, row) in rows.enumerated() { row.isHighlighted = i == highlighted }
        if rows.indices.contains(highlighted) { rows[highlighted].scrollToVisible(rows[highlighted].bounds) }
    }

    /// The sheet's keys; true when the key was one of them.
    func handle(_ key: PasteSheetWindow.Key) -> Bool {
        switch key {
        case .up: highlighted = max(0, highlighted - 1)
        case .down: highlighted = min(rows.count - 1, highlighted + 1)
        case .toggle:
            guard rows.indices.contains(highlighted) else { return true }
            toggle(rows[highlighted].box)
        case .apply: apply()
        case .cancel: cancel()
        }
        return true
    }

    /// What a click on `box` does, from the keyboard.
    private func toggle(_ box: NSButton) {
        if categoryChecks.values.contains(where: { $0 === box }) {
            categoryClicked(box)
        } else {
            box.state = box.state == .on ? .off : .on
            refreshCategories()
        }
    }

    /// A category's box is on when all of its properties are ticked, mixed when some are.
    private func refreshCategories() {
        for (category, properties) in offered {
            let on = properties.filter { propertyChecks[$0]?.state == .on }.count
            let head = categoryChecks[category]
            head?.allowsMixedState = on > 0 && on < properties.count
            head?.state = on == 0 ? .off : on == properties.count ? .on : .mixed
        }
    }

    /// A category's box ticks all of its properties, or clears them when all were ticked.
    @objc private func categoryClicked(_ sender: NSButton) {
        highlight(sender)
        guard let (_, properties) = offered.first(where: { categoryChecks[$0.category] === sender }) else { return }
        let all = properties.allSatisfy { propertyChecks[$0]?.state == .on }
        for p in properties { propertyChecks[p]?.state = all ? .off : .on }
        refreshCategories()
    }

    @objc private func propertyClicked(_ sender: NSButton) {
        highlight(sender)
        refreshCategories()
    }

    /// A click on a row's box highlights that row, so the keys go on from there.
    private func highlight(_ box: NSButton) {
        if let i = rows.firstIndex(where: { $0.box === box }) { highlighted = i }
    }

    @objc func apply() { finish(ticked) }

    @objc func cancel() { finish(nil) }

    private func finish(_ result: Set<PasteProperty>?) {
        guard let done = onDone else { return }
        onDone = nil
        window.sheetParent?.endSheet(window)
        window.orderOut(nil)
        done(result)
    }

    // MARK: Remembered ticks

    static func remembered(_ defaults: UserDefaults) -> Set<PasteProperty> {
        Set((defaults.stringArray(forKey: defaultsKey) ?? []).compactMap(PasteProperty.init(rawValue:)))
    }

    /// What is remembered after an Apply: the ticks of the properties the sheet offered, and the
    /// earlier ticks of those it did not (another kind of cue's), so each comes back next time.
    static func remember(
        _ ticked: Set<PasteProperty>, offered: Set<PasteProperty>, in defaults: UserDefaults
    ) {
        let kept = remembered(defaults).subtracting(offered).union(ticked)
        defaults.set(kept.map(\.rawValue).sorted(), forKey: defaultsKey)
    }
}

/// The sheet's window. Its keys go to the sheet before any control (a tick box may hold the focus
/// when the system lets buttons take it): Up, Down, Space, Return and the keypad's Enter, Esc.
final class PasteSheetWindow: NSWindow {
    enum Key { case up, down, toggle, apply, cancel }
    var onKey: ((Key) -> Bool)?

    static func key(_ event: NSEvent) -> Key? {
        let mods = event.modifierFlags.intersection([.command, .option, .control, .shift])
        guard mods.isEmpty else { return nil }
        switch event.keyCode {
        case 126: return .up
        case 125: return .down
        case 49: return .toggle
        case 36, 76: return .apply
        case 53: return .cancel
        default: return nil
        }
    }

    override func sendEvent(_ event: NSEvent) {
        if event.type == .keyDown, let key = Self.key(event), onKey?(key) == true { return }
        super.sendEvent(event)
    }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if event.type == .keyDown, let key = Self.key(event), onKey?(key) == true { return true }
        return super.performKeyEquivalent(with: event)
    }

    override func cancelOperation(_ sender: Any?) { _ = onKey?(.cancel) }
}

/// One row of the sheet: a tick box, indented under its category, drawn highlighted when the keys
/// act on it.
final class PasteSheetRow: NSView {
    let box: NSButton
    var isHighlighted = false {
        didSet { if isHighlighted != oldValue { needsDisplay = true } }
    }

    init(box: NSButton, indent: CGFloat) {
        self.box = box
        super.init(frame: .zero)
        box.translatesAutoresizingMaskIntoConstraints = false
        addSubview(box)
        NSLayoutConstraint.activate([
            box.leadingAnchor.constraint(equalTo: leadingAnchor, constant: indent + 4),
            box.topAnchor.constraint(equalTo: topAnchor, constant: 2),
            box.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -2),
            box.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -4),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override func draw(_ dirtyRect: NSRect) {
        NSBezierPath.clip(bounds)
        guard isHighlighted else { return }
        // The selection colour at a strength the box and its title stay readable on, light or dark.
        NSColor.selectedContentBackgroundColor.withAlphaComponent(0.45).setFill()
        NSBezierPath(roundedRect: bounds, xRadius: 4, yRadius: 4).fill()
    }
}

private final class FlippedView: NSView {
    override var isFlipped: Bool { true }
}
