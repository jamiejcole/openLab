// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The Paste Cue Properties sheet (ED-06; QLab 5, manual p. 154, surface `sheet.paste-cue-properties`):
/// the copied cue's properties by category, each with a tick box, and a box per category that ticks
/// or clears all of it. Apply (Return or Enter) hands back the ticked properties; Cancel (Esc) hands
/// back nil and nothing changes. It opens with the boxes as they were when it was last applied.
@MainActor
final class PastePropertiesSheet: NSObject {
    /// Where the ticked properties are kept between uses (per Mac, as QLab keeps its last set).
    static let defaultsKey = "pasteCueProperties.ticked"

    let window: NSWindow
    let titleLabel = NSTextField(wrappingLabelWithString: "")
    let applyButton = NSButton(title: loc("paste.apply"), target: nil, action: nil)
    let cancelButton = NSButton(title: loc("edit.cancel"), target: nil, action: nil)
    private(set) var categoryChecks: [PasteProperty.Category: NSButton] = [:]
    private(set) var propertyChecks: [PasteProperty: NSButton] = [:]
    private let offered: [(category: PasteProperty.Category, properties: [PasteProperty])]
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
            contentRect: NSRect(x: 0, y: 0, width: 460, height: 400), styleMask: [.titled], backing: .buffered,
            defer: true)
        super.init()
        window.isReleasedWhenClosed = false
        window.setAccessibilityIdentifier("paste-properties-sheet")
        (window as? PasteSheetWindow)?.onEnter = { [weak self] in self?.apply() }
        (window as? PasteSheetWindow)?.onCancel = { [weak self] in self?.cancel() }
        build(title: title, ticked: ticked)
    }

    private func build(title: String, ticked: Set<PasteProperty>) {
        titleLabel.stringValue = title
        titleLabel.setAX("paste-properties-title")
        titleLabel.font = .systemFont(ofSize: NSFont.systemFontSize, weight: .semibold)
        let list = NSStackView()
        list.orientation = .vertical
        list.alignment = .leading
        list.spacing = 4
        list.edgeInsets = NSEdgeInsets(top: 6, left: 8, bottom: 6, right: 8)
        for (category, properties) in offered {
            let head = NSButton(
                checkboxWithTitle: loc(category.titleKey), target: self, action: #selector(categoryClicked))
            head.font = .boldSystemFont(ofSize: NSFont.systemFontSize)
            head.setAX("paste-category-\(category.rawValue)")
            categoryChecks[category] = head
            if let last = list.arrangedSubviews.last { list.setCustomSpacing(10, after: last) }
            list.addArrangedSubview(head)
            for property in properties {
                let box = NSButton(
                    checkboxWithTitle: loc(property.titleKey), target: self, action: #selector(propertyClicked))
                box.setAX("paste-property-\(property.rawValue)")
                box.state = ticked.contains(property) ? .on : .off
                propertyChecks[property] = box
                let row = NSStackView(views: [box])
                row.edgeInsets = NSEdgeInsets(top: 0, left: 20, bottom: 0, right: 0)
                list.addArrangedSubview(row)
            }
        }
        refreshCategories()
        let flipped = FlippedView()
        flipped.addSubview(list)
        list.translatesAutoresizingMaskIntoConstraints = false
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
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
            content.heightAnchor.constraint(equalToConstant: 400),
        ])
        window.contentView = content
        window.initialFirstResponder = propertyChecks[offered.first?.properties.first ?? .name]
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
        guard let (_, properties) = offered.first(where: { categoryChecks[$0.category] === sender }) else { return }
        let all = properties.allSatisfy { propertyChecks[$0]?.state == .on }
        for p in properties { propertyChecks[p]?.state = all ? .off : .on }
        refreshCategories()
    }

    @objc private func propertyClicked(_ sender: NSButton) { refreshCategories() }

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

/// Return and Esc are the buttons' keys; Enter (the keypad's) applies too, as in QLab.
final class PasteSheetWindow: NSWindow {
    var onEnter: (() -> Void)?
    var onCancel: (() -> Void)?

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 76 {
            onEnter?()
        } else {
            super.keyDown(with: event)
        }
    }

    override func cancelOperation(_ sender: Any?) { onCancel?() }
}

private final class FlippedView: NSView {
    override var isFlipped: Bool { true }
}
