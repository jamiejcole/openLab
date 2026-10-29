// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// The engine alarm on screen. A protocol so tests record what is shown.
@MainActor
protocol EngineAlarmPresenting: AnyObject {
    func show(title: String, message: String)
    func setDetail(_ text: String)
    /// Shows (or, with nil, hides) the button that offers the autosaves.
    func setRestoreAction(_ action: (() -> Void)?)
    func dismiss()
    var isVisible: Bool { get }
}

/// The engine alarm: a floating panel that cannot be missed (above every window, a critical
/// Dock bounce, a beep) but is not modal and never becomes key on its own, so transport keys and
/// Esc keep working in the workspace windows while it is up.
@MainActor
final class EngineAlarmPanel: NSObject, EngineAlarmPresenting {
    private(set) var panel: NSPanel?
    private let titleLabel = NSTextField(labelWithString: "")
    private let messageLabel = NSTextField(wrappingLabelWithString: "")
    private let detailLabel = NSTextField(wrappingLabelWithString: "")
    private var restoreButton: NSButton?
    private var restoreAction: (() -> Void)?

    func show(title: String, message: String) {
        let panel = self.panel ?? makePanel()
        titleLabel.stringValue = title
        messageLabel.stringValue = message
        detailLabel.stringValue = ""
        detailLabel.isHidden = true
        panel.orderFrontRegardless()
        NSSound.beep()
        NSApp.requestUserAttention(.criticalRequest)
    }

    func setDetail(_ text: String) {
        detailLabel.stringValue = text
        detailLabel.isHidden = text.isEmpty
    }

    func setRestoreAction(_ action: (() -> Void)?) {
        restoreAction = action
        restoreButton?.isHidden = action == nil
        if action != nil { panel?.orderFrontRegardless() }
    }

    func dismiss() {
        panel?.orderOut(nil)
        restoreAction = nil
    }

    var isVisible: Bool { panel?.isVisible == true }

    private func makePanel() -> NSPanel {
        let panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 420, height: 160),
            styleMask: [.titled, .utilityWindow, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.title = loc("engine.alarm.window")
        panel.identifier = NSUserInterfaceItemIdentifier(AX.engineAlarm)
        panel.level = .floating
        panel.isFloatingPanel = true
        panel.becomesKeyOnlyIfNeeded = true
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]

        let icon = NSImageView()
        if let image = NSImage(systemSymbolName: "exclamationmark.triangle.fill", accessibilityDescription: nil) {
            icon.image = image
            icon.contentTintColor = .systemRed
            icon.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 28, weight: .bold)
        }
        titleLabel.font = .boldSystemFont(ofSize: 15)
        titleLabel.textColor = .systemRed
        titleLabel.setAX(AX.engineAlarmTitle)
        messageLabel.setAX(AX.engineAlarmMessage)
        // Selectable text needs its panel to be key: a click on the message would take the keys
        // from the workspace window and GO would stop working while the engine is down.
        messageLabel.isSelectable = false
        detailLabel.isSelectable = false
        detailLabel.font = .systemFont(ofSize: 11)
        detailLabel.textColor = .secondaryLabelColor
        detailLabel.setAX(AX.engineAlarmDetail)

        let restore = NSButton(title: loc("engine.alarm.restore"), target: self, action: #selector(restoreClicked))
        restore.setAX(AX.engineAlarmRestore)
        restore.isHidden = true
        restoreButton = restore
        let dismiss = NSButton(title: loc("engine.alarm.dismiss"), target: self, action: #selector(dismissClicked))
        dismiss.setAX(AX.engineAlarmDismiss)
        let buttons = NSStackView(views: [restore, dismiss])
        buttons.orientation = .horizontal

        let text = NSStackView(views: [titleLabel, messageLabel, detailLabel, buttons])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 8
        let row = NSStackView(views: [icon, text])
        row.orientation = .horizontal
        row.alignment = .top
        row.spacing = 12
        row.edgeInsets = NSEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        panel.contentView = row
        panel.center()
        self.panel = panel
        return panel
    }

    @objc private func restoreClicked() {
        let action = restoreAction
        setRestoreAction(nil)
        action?()
    }

    @objc private func dismissClicked() { dismiss() }
}

/// Joins the supervisor and the session to the alarm: shows it when the engine is gone, says
/// what came back when the new engine has reopened the windows, and offers the autosaves the
/// windows did not take (NFR-05). It never runs a modal on its own: the operator opens the
/// autosave prompts with the alarm's button.
@MainActor
final class EngineAlarmController {
    let presenter: EngineAlarmPresenting
    private let session: EngineSession
    private let supervisor: EngineSupervisor
    private let alerts: @MainActor () -> WorkspaceAlerts
    private var subs: [AnyCancellable] = []
    private var isShown: Bool { presenter.isVisible }
    /// What the last reattach reported (kept when the autosave line is added).
    private var summary = ""
    /// The latest offer check (tests await it).
    private(set) var offerTask: Task<Void, Never>?

    init(
        session: EngineSession, supervisor: EngineSupervisor, presenter: EngineAlarmPresenting,
        alerts: @escaping @MainActor () -> WorkspaceAlerts
    ) {
        self.session = session
        self.supervisor = supervisor
        self.presenter = presenter
        self.alerts = alerts
        subs.append(supervisor.alarms.sink { [weak self] in self?.alarm($0) })
        subs.append(supervisor.ready.sink { [weak self] in self?.engineReady() })
        subs.append(session.events.sink { [weak self] in self?.sessionEvent($0) })
    }

    private func alarm(_ alarm: EngineSupervisor.Alarm) {
        switch alarm {
        case .engineStopped(let crashed):
            presenter.show(
                title: loc(crashed ? "engine.alarm.crashed" : "engine.alarm.stopped"),
                message: loc("engine.alarm.restarting"))
        case .cannotStart(let reason):
            presenter.show(title: loc("engine.alarm.cannotStart"), message: reason)
        }
        presenter.setRestoreAction(nil)
        summary = ""
    }

    /// A reattach is reported when it ends; with nothing to reattach the offer comes now.
    private func engineReady() {
        if !session.isReattaching { checkRecovery() }
    }

    private func sessionEvent(_ event: SessionEvent) {
        guard case .reattached(let failures, let restored, let editsLost) = event else { return }
        var lines = [loc("engine.alarm.back")]
        for name in restored { lines.append(String(format: loc("engine.alarm.restoredOne"), name)) }
        for name in editsLost { lines.append(String(format: loc("engine.alarm.editsLostOne"), name)) }
        for f in failures { lines.append(String(format: loc("engine.alarm.failedOne"), f.name, f.reason)) }
        if !isShown {
            presenter.show(title: loc("engine.alarm.restarted"), message: loc("engine.alarm.restartedMessage"))
        }
        summary = lines.joined(separator: "\n")
        presenter.setDetail(summary)
        checkRecovery()
    }

    /// After an unclean exit: the autosaves no open window holds, behind the alarm's button.
    private func checkRecovery() {
        guard supervisor.recoveryPending else { return }
        supervisor.recoveryPending = false
        offerTask = Task { [weak self] in
            guard let self else { return }
            let entries: [RecoveryEntryInfo]
            switch await self.session.scanRecovery() {
            case .success(let e): entries = e
            case .failure(let e):
                engineLog.error("recovery scan failed: \(String(describing: e), privacy: .public)")
                return
            }
            let offer = self.unheld(entries)
            guard !offer.isEmpty else { return }
            if !self.isShown {
                self.presenter.show(title: loc("engine.alarm.unclean"), message: loc("engine.alarm.uncleanMessage"))
            }
            let line = String(format: loc("engine.alarm.autosaves"), offer.count)
            self.presenter.setDetail(self.summary.isEmpty ? line : self.summary + "\n" + line)
            self.presenter.setRestoreAction { [weak self] in Task { await self?.restore(offer) } }
        }
    }

    /// File > Recover Autosaves…: the autosaves no window holds, whenever the operator asks, so
    /// one dismissed with the alarm (or never offered: a quit during a reattach) stays reachable,
    /// an untitled workspace's included.
    func recoverAutosaves() async {
        switch await session.scanRecovery() {
        case .success(let entries):
            let offer = unheld(entries)
            if offer.isEmpty { return alerts().showInfo(loc("recover.none")) }
            await restore(offer)
        case .failure(let e):
            alerts().showError(String(format: loc("error.recoverFailed"), EngineSession.describe(e)))
        }
    }

    /// The entries no window holds: see `AutosaveOffer.offerable`.
    private func unheld(_ entries: [RecoveryEntryInfo]) -> [RecoveryEntryInfo] {
        let held = Set(
            session.stores.values.compactMap { $0.model?.id } + session.workspaces.map(\.workspaceId)
                + session.detached.map(\.info.workspaceId))
        let listed = Dictionary(session.workspaces.map { ($0.doc, $0.workspaceId) }, uniquingKeysWith: { a, _ in a })
        let windows =
            session.stores.map { doc, store in
                AutosaveOffer.Window(workspace: store.model?.id ?? listed[doc], cover: store.autosaveCover)
            }
            + session.detached.map {
                AutosaveOffer.Window(workspace: $0.info.workspaceId, cover: $0.store.autosaveCover)
            }
        return AutosaveOffer.offerable(entries, held: held, windows: windows)
    }

    /// One prompt per autosave, newest first, while there is room (at most 8 workspaces, §21).
    func restore(_ entries: [RecoveryEntryInfo]) async {
        for entry in entries {
            guard session.stores.count < 8 else { break }
            let name = entry.showName ?? loc("engine.alarm.untitled")
            guard alerts().offerAutosave(name: name, modifiedMs: entry.modifiedMs, setAside: entry.stale) else {
                continue
            }
            if case .failure(let e) = await session.openRecovery(entry) {
                alerts().showError(String(format: loc("error.openFailed"), EngineSession.describe(e)))
            }
        }
    }
}
