// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Collapsible right sidebar: transport strip, list tabs, new-list buttons. All inert for now.
final class SidebarViewController: NSViewController {
    private static let transport: [(id: String, symbol: String, key: String)] = [
        (AX.transportReset, "stop.circle", "transport.reset"),
        (AX.transportPause, "pause.circle", "transport.pause"),
        (AX.transportResume, "play.circle", "transport.resume"),
        (AX.transportPanic, "exclamationmark.octagon", "transport.panic"),
    ]

    static var symbolNames: [String] { transport.map(\.symbol) }

    let tabs = NSSegmentedControl(
        labels: [loc("sidebar.tab.cueLists"), loc("sidebar.tab.activeCues")],
        trackingMode: .selectOne, target: nil, action: nil)
    let newListButton = NSButton(title: loc("sidebar.newList"), target: nil, action: nil)
    let newCartButton = NSButton(title: loc("sidebar.newCart"), target: nil, action: nil)

    override func loadView() {
        let root = NSView()
        root.setAX(AX.sidebar)

        let strip = NSStackView()
        strip.distribution = .fillEqually
        strip.spacing = 6
        for entry in Self.transport {
            let name = loc(entry.key)
            let button = NSButton(
                image: symbolImage(entry.symbol, name) ?? NSImage(), target: nil, action: nil)
            button.setAX(entry.id, label: name)
            button.toolTip = name
            button.bezelStyle = .texturedRounded
            button.imageScaling = .scaleProportionallyDown
            button.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 22, weight: .regular)
            button.heightAnchor.constraint(equalToConstant: 44).isActive = true
            strip.addArrangedSubview(button)
        }

        tabs.selectedSegment = 0
        tabs.setAX(AX.sidebarTabs, label: loc("sidebar.tabs.label"))

        let table = NSTableView()
        table.addTableColumn(NSTableColumn(identifier: NSUserInterfaceItemIdentifier("sidebar")))
        table.headerView = nil
        table.setAX(AX.sidebarList, label: loc("sidebar.list.label"))
        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder

        newListButton.setAX(AX.newListButton)
        newCartButton.setAX(AX.newCartButton)
        for b in [newListButton, newCartButton] {
            b.bezelStyle = .rounded
            b.isEnabled = false  // list management is not built yet
        }
        let bottom = NSStackView(views: [newListButton, newCartButton])
        bottom.distribution = .fillEqually

        let column = NSStackView(views: [strip, tabs, scroll, bottom])
        column.orientation = .vertical
        column.spacing = 8
        column.edgeInsets = NSEdgeInsets(top: 10, left: 10, bottom: 10, right: 10)
        column.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(column)
        NSLayoutConstraint.activate([
            column.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            column.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            column.topAnchor.constraint(equalTo: root.topAnchor),
            column.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            root.widthAnchor.constraint(greaterThanOrEqualToConstant: 220),
        ])
        for v in [strip, tabs, scroll, bottom] {
            v.widthAnchor.constraint(equalTo: column.widthAnchor, constant: -20).isActive = true
        }
        view = root
    }
}
