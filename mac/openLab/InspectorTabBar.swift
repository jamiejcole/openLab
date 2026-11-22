// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// One tab title in the inspector's bar: drawn flat, a selected one with a quiet fill and a line
/// under it, as the inspector's tabs are in QLab (UI map `inspector.*`).
final class InspectorTabButton: NSButton {
    let tabID: String
    var isChosen = false {
        didSet {
            guard isChosen != oldValue else { return }
            needsDisplay = true
            setAccessibilityValue(isChosen ? 1 : 0)
        }
    }
    static let font = NSFont.systemFont(ofSize: 11)
    static let padding: CGFloat = 12

    init(id: String, title: String) {
        tabID = id
        super.init(frame: .zero)
        self.title = title
        isBordered = false
        setButtonType(.momentaryChange)
        setAccessibilityRole(.radioButton)
        setAccessibilityLabel(title)
        setAccessibilityIdentifier("inspector-tab-\(id)")
        setAccessibilityValue(0)
        setContentHuggingPriority(.required, for: .horizontal)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// The whole title and its padding: a tab is never narrower, so a title is never cut.
    var wantedWidth: CGFloat {
        ((title as NSString).size(withAttributes: [.font: Self.font]).width).rounded(.up) + 2 * Self.padding
    }

    nonisolated override var isFlipped: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        if isChosen {
            NSColor.labelColor.withAlphaComponent(0.10).setFill()
            bounds.fill()
            NSColor.controlAccentColor.setFill()
            NSRect(x: 0, y: bounds.height - 2, width: bounds.width, height: 2).fill()
        }
        let attrs: [NSAttributedString.Key: Any] = [
            .font: Self.font,
            .foregroundColor: isEnabled ? NSColor.labelColor : NSColor.disabledControlTextColor,
        ]
        let s = NSAttributedString(string: title, attributes: attrs)
        let size = s.size()
        s.draw(at: NSPoint(x: (bounds.width - size.width) / 2, y: (bounds.height - size.height) / 2))
    }
}

/// The inspector's tab titles in a row at the left, one button per tab of the tab view, in a scroll
/// view: at the least window the row is wider than the bar and scrolls, so no title is ever cut.
@MainActor
final class InspectorTabBar: NSView {
    static let height: CGFloat = 24
    let scroll = TabBarScrollView()
    private let strip = NSView()
    private(set) var buttons: [InspectorTabButton] = []
    /// The user chose the tab with this identifier.
    var onChoose: ((String) -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        scroll.documentView = strip
        scroll.hasHorizontalScroller = false
        scroll.hasVerticalScroller = false
        scroll.drawsBackground = false
        scroll.borderType = .noBorder
        scroll.horizontalScrollElasticity = .allowed
        scroll.verticalScrollElasticity = .none
        scroll.translatesAutoresizingMaskIntoConstraints = false
        addSubview(scroll)
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo: topAnchor),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            heightAnchor.constraint(equalToConstant: Self.height),
        ])
        setAccessibilityRole(.radioGroup)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Draws the buttons for `tabs` (identifier, title), the one with identifier `selected` chosen.
    func show(_ tabs: [(id: String, title: String)], selected: String?) {
        if buttons.map(\.tabID) != tabs.map(\.id) || buttons.map(\.title) != tabs.map(\.title) {
            for b in buttons { b.removeFromSuperview() }
            buttons = tabs.map { t in
                let b = InspectorTabButton(id: t.id, title: t.title)
                b.target = self
                b.action = #selector(chosen(_:))
                strip.addSubview(b)
                return b
            }
            var x: CGFloat = 0
            for b in buttons {
                let w = b.wantedWidth
                b.frame = NSRect(x: x, y: 0, width: w, height: Self.height)
                x += w
            }
            strip.frame = NSRect(x: 0, y: 0, width: x, height: Self.height)
        }
        select(selected)
    }

    /// Whether the titles are wider than the bar, so it scrolls.
    var scrolls: Bool { strip.frame.width > scroll.contentView.bounds.width + 0.5 }

    func select(_ id: String?) {
        for b in buttons { b.isChosen = b.tabID == id }
        // A chosen tab that is partly out of the bar's view is brought in.
        if let b = buttons.first(where: { $0.tabID == id }) { scroll.contentView.scrollToVisible(b.frame) }
        scroll.reflectScrolledClipView(scroll.contentView)
    }

    @objc private func chosen(_ sender: InspectorTabButton) { onChoose?(sender.tabID) }

}

/// A scroll view of one row: the wheel's up and down move it sideways too, so a mouse with no
/// horizontal wheel can reach every tab.
final class TabBarScrollView: NSScrollView {
    override func scrollWheel(with event: NSEvent) {
        guard abs(event.scrollingDeltaY) > abs(event.scrollingDeltaX) else { return super.scrollWheel(with: event) }
        var origin = contentView.bounds.origin
        let room = max(0, (documentView?.frame.width ?? 0) - contentView.bounds.width)
        origin.x = min(max(0, origin.x - event.scrollingDeltaY), room)
        contentView.scroll(to: origin)
        reflectScrolledClipView(contentView)
    }
}
