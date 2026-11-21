// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// WS-04: what the window says when a copy of media into the show folder ends: a short footer line
/// (how many files were copied, how many the folder already had, how many failed) and, for
/// failures, a list naming each cue (number, else name) and file with the reason, as the line's
/// tooltip and in the show log. Nothing modal: a copy may end during a show.
enum MediaCopyNotice {
    /// Whether a report is worth a word: the on-demand command always is; a copy made as files were
    /// added only when something failed or it was stopped.
    static func shown(_ report: MediaCopyReport) -> Bool {
        report.onDemand || report.failedCount > 0 || report.cancelled
    }

    static func line(_ report: MediaCopyReport) -> String {
        if report.cancelled { return loc("copyMedia.notice.stopped") }
        let already = report.reused + report.inFolder
        var text = String(format: loc("copyMedia.notice.copied"), Int(report.copied), Int(already))
        if report.failedCount > 0 {
            text += " " + String(format: loc("copyMedia.notice.failed"), Int(report.failedCount))
        }
        return text
    }

    /// One line per failure listed: the cue, the file's name and why.
    static func failures(_ report: MediaCopyReport, model: WorkspaceModel?) -> [String] {
        var lines = report.failed.map { failure in
            String(
                format: loc("copyMedia.notice.failure"), cueLabel(failure.cue, model: model),
                (failure.file as NSString).lastPathComponent, reason(failure.reason))
        }
        let unlisted = Int(report.failedCount) - report.failed.count
        if unlisted > 0 { lines.append(String(format: loc("copyMedia.notice.more"), unlisted)) }
        return lines
    }

    static func reason(_ code: String) -> String {
        switch code {
        case "missing", "unreadable", "disk_full", "read_only", "not_responding", "bad_name":
            return loc("copyMedia.reason.\(code)")
        default: return loc("copyMedia.reason.other")
        }
    }

    /// The cue's number, else its name; a cue deleted since is said so.
    private static func cueLabel(_ id: ModelID, model: WorkspaceModel?) -> String {
        guard let model, let cue = model.cue(id) else { return loc("copyMedia.notice.cueGone") }
        if let number = cue.number, !number.isEmpty { return number }
        return model.listName(cue)
    }
}
