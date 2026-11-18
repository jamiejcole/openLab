// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// GO button, the standing-by cue as "number · name" on one line, and its notes. As in QLab the
/// notes are editable in Edit Mode and read-only in Show Mode.
final class MastheadView: NSView, NSTextViewDelegate {
    let goButton = NSButton(title: loc("masthead.go"), target: nil, action: nil)
    let standbyField = StandbyField()
    let notesView = NotesTextView()
    /// The cue the masthead shows; nil when nothing stands by.
    private(set) var cueID: ModelID?
    /// Show Mode (or no cue): the notes cannot be typed in.
    var isLocked = false { didSet { applyEditable() } }
    /// The typed notes for the shown cue; the window sends them the way the inspector does.
    var onCommitNotes: ((ModelID, String) -> Void)?
    /// Show Mode: bigger GO, standby and notes (SM-01), the same colours.
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
        goButton.layer?.cornerRadius = 4
        goButton.layer?.borderWidth = 3

        standbyField.stringValue = loc("masthead.standby.none")
        standbyField.setAX(AX.standbyField)
        standbyField.lineBreakMode = .byTruncatingTail
        standbyField.maximumNumberOfLines = 1
        // A long name is cut short with an ellipsis; it never widens the window to fit.
        standbyField.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)

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
        notesScroll.borderType = .noBorder
        notesScroll.wantsLayer = true
        notesScroll.layer?.cornerRadius = 3
        notesScroll.layer?.borderWidth = 1
        notesView.placeholder = loc("masthead.notes.placeholder")
        notesView.textContainerInset = NSSize(width: 4, height: 3)

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
        // Edit Mode and Show Mode look the same (QLab's do); Show Mode only makes them larger.
        goButton.attributedTitle = NSAttributedString(
            string: loc("masthead.go"),
            attributes: [
                .font: NSFont.systemFont(ofSize: m.goFontSize, weight: .regular), .foregroundColor: Self.goText,
            ])
        standbyField.font = .systemFont(ofSize: m.standbyFontSize)
        standbyField.textColor = .labelColor
        notesView.font = .systemFont(ofSize: m.notesFontSize)
        notesView.textColor = .labelColor
        applyColours()
        needsLayout = true
    }

    /// The GO button's text: white on its grey-violet fill in both appearances.
    static let goText = NSColor.white
    /// QLab's GO button: a grey-violet face with a green edge. Dark enough under white text to read in the
    /// light appearance too.
    static let goFill = NSColor(srgbRed: 0.43, green: 0.43, blue: 0.56, alpha: 1)
    /// The standby line's and notes' boxes: a step off the window's background in either appearance.
    static let boxFill = NSColor(name: nil) { a in
        a.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
            ? NSColor(white: 0.30, alpha: 1) : NSColor(white: 0.90, alpha: 1)
    }

    /// Layer colours are fixed when set: they are resolved again whenever the appearance changes.
    private func applyColours() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            goButton.layer?.backgroundColor = Self.goFill.cgColor
            goButton.layer?.borderColor = NSColor.systemGreen.cgColor
            standbyField.layer?.backgroundColor = Self.boxFill.cgColor
            notesScroll.layer?.backgroundColor = Self.boxFill.cgColor
            notesScroll.layer?.borderColor = NSColor.separatorColor.cgColor
        }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyColours()
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

    /// Typing ended (or its text was taken): the masthead may now show the standby cue that came
    /// while the operator typed. The window re-reads it, since the cue may have changed or gone.
    var onEditingEnded: (() -> Void)?
    /// The text last handed over by `pendingNotes`.
    private var handedOver: String?

    /// Shows `cue` as standing by. While notes are being typed for another cue the masthead keeps
    /// showing that cue, the text and the focus: a Space typed next must go into the text, not fire
    /// GO. The new standby cue is shown once typing ends.
    func show(standby cue: Cue?, name: String? = nil) {
        if let editing = editingID, editing != cue?.id { return }
        let changed = cueID != cue?.id
        cueID = cue?.id
        if let cue {
            standbyField.stringValue = [cue.number, name ?? CueKindStyle.kindLabel(cue.kindType)].compactMap { $0 }
                .filter { !$0.isEmpty }
                .joined(separator: " \u{00B7} ")
        } else {
            standbyField.stringValue = loc("masthead.standby.none")
        }
        let notes = cue?.raw["notes"]?.stringValue ?? ""
        if notesView.string != notes, changed || editingID == nil { notesView.string = notes }
        applyEditable()
    }

    /// Escape, Return and Tab end editing (the text is committed by the end-of-editing that follows)
    /// and give focus back, so Space fires GO again and the arrow keys work. Shift-Return types a
    /// new line.
    func textView(_ textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard selector == #selector(NSResponder.cancelOperation(_:)) || textView.endsNotesTyping(selector) else {
            return false
        }
        window?.makeFirstResponder(nil)
        return true
    }

    func textDidBeginEditing(_ notification: Notification) {
        editingID = cueID
        handedOver = nil
    }

    func textDidEndEditing(_ notification: Notification) {
        guard let id = editingID else { return }
        editingID = nil
        if !isLocked { onCommitNotes?(id, notesView.string) }
        onEditingEnded?()
    }

    /// Notes typed and not yet handed over, for Show Mode, Save, Close, Quit and every edit command.
    /// The typing goes on (the focus stays, and AppKit reports no new start of editing), so the cue
    /// being typed for stays too: what is typed next goes to it, and the end of editing sends the
    /// whole text. Nil once handed over and unchanged, as sending it runs this again.
    func pendingNotes() -> (id: ModelID, text: String)? {
        guard let id = editingID, !isLocked, notesView.string != handedOver else { return nil }
        handedOver = notesView.string
        return (id, notesView.string)
    }

    /// The text handed over was refused or could not be sent, and the operator has the notice with
    /// Copy Text: the typing ends and the field shows what the cue holds, as the inspector's does.
    func endTypingAfterRefusal() {
        editingID = nil
        handedOver = nil
        if window?.firstResponder === notesView { window?.makeFirstResponder(nil) }
        onEditingEnded?()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }
}

/// The standby line: the cue's number and name in a filled box with room inside it.
final class StandbyField: NSTextField {
    private static let inset = NSSize(width: 6, height: 3)

    init() {
        super.init(frame: .zero)
        cell = StandbyCell(textCell: "")
        isEditable = false
        isSelectable = false
        isBordered = false
        drawsBackground = false
        wantsLayer = true
        layer?.cornerRadius = 3
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override var intrinsicContentSize: NSSize {
        let size = super.intrinsicContentSize
        return NSSize(width: size.width, height: size.height + 2 * Self.inset.height)
    }

    private final class StandbyCell: NSTextFieldCell {
        override func drawingRect(forBounds rect: NSRect) -> NSRect {
            super.drawingRect(forBounds: rect.insetBy(dx: StandbyField.inset.width, dy: StandbyField.inset.height))
        }
    }
}
