// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// The workspace status window (QLab 5's Workspace Status, manual pp. 164-165; UI map `status.*`):
/// one window with a Warnings page (the pre-show check: what keeps the show from running, a row
/// selecting its cue), a Logs page (the show log) and an Info page (this workspace's name, id and
/// file). Window > Workspace Status (Shift-Command-W), Window > Log and a click on the footer's
/// warning mark all open it, each on its page. Like the log window it never takes the keyboard:
/// GO and the transport keys keep reaching the workspace window.
@MainActor
final class StatusWindowController: NSWindowController {
    enum Page: String, CaseIterable {
        case warnings, logs, info

        var titleKey: String { "status.page.\(rawValue)" }
    }

    let store: WorkspaceStore
    let warnings: PreshowWindowController
    let logs: LogWindowController
    let tabs = NSTabView(frame: NSRect(x: 0, y: 0, width: 600, height: 400))
    let nameValue = NSTextField(labelWithString: "")
    let idValue = NSTextField(labelWithString: "")
    let fileValue = NSTextField(labelWithString: "")
    let copyIDButton = NSButton(title: loc("status.info.copy"), target: nil, action: nil)
    /// Where the Copy button puts the id (a test gives its own pasteboard).
    var pasteboard = NSPasteboard.general
    private var subs: [AnyCancellable] = []

    init(store: WorkspaceStore, warnings: PreshowWindowController, logs: LogWindowController) {
        self.store = store
        self.warnings = warnings
        self.logs = logs
        let panel = LogPanel(
            contentRect: NSRect(x: 0, y: 0, width: 820, height: 480),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .utilityWindow, .nonactivatingPanel],
            backing: .buffered, defer: false)
        panel.hidesOnDeactivate = false
        panel.isFloatingPanel = true
        panel.title = loc("status.title")
        panel.identifier = NSUserInterfaceItemIdentifier(AX.statusWindow)
        panel.isReleasedWhenClosed = false
        panel.minSize = NSSize(width: 520, height: 300)
        super.init(window: panel)
        // The pages keep their own controllers (their lists, filters and checks) but not their
        // windows: their content is taken in here, and the log reads "open" from this window.
        for (page, controller) in [(Page.warnings, warnings as NSWindowController), (.logs, logs)] {
            let item = NSTabViewItem(identifier: page.rawValue)
            item.label = loc(page.titleKey)
            item.view = Self.adopt(controller)
            tabs.addTabViewItem(item)
        }
        let info = NSTabViewItem(identifier: Page.info.rawValue)
        info.label = loc(Page.info.titleKey)
        info.view = buildInfo()
        tabs.addTabViewItem(info)
        tabs.setAX(AX.statusTabs)
        panel.contentView = tabs
        panel.center()
        logs.isOpen = { [weak self] in self?.window?.isVisible == true }
        subs.append(store.$model.sink { [weak self] _ in self?.renderInfo() })
        subs.append(store.$document.sink { [weak self] _ in self?.renderInfo() })
        renderInfo()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// The controller's content view, moved out of its window (which is never shown).
    private static func adopt(_ controller: NSWindowController) -> NSView {
        let view = controller.window?.contentView ?? NSView()
        controller.window?.contentView = NSView()
        return view
    }

    /// The page on show.
    var page: Page? {
        tabs.selectedTabViewItem.flatMap { ($0.identifier as? String).flatMap(Page.init) }
    }

    /// Shows the window on `page` without taking the keyboard, and reads what the page shows: the
    /// pre-show check runs, the log reads the engine's tail. The task's value tells tests it is done.
    @discardableResult
    func present(_ page: Page) -> Task<Void, Never> {
        tabs.selectTabViewItem(withIdentifier: page.rawValue)
        window?.orderFront(nil)
        return Task {
            switch page {
            case .warnings: await warnings.runCheck()
            case .logs: await logs.refresh()
            case .info: break
            }
        }
    }

    /// Titles the window for its workspace.
    func retitle(_ workspace: String) {
        window?.title = String(format: loc("status.title.workspace"), workspace)
    }

    // MARK: Info page

    private func buildInfo() -> NSView {
        func title(_ key: String) -> NSTextField {
            let label = NSTextField(labelWithString: loc(key))
            label.font = .systemFont(ofSize: 13, weight: .semibold)
            return label
        }
        let fields = [
            (nameValue, "status-info-name"), (idValue, "status-info-id"), (fileValue, "status-info-file"),
        ]
        for (field, id) in fields {
            field.setAX(id)
            field.isSelectable = true
            field.lineBreakMode = .byTruncatingMiddle
            field.setContentCompressionResistancePriority(NSLayoutConstraint.Priority(1), for: .horizontal)
        }
        copyIDButton.bezelStyle = .rounded
        copyIDButton.target = self
        copyIDButton.action = #selector(copyID)
        copyIDButton.setAX("status-info-copy-id")
        let note = NSTextField(wrappingLabelWithString: loc("status.info.idNote"))
        note.font = .systemFont(ofSize: 11)
        note.textColor = .secondaryLabelColor
        note.preferredMaxLayoutWidth = 460
        let grid = NSGridView(views: [
            [title("status.info.name"), nameValue],
            [title("status.info.id"), NSStackView(views: [idValue, copyIDButton])],
            [title("status.info.file"), fileValue],
        ])
        grid.columnSpacing = 12
        grid.rowSpacing = 10
        grid.column(at: 0).xPlacement = .trailing
        let stack = NSStackView(views: [grid, note])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        stack.edgeInsets = NSEdgeInsets(top: 20, left: 20, bottom: 20, right: 20)
        let page = NSView()
        stack.translatesAutoresizingMaskIntoConstraints = false
        page.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: page.topAnchor),
            stack.leadingAnchor.constraint(equalTo: page.leadingAnchor),
            stack.trailingAnchor.constraint(lessThanOrEqualTo: page.trailingAnchor),
        ])
        return page
    }

    /// The workspace id as OSC replies and addresses write it (upper case).
    var workspaceID: String? { store.model?.id.raw.uppercased() }

    private func renderInfo() {
        nameValue.stringValue = store.model?.name ?? ""
        idValue.stringValue = workspaceID ?? ""
        fileValue.stringValue = store.document?.path ?? loc("status.info.notSaved")
        copyIDButton.isEnabled = workspaceID != nil
    }

    @objc private func copyID() {
        guard let id = workspaceID else { return }
        pasteboard.clearContents()
        pasteboard.setString(id, forType: .string)
    }
}
