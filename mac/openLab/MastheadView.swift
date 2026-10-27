// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// GO button, standing-by cue name and its notes. Read-only until the store exists (M5.2+).
final class MastheadView: NSView {
    let goButton = NSButton(title: loc("masthead.go"), target: nil, action: nil)
    let standbyField = NSTextField(labelWithString: loc("masthead.standby.none"))
    let notesView = NSTextView()

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
        notesView.isEditable = false
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

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }
}
