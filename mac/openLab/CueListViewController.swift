// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Empty cue list: columns only. Rows and cells arrive with M5.4.
final class CueListViewController: NSViewController {
    /// QLab order. The first and last columns carry no title.
    static let columns: [(id: String, titleKey: String?, width: CGFloat)] = [
        ("status", nil, 36), ("number", "cuelist.col.number", 60), ("name", "cuelist.col.name", 280),
        ("target", "cuelist.col.target", 160), ("preWait", "cuelist.col.preWait", 80),
        ("duration", "cuelist.col.duration", 80), ("postWait", "cuelist.col.postWait", 80),
        ("continue", nil, 36),
    ]

    let outlineView = NSOutlineView()

    override func loadView() {
        for spec in Self.columns {
            let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(spec.id))
            column.title = spec.titleKey.map(loc) ?? ""
            column.width = spec.width
            column.minWidth = 24
            outlineView.addTableColumn(column)
        }
        outlineView.outlineTableColumn = outlineView.tableColumns.first { $0.identifier.rawValue == "name" }
        outlineView.usesAlternatingRowBackgroundColors = true
        outlineView.allowsMultipleSelection = true
        outlineView.setAX(AX.cueList, label: loc("cuelist.label"))
        let scroll = NSScrollView()
        scroll.documentView = outlineView
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        view = scroll
    }
}
