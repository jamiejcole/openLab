// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The category list on the left of the workspace settings and the page bar above a category's pages
/// (QLab 5: a list of categories, each with one or more pages). Picking a category shows its first
/// page; the File menu opens a page directly (`show(page:)`).
extension WorkspaceSettingsWindowController: NSTableViewDataSource, NSTableViewDelegate {
    static let categoryWidth: CGFloat = 130

    func buildCategoryList() -> NSView {
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("category"))
        column.width = Self.categoryWidth - 4
        categoryTable.addTableColumn(column)
        categoryTable.headerView = nil
        categoryTable.rowHeight = 24
        categoryTable.style = .sourceList
        categoryTable.dataSource = self
        categoryTable.delegate = self
        categoryTable.setAX(AX.settingsCategories, label: loc("settings.title"))
        let scroll = NSScrollView()
        scroll.documentView = categoryTable
        scroll.hasVerticalScroller = false
        scroll.drawsBackground = false
        scroll.borderType = .bezelBorder
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.widthAnchor.constraint(equalToConstant: Self.categoryWidth).isActive = true
        return scroll
    }

    func buildPageBar() {
        pageBar.segmentStyle = .rounded
        pageBar.target = self
        pageBar.action = #selector(pageBarChosen)
        pageBar.setAX(AX.settingsPageBar, label: loc("settings.title"))
    }

    /// Shows `page` (and with it its category's page bar).
    func show(page: SettingsPage) {
        tabs.selectTabViewItem(withIdentifier: page.rawValue)
        syncCategories()
    }

    /// The page on show.
    var shownPage: SettingsPage? {
        tabs.selectedTabViewItem.flatMap { ($0.identifier as? String).flatMap(SettingsPage.init) }
    }

    /// Makes the list and the page bar say what the tab view shows.
    func syncCategories() {
        guard let page = shownPage else { return }
        let category = page.category
        syncingCategories = true
        defer { syncingCategories = false }
        if let row = SettingsCategory.allCases.firstIndex(of: category), categoryTable.selectedRow != row {
            categoryTable.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        }
        let pages = category.pages
        if pageBar.segmentCount != pages.count || pageBarCategory != category {
            pageBar.segmentCount = pages.count
            for (i, p) in pages.enumerated() { pageBar.setLabel(loc(p.titleKey), forSegment: i) }
            pageBarCategory = category
        }
        pageBar.selectedSegment = pages.firstIndex(of: page) ?? 0
        pageBar.isHidden = pages.count < 2
    }

    @objc private func pageBarChosen() {
        guard let category = shownPage?.category, category.pages.indices.contains(pageBar.selectedSegment) else {
            return
        }
        show(page: category.pages[pageBar.selectedSegment])
    }

    // MARK: Table

    func numberOfRows(in tableView: NSTableView) -> Int { SettingsCategory.allCases.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let category = SettingsCategory.allCases[row]
        let label = NSTextField(labelWithString: loc(category.titleKey))
        label.setAX("settings-category-\(category.rawValue)", label: loc(category.titleKey))
        label.lineBreakMode = .byTruncatingTail
        return label
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !syncingCategories, categoryTable.selectedRow >= 0 else { return }
        let category = SettingsCategory.allCases[categoryTable.selectedRow]
        // The category already on show keeps its page.
        if shownPage?.category == category { return }
        if let first = category.pages.first { show(page: first) }
    }
}
