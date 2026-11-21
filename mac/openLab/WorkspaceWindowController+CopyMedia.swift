// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit
import Combine

/// WS-04: File > Copy Media into Show Folder (QLab 5's on-demand copy, manual p. 101) and the notice a
/// copy leaves when it ends. The engine copies on a thread of its own and points the cues at the
/// copies in one undo step; the window only asks and tells.
extension WorkspaceWindowController {
    /// The command can be chosen: a saved show (it has a folder), Edit Mode, and no on-demand copy
    /// of this show running.
    var canCopyMedia: Bool {
        !isShowMode && store.document?.path != nil && !store.copyOnDemandRunning
    }

    func copyMediaIntoShowFolder() {
        guard canCopyMedia else { return NSSound.beep() }
        Task { [weak self] in
            guard let self else { return }
            if case .failure(let e) = await self.store.copyMediaIntoShowFolder() {
                self.quietNotice(e.detailText, error: true)
            }
        }
    }

    /// Follows the store's copy reports for as long as the window lives.
    func watchMediaCopies() -> AnyCancellable {
        store.mediaCopied.sink { [weak self] report in
            MainActor.assumeIsolated { self?.showMediaCopied(report) }
        }
    }

    /// The footer line, its tooltip listing failures, and one show-log record per failure.
    func showMediaCopied(_ report: MediaCopyReport) {
        guard MediaCopyNotice.shown(report) else { return }
        let line = MediaCopyNotice.line(report)
        let failures = MediaCopyNotice.failures(report, model: store.model)
        for failure in failures { _ = store.receive(.log(.warn, failure)) }
        _ = store.receive(.log(report.failedCount > 0 ? .warn : .info, line))
        footer.showNotice(
            line, error: report.failedCount > 0 || report.cancelled,
            tooltip: failures.isEmpty ? nil : failures.joined(separator: "\n"))
    }
}
