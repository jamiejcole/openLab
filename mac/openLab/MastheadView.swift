// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// GO button, the standing-by cue as "number · name" on one line, and its notes. As in QLab the
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
    /// Show Mode: bigger GO, standby and notes and the high-contrast colours (SM-01, SM-03).
    private(set) var isShowMode = false
    private(set) var metrics = MastheadMetrics.make(showMode: false, window: NSSize(width: 1200, height: 800))
    private var heightConstraint: NSLayoutConstraint!
    private var goWidth: NSLayoutConstraint!
    private var goHeight: NSLayoutConstraint!
    private var notesHeight: NSLayoutConstraint!
    private let notesScroll = NSScrollView()

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        setAX(AX.masthead)

        goButton.setAX(AX.goButton, label: loc("masthead.go.label"))
        goButton.isBordered = false
        goButton.wantsLayer = true
        goButton.layer?.cornerRadius = 8
        goButton.layer?.backgroundColor = NSColor.systemGreen.withAlphaComponent(0.5).cgColor

        standbyField.setAX(AX.standbyField)
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
        heightConstraint = heightAnchor.constraint(equalToConstant: metrics.height)
        goWidth = goButton.widthAnchor.constraint(equalToConstant: metrics.goSize.width)
        goHeight = goButton.heightAnchor.constraint(equalToConstant: metrics.goSize.height)
        notesHeight = notesScroll.heightAnchor.constraint(equalToConstant: metrics.notesHeight)
        NSLayoutConstraint.activate([
            heightConstraint, goWidth, goHeight, notesHeight,
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            row.topAnchor.constraint(equalTo: topAnchor, constant: 10),
            row.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -10),
            right.trailingAnchor.constraint(equalTo: row.trailingAnchor),
        ])
        apply(showMode: false, window: NSSize(width: 1200, height: 800))
    }

    /// Sizes and colours for the mode and the window's content size (called on a mode change and
    /// on every resize).
    func apply(showMode: Bool, window size: NSSize) {
        isShowMode = showMode
        let m = MastheadMetrics.make(showMode: showMode, window: size)
        metrics = m
        heightConstraint.constant = m.height
        goWidth.constant = m.goSize.width
        goHeight.constant = m.goSize.height
        notesHeight.constant = m.notesHeight
        let p = ShowModePalette.self
        let goText = showMode ? p.goText.color : NSColor.white
        goButton.attributedTitle = NSAttributedString(
            string: loc("masthead.go"),
            attributes: [.font: NSFont.systemFont(ofSize: m.goFontSize, weight: .bold), .foregroundColor: goText])
        goButton.layer?.backgroundColor =
            (showMode ? p.goFill.color : NSColor.systemGreen.withAlphaComponent(0.5)).cgColor
        standbyField.font = .systemFont(ofSize: m.standbyFontSize, weight: showMode ? .semibold : .regular)
        standbyField.textColor = showMode ? p.standbyText.color : .secondaryLabelColor
        notesView.font = .systemFont(ofSize: m.notesFontSize)
        notesView.textColor = showMode ? p.notesText.color : .secondaryLabelColor
        layer?.backgroundColor = showMode ? p.background.color.cgColor : nil
        needsLayout = true
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
            // AppKit announces the start of typing once per focus session: leave the field so the
            // next keystroke starts a new session for the new cue instead of typing unseen.
            if notesView.window?.firstResponder === notesView { notesView.window?.makeFirstResponder(nil) }
        }
        let changed = cueID != cue?.id
        cueID = cue?.id
        if let cue {
            standbyField.stringValue = [cue.number, cue.listName].compactMap { $0 }.filter { !$0.isEmpty }
                .joined(separator: " \u{00B7} ")
        } else {
            standbyField.stringValue = loc("masthead.standby.none")
        }
        let notes = cue?.raw["notes"]?.stringValue ?? ""
        if notesView.string != notes, changed || editingID == nil { notesView.string = notes }
        applyEditable()
    }

    /// Escape ends editing (the text is committed by the end-of-editing that follows) and gives
    /// focus back, so Space fires GO again.
    func textView(_ textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard selector == #selector(NSResponder.cancelOperation(_:)) else { return false }
        window?.makeFirstResponder(nil)
        return true
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
