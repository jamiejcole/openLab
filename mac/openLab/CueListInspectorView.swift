// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// What the inspector shows for a cue list chosen in the sidebar's Lists tab (the list itself, not a cue
/// in it): the Basics and Triggers tabs a QLab 5 cue list has. A list has a name and nothing else in the
/// model, so the name is the one field that edits (undoable, saved with the show); the others are drawn
/// as QLab 5 draws a list's, off, and the Triggers tab says it has none.
@MainActor
final class CueListInspectorView: NSView, NSTextFieldDelegate {
    static let basics = "basics"
    static let triggers = "triggers"

    let tabBar = InspectorTabBar()
    let nameField = NSTextField()
    let numberField = NSTextField()
    let durationField = NSTextField()
    let preWaitField = NSTextField()
    let postWaitField = NSTextField()
    let continuePopup = NSPopUpButton(frame: .zero, pullsDown: false)
    let targetNote = NSTextField(labelWithString: loc("inspector.target.notApplicable"))
    let notesView = NSTextView()
    let flaggedCheck = NSButton(checkboxWithTitle: loc("inspector.flagged"), target: nil, action: nil)
    let autoLoadCheck = NSButton(checkboxWithTitle: loc("inspector.autoLoad"), target: nil, action: nil)
    let armedCheck = NSButton(checkboxWithTitle: loc("inspector.armed"), target: nil, action: nil)
    let triggersNote = NSTextField(wrappingLabelWithString: loc("inspector.cueList.noTriggers"))
    private let basicsPane = NSView()
    private let triggersPane = NSView()
    private(set) var shownTab = CueListInspectorView.basics
    private(set) var listID: ModelID?
    private var listName = ""
    /// The user committed a new name for the list shown.
    var onRename: ((ModelID, String) -> Void)?
    /// Typing in the name ended (Return, Esc, Tab or a click away): the cue list takes the keys back.
    var onEndTyping: (() -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        build()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Shows `list`: its name in the field (unless it is being typed in), the tab the user last chose.
    func show(name: String, id: ModelID, locked: Bool, force: Bool = false) {
        let changed = listID != id
        listID = id
        listName = name
        if changed || force || nameField.currentEditor() == nil, nameField.stringValue != name {
            nameField.stringValue = name
        }
        nameField.isEnabled = !locked
        tabBar.show(
            [(Self.basics, loc("inspector.tab.basics")), (Self.triggers, loc("inspector.tab.triggers"))],
            selected: shownTab)
        choose(shownTab)
    }

    func choose(_ id: String) {
        shownTab = id
        basicsPane.isHidden = id != Self.basics
        triggersPane.isHidden = id != Self.triggers
        tabBar.select(id)
    }

    // MARK: Name

    func controlTextDidEndEditing(_ notification: Notification) {
        guard (notification.object as? NSTextField) === nameField else { return }
        commitName()
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard control === nameField else { return false }
        if selector == #selector(NSResponder.cancelOperation(_:)) {
            nameField.stringValue = listName
            window?.makeFirstResponder(nil)
            onEndTyping?()
            return true
        }
        if selector == #selector(NSResponder.insertNewline(_:)) {
            window?.makeFirstResponder(nil)
            onEndTyping?()
            return true
        }
        return false
    }

    /// Sends the typed name when it changed; an empty one goes back to the list's (the engine refuses it).
    private func commitName() {
        guard let id = listID else { return }
        let text = nameField.stringValue
        if text.isEmpty || text == listName {
            nameField.stringValue = listName
            return
        }
        onRename?(id, text)
    }

    // MARK: Layout

    private func build() {
        nameField.setAX(AX.cueListInspectorName, label: loc("cuelist.col.name"))
        nameField.delegate = self
        nameField.font = .systemFont(ofSize: NSFont.systemFontSize)
        nameField.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        numberField.setAX("inspector-cue-list-number")
        let off: [NSControl] = [numberField, durationField, preWaitField, postWaitField, continuePopup]
        for f in [numberField, durationField, preWaitField, postWaitField] {
            f.isEnabled = false
            f.widthAnchor.constraint(equalToConstant: 80).isActive = true
            f.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        }
        for f in [durationField, preWaitField, postWaitField] { f.stringValue = CueTime.format(0) }
        continuePopup.addItem(withTitle: loc("inspector.continue.do_not_continue"))
        continuePopup.controlSize = .small
        continuePopup.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        continuePopup.widthAnchor.constraint(equalToConstant: 124).isActive = true
        for c in off { c.isEnabled = false }
        notesView.isEditable = false
        notesView.isSelectable = false
        notesView.drawsBackground = false
        notesView.textColor = .disabledControlTextColor
        notesView.autoresizingMask = [.width]
        for c in [flaggedCheck, autoLoadCheck, armedCheck] { c.isEnabled = false }
        targetNote.textColor = .secondaryLabelColor
        targetNote.font = .systemFont(ofSize: NSFont.smallSystemFontSize)

        func small(_ title: String) -> NSTextField {
            let label = NSTextField(labelWithString: title)
            label.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            label.textColor = .secondaryLabelColor
            return label
        }
        func labelled(_ title: String, _ control: NSView) -> NSView {
            let label = small(title)
            label.alignment = .right
            label.widthAnchor.constraint(equalToConstant: 52).isActive = true
            let s = NSStackView(views: [label, control])
            s.spacing = 6
            s.alignment = .firstBaseline
            return s
        }
        func row(_ title: String, _ control: NSView) -> NSView {
            let s = NSStackView(views: [small(title), control])
            s.orientation = .vertical
            s.alignment = .leading
            s.spacing = 2
            control.widthAnchor.constraint(equalTo: s.widthAnchor).isActive = true
            return s
        }
        func column(_ views: [NSView], width: CGFloat? = nil) -> NSStackView {
            let s = NSStackView(views: views)
            s.orientation = .vertical
            s.alignment = .leading
            s.spacing = 8
            if let width { s.widthAnchor.constraint(equalToConstant: width).isActive = true }
            return s
        }
        let left = column(
            [
                labelled(loc("cuelist.col.number"), numberField),
                labelled(loc("cuelist.col.duration"), durationField),
                labelled(loc("cuelist.col.preWait"), preWaitField),
                labelled(loc("cuelist.col.postWait"), postWaitField),
                labelled(loc("inspector.continue"), continuePopup),
            ], width: 182)
        let scroll = NSScrollView()
        scroll.documentView = notesView
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 48).isActive = true
        let centre = column([
            row(loc("cuelist.col.name"), nameField), row(loc("cuelist.col.target"), targetNote),
            row(loc("inspector.notes"), scroll),
        ])
        for v in centre.arrangedSubviews { v.widthAnchor.constraint(equalTo: centre.widthAnchor).isActive = true }
        let right = column([flaggedCheck, autoLoadCheck, armedCheck], width: 103)
        let basicsRow = NSStackView(views: [left, centre, right])
        basicsRow.orientation = .horizontal
        basicsRow.alignment = .top
        basicsRow.distribution = .fill
        basicsRow.spacing = 12
        // The middle column takes the width the other two leave, as the cue's Basics does.
        centre.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        nameField.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        scroll.setContentHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        left.setContentHuggingPriority(.required, for: .horizontal)
        right.setContentHuggingPriority(.required, for: .horizontal)
        basicsRow.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 8, right: 12)
        pin(basicsRow, in: basicsPane)

        triggersNote.textColor = .secondaryLabelColor
        triggersNote.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        triggersNote.translatesAutoresizingMaskIntoConstraints = false
        triggersPane.addSubview(triggersNote)
        NSLayoutConstraint.activate([
            triggersNote.topAnchor.constraint(equalTo: triggersPane.topAnchor, constant: 12),
            triggersNote.leadingAnchor.constraint(equalTo: triggersPane.leadingAnchor, constant: 12),
            triggersNote.trailingAnchor.constraint(lessThanOrEqualTo: triggersPane.trailingAnchor, constant: -12),
        ])

        tabBar.setAccessibilityIdentifier(AX.cueListInspectorTabs + "-bar")
        tabBar.onChoose = { [weak self] id in self?.choose(id) }
        for v in [tabBar, basicsPane, triggersPane] {
            v.translatesAutoresizingMaskIntoConstraints = false
            addSubview(v)
        }
        triggersPane.isHidden = true
        NSLayoutConstraint.activate([
            tabBar.topAnchor.constraint(equalTo: topAnchor),
            tabBar.leadingAnchor.constraint(equalTo: leadingAnchor),
            tabBar.trailingAnchor.constraint(equalTo: trailingAnchor),
            basicsPane.topAnchor.constraint(equalTo: tabBar.bottomAnchor),
            basicsPane.leadingAnchor.constraint(equalTo: leadingAnchor),
            basicsPane.trailingAnchor.constraint(equalTo: trailingAnchor),
            basicsPane.bottomAnchor.constraint(equalTo: bottomAnchor),
            triggersPane.topAnchor.constraint(equalTo: tabBar.bottomAnchor),
            triggersPane.leadingAnchor.constraint(equalTo: leadingAnchor),
            triggersPane.trailingAnchor.constraint(equalTo: trailingAnchor),
            triggersPane.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    private func pin(_ content: NSView, in pane: NSView) {
        content.translatesAutoresizingMaskIntoConstraints = false
        pane.addSubview(content)
        NSLayoutConstraint.activate([
            content.topAnchor.constraint(equalTo: pane.topAnchor),
            content.leadingAnchor.constraint(equalTo: pane.leadingAnchor),
            content.trailingAnchor.constraint(equalTo: pane.trailingAnchor),
            content.bottomAnchor.constraint(lessThanOrEqualTo: pane.bottomAnchor),
        ])
    }
}
