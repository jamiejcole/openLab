// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// What an alert on the alarm's panel is about: the engine alarm (critical, with a beep) or the
/// offer of autosaved work after an unclean end (an ordinary alert).
enum EngineAlertKind { case alarm, recoveryOffer }

/// The engine alarm on screen. A protocol so tests record what is shown.
@MainActor
protocol EngineAlarmPresenting: AnyObject {
    func show(title: String, message: String, kind: EngineAlertKind)
    func setDetail(_ text: String)
    /// Shows (or, with nil, hides) the button that offers the autosaves.
    func setRestoreAction(_ action: (() -> Void)?)
    func dismiss()
    var isVisible: Bool { get }
}

extension EngineAlarmPresenting {
    func show(title: String, message: String) { show(title: title, message: message, kind: .alarm) }
}

/// The engine alarm: a standard alert window (a critical one for the alarm) that cannot be missed
/// (above every window, a critical Dock bounce, a beep) but is not modal and never becomes key on
/// its own, so transport keys and Esc keep working in the workspace windows while it is up.
@MainActor
final class EngineAlarmPanel: NSObject, EngineAlarmPresenting {
    private var alert: FloatingAlert?
    private var title = ""
    private var message = ""
    private var detail = ""
    private var kind = EngineAlertKind.alarm
    private var restoreAction: (() -> Void)?

    /// The alert's window while one is up.
    var panel: NSWindow? { alert?.window }
    /// The standard alert's style (a critical one for the alarm).
    var alertStyle: NSAlert.Style? { alert?.alert.alertStyle }

    func show(title: String, message: String, kind: EngineAlertKind) {
        self.title = title
        self.message = message
        self.kind = kind
        detail = ""
        render()
        if kind == .alarm {
            NSSound.beep()
            NSApp.requestUserAttention(.criticalRequest)
        }
    }

    func setDetail(_ text: String) {
        detail = text
        if isVisible { render() }
    }

    func setRestoreAction(_ action: (() -> Void)?) {
        restoreAction = action
        // Clearing the button never brings back an alert the operator dismissed.
        if action != nil || isVisible { render() }
    }

    func dismiss() {
        alert?.window.orderOut(nil)
        alert = nil
        restoreAction = nil
    }

    var isVisible: Bool { alert?.window.isVisible == true }

    /// Builds the alert for what is to say now, in the place of the one up (a standard alert's
    /// layout is fixed when it is built).
    private func render() {
        var buttons: [FloatingAlert.Button] = []
        if restoreAction != nil {
            buttons.append(
                .init(
                    title: loc("engine.alarm.restore"), id: AX.engineAlarmRestore,
                    action: { [weak self] in
                        self?.restoreClicked()
                    }))
        }
        let dismissKey = kind == .recoveryOffer ? "engine.alarm.notNow" : "engine.alarm.dismiss"
        buttons.append(
            .init(title: loc(dismissKey), id: AX.engineAlarmDismiss, action: { [weak self] in self?.dismiss() }))
        let next = FloatingAlert(
            critical: kind == .alarm, title: title, message: message, detail: detail, buttons: buttons,
            messageIDs: (AX.engineAlarmTitle, AX.engineAlarmMessage, AX.engineAlarmDetail))
        next.window.identifier = NSUserInterfaceItemIdentifier(AX.engineAlarm)
        if let old = alert?.window, old.isVisible {
            next.window.setFrameTopLeftPoint(NSPoint(x: old.frame.minX, y: old.frame.maxY))
            old.orderOut(nil)
        } else {
            next.window.center()
        }
        alert = next
        next.window.orderFrontRegardless()
    }

    private func restoreClicked() {
        let action = restoreAction
        setRestoreAction(nil)
        action?()
    }
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
    private var reattach: (failures: [ReattachFailure], restored: [String], editsLost: [String], ids: [ModelID])?
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

    /// The "not responding", "still stopping" or "did not start in time" alarm is up: it tells the operator about an engine
    /// that may still be running, so it goes away when an engine answers (no other alarm is taken
    /// down by a connection).
    private var notRespondingShown = false

    private func alarm(_ alarm: EngineSupervisor.Alarm) {
        notRespondingShown = false
        switch alarm {
        case .engineStopped(let crashed):
            presenter.show(
                title: loc(crashed ? "engine.alarm.crashed" : "engine.alarm.stopped"),
                message: loc("engine.alarm.restarting"))
        case .cannotStart(let reason):
            presenter.show(title: loc("engine.alarm.cannotStart"), message: reason)
        case .notResponding:
            notRespondingShown = true
            presenter.show(title: loc("engine.alarm.notResponding"), message: loc("engine.alarm.notRespondingMessage"))
        case .newEngineNotAnswering:
            notRespondingShown = true
            presenter.show(title: loc("engine.alarm.notResponding"), message: loc("engine.alarm.newEngineNotAnswering"))
        case .startTimedOut:
            notRespondingShown = true
            presenter.show(title: loc("engine.alarm.cannotStart"), message: loc("engine.alarm.startTimeout"))
        case .stillStopping:
            notRespondingShown = true
            presenter.show(title: loc("engine.alarm.stillStopping"), message: loc("engine.alarm.stillStoppingMessage"))
        }
        presenter.setRestoreAction(nil)
        summary = ""
        reattach = nil
    }

    /// A reattach is reported when it ends; with nothing to reattach the offer comes now.
    private func engineReady() {
        if notRespondingShown {
            notRespondingShown = false
            presenter.dismiss()
        }
        if !session.isReattaching { checkRecovery() }
    }

    private func sessionEvent(_ event: SessionEvent) {
        guard case .reattached(let failures, let restored, let editsLost, let editsLostIDs) = event else { return }
        reattach = (failures, restored, editsLost, editsLostIDs)
        summary = reattachSummary(setAside: [])
        if !isShown {
            presenter.show(title: loc("engine.alarm.restarted"), message: loc("engine.alarm.restartedMessage"))
        }
        presenter.setDetail(summary)
        checkRecovery()
    }

    /// What the reattach reported. A workspace reopened from its file that lost its unsaved edits
    /// has the earlier autosave set aside when one is offered (`setAside`, by workspace id): saying
    /// the edits are lost then would keep the operator from pressing Restore (#360).
    private func reattachSummary(setAside: Set<ModelID>) -> String {
        guard let r = reattach else { return "" }
        var lines = [loc("engine.alarm.back")]
        for name in r.restored { lines.append(String(format: loc("engine.alarm.restoredOne"), name)) }
        for (i, name) in r.editsLost.enumerated() {
            let set = i < r.ids.count && setAside.contains(r.ids[i])
            let key = set ? "engine.alarm.editsLostSetAside" : "engine.alarm.editsLostOne"
            lines.append(String(format: loc(key), name))
        }
        for f in r.failures { lines.append(String(format: loc("engine.alarm.failedOne"), f.name, f.reason)) }
        return lines.joined(separator: "\n")
    }

    /// After an unclean exit: the autosaves no open window holds, offered in an alert about recovering
    /// unsaved changes. The unclean end itself raises nothing (it is in the show log and Warnings).
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
            // Offered when the button is pressed: a workspace opened since (the launch choice
            // reopened it) is not opened a second time.
            if !self.isShown {
                self.presenter.show(
                    title: loc("engine.alarm.recover"), message: loc("engine.alarm.recoverMessage"),
                    kind: .recoveryOffer)
            }
            self.summary = self.reattachSummary(setAside: Set(offer.filter(\.stale).map(\.workspaceId)))
            self.offered = entries
            self.showAutosaveLine(count: offer.count)
            self.presenter.setRestoreAction { [weak self] in
                Task {
                    guard let self else { return }
                    await self.restore(self.unheld(entries))
                }
            }
        }
    }

    /// The scan result behind the alarm's autosave line (nil: none was offered).
    private var offered: [RecoveryEntryInfo]?

    private func showAutosaveLine(count: Int) {
        let line = count == 0 ? loc("recover.none") : String(format: loc("engine.alarm.autosaves"), count)
        presenter.setDetail(summary.isEmpty ? line : summary + "\n" + line)
    }

    /// The launch choice may have opened workspaces the offer named: count again what is left.
    func refreshOffer() {
        guard let entries = offered else { return }
        let left = unheld(entries).count
        showAutosaveLine(count: left)
        if left == 0 { presenter.setRestoreAction(nil) }
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
        let listed = Dictionary(session.workspaces.map { ($0.doc, $0) }, uniquingKeysWith: { a, _ in a })
        // Untitled only when the path is known to be none: the store's own document, or the list.
        let shown = session.stores.compactMap { doc, store -> AutosaveOffer.Holder? in
            guard store.model?.id != nil || listed[doc] != nil else { return nil }
            let info = session.info(of: store, doc: doc)
            let known = store.document != nil || listed[doc] != nil
            return AutosaveOffer.Holder(
                workspace: info.workspaceId, pathHash: info.pathHash, untitled: known && info.path == nil)
        }
        let held = Set(
            shown
                + session.workspaces.map {
                    AutosaveOffer.Holder(workspace: $0.workspaceId, pathHash: $0.pathHash, untitled: $0.path == nil)
                }
                + session.detached.map {
                    AutosaveOffer.Holder(
                        workspace: $0.info.workspaceId, pathHash: $0.info.pathHash,
                        untitled: $0.info.path == nil && $0.store.document != nil)
                })
        let windows =
            session.stores.map { doc, store in
                AutosaveOffer.Window(workspace: store.model?.id ?? listed[doc]?.workspaceId, cover: store.autosaveCover)
            }
            + session.detached.map {
                AutosaveOffer.Window(workspace: $0.info.workspaceId, cover: $0.store.autosaveCover)
            }
        return AutosaveOffer.offerable(entries, held: held, windows: windows)
    }

    /// Delete on a set-aside autosave: after a confirmation, the engine removes that one file.
    private func delete(_ entry: RecoveryEntryInfo, name: String) async {
        guard alerts().confirmDeleteAutosave(name: name) else { return }
        switch await session.discardRecovery(entry) {
        case .success:
            offered?.removeAll { $0 == entry }
            refreshOffer()
        case .failure(let e):
            alerts().showError(String(format: loc("error.discardAutosaveFailed"), EngineSession.describe(e)))
        }
    }

    /// One prompt per autosave, newest first, while there is room (at most 8 workspaces).
    func restore(_ entries: [RecoveryEntryInfo]) async {
        for entry in entries {
            guard session.stores.count < 8 else { break }
            // The prompts are modal one after another: a workspace opened meanwhile is not offered.
            guard !unheld([entry]).isEmpty else { continue }
            let name = entry.showName ?? loc("engine.alarm.untitled")
            switch alerts().offerAutosave(name: name, modifiedMs: entry.modifiedMs, setAside: entry.stale) {
            case .skip: continue
            case .delete:
                await delete(entry, name: name)
                continue
            case .restore: break
            }
            if case .failure(let e) = await session.openRecovery(entry) {
                alerts().showError(String(format: loc("error.openFailed"), EngineSession.describe(e)))
            }
        }
    }
}
