// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Bottom inspector. Shows the empty state until selection exists; tabs come in M5.6+.
final class InspectorViewController: NSViewController {
    let emptyLabel = NSTextField(labelWithString: loc("inspector.empty"))

    override func loadView() {
        let root = NSView()
        root.setAX(AX.inspector)
        emptyLabel.setAX(AX.inspectorEmpty)
        emptyLabel.font = .systemFont(ofSize: 24, weight: .medium)
        emptyLabel.textColor = .tertiaryLabelColor
        emptyLabel.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(emptyLabel)
        NSLayoutConstraint.activate([
            emptyLabel.centerXAnchor.constraint(equalTo: root.centerXAnchor),
            emptyLabel.centerYAnchor.constraint(equalTo: root.centerYAnchor),
            root.heightAnchor.constraint(greaterThanOrEqualToConstant: 100),
        ])
        view = root
    }
}
