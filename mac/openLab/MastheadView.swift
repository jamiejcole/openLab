// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// GO button, the standing-by cue as "number, name" on one line, and its notes. As in QLab the
/// notes are editable in Edit Mode and read-only in Show Mode.
final class MastheadView: NSView, NSTextViewDelegate {
    let goButton = NSButton(title: loc("masthead.go"), target: nil, action: nil)
    let standbyField = NSTextField(labelWithString: loc("masthead.standby.none"))
    let notesView = NSTextView()
    /// The cue the masthead shows; nil when nothing stands by.
    private(set) var cueID: ModelID?
    /// Show Mode (or no cue): the notes cannot be typed in.
    var isLocked = false { didSet { applyEditable() } }
    /// The typed notes for the shown cue; the window sends them the way the inspector does.
    var onCommitNotes: ((ModelID, String) -> Void)?

    override init(frame: NSRect) {
        super.init(frame: frame)
        setAX(AX.masthead)

        goButton.setAX(AX.goButton, label: loc("masthead.go.label"))
        goButton.isBordered = false
        goButton.wantsLayer = true
        goButton.layer?.cornerRadius = 8
        goButton.layer?.backgroundColor = NSColor.systemGreen.withAlphaComponent(0.5).cgColor
        goButton.attributedTitle = NSAttributedString(
            string: loc("masthead.go"),
            attributes: [
                .font: NSFont.systemFont(ofSize: 40, weight: .bold),
                .foregroundColor: NSColor.white,
            ])

        standbyField.setAX(AX.standbyField)
        standbyField.font = .systemFont(ofSize: 22)
        standbyField.textColor = .secondaryLabelColor
        standbyField.lineBreakMode = .byTruncatingTail
        standbyField.maximumNumberOfLines = 1

        notesView.setAX(AX.standbyNotes, label: loc("masthead.notes.label"))
        // Only editable in Edit Mode with a cue. Locked, it takes no focus: a focused text view
        // swallows the transport keys (Space = GO).
        notesView.isEditable = false
        notesView.isSelectable = false
        notesView.isRichText = false
        notesView.isAutomaticQuoteSubstitutionEnabled = false
        notesView.isAutomaticDashSubstitutionEnabled = false
        notesView.delegate = self
        notesView.drawsBackground = false
        notesView.font = .systemFont(ofSize: 13)
        notesView.textColor = .secondaryLabelColor
        let notesScroll = NSScrollView()
        notesScroll.documentView = notesView
        notesScroll.hasVerticalScroller = true
        notesScroll.drawsBackground = false
        notesScroll.borderType = .bezelBorder

        let right = NSStackView(views: [standbyField, notesScroll])
        right.orientation = .vertical
        right.alignment = .leading
        right.spacing = 6
        for v in [standbyField, notesScroll] { v.widthAnchor.constraint(equalTo: right.widthAnchor).isActive = true }

        right.setHuggingPriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        let row = NSStackView(views: [goButton, right])
        row.orientation = .horizontal
        row.alignment = .top
        row.spacing = 12
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            row.topAnchor.constraint(equalTo: topAnchor, constant: 10),
            row.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -10),
            right.trailingAnchor.constraint(equalTo: row.trailingAnchor),
            goButton.widthAnchor.constraint(equalToConstant: 130),
            goButton.heightAnchor.constraint(equalToConstant: 80),
            notesScroll.heightAnchor.constraint(equalToConstant: 44),
        ])
    }

    private func applyEditable() {
        let on = !isLocked && cueID != nil
        notesView.isEditable = on
        notesView.isSelectable = on
        if !on, notesView.window?.firstResponder === notesView { notesView.window?.makeFirstResponder(nil) }
    }

    /// The cue whose notes are being typed: fixed when typing starts, so the text always goes to
    /// the cue it was typed for even if the playhead moves meanwhile.
    private var editingID: ModelID?

    /// Shows `cue` as standing by. Notes being typed for another cue are committed to that cue first.
    func show(standby cue: Cue?) {
        if let editing = editingID, editing != cue?.id {
            editingID = nil
            onCommitNotes?(editing, notesView.string)
        }
        let changed = cueID != cue?.id
        cueID = cue?.id
        if let cue {
            standbyField.stringValue = [cue.number, cue.listName].compactMap { $0 }.filter { !$0.isEmpty }
                .joined(separator: ", ")
        } else {
            standbyField.stringValue = loc("masthead.standby.none")
        }
        let notes = cue?.raw["notes"]?.stringValue ?? ""
        if notesView.string != notes, changed || editingID == nil { notesView.string = notes }
        applyEditable()
    }

    func textDidBeginEditing(_ notification: Notification) {
        editingID = cueID
    }

    func textDidEndEditing(_ notification: Notification) {
        guard let id = editingID else { return }
        editingID = nil
        if !isLocked { onCommitNotes?(id, notesView.string) }
    }

    /// Notes typed and not yet sent, handed over (and forgotten) for Show Mode, Save, Close and Quit.
    func takePendingNotes() -> (id: ModelID, text: String)? {
        guard let id = editingID, !isLocked else { return nil }
        editingID = nil
        return (id, notesView.string)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }
}
