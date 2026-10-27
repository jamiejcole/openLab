// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// How the engine's `status` (DX-03) is shown: one compact line for the footer and a list of
/// details for the popover. Severity is carried by the symbol and by a word in the text ("Alarm"),
/// never by colour alone (colour only reinforces it).
struct StatusPresentation: Equatable {
    enum Level: Int, Comparable {
        case normal, warning, alarm
        static func < (a: Level, b: Level) -> Bool { a.rawValue < b.rawValue }
    }

    let level: Level
    /// The footer text.
    let summary: String
    let symbol: String
    let details: [String]

    static func make(_ status: SystemStatus?) -> StatusPresentation {
        guard let s = status else {
            return StatusPresentation(
                level: .normal, summary: loc("status.waiting"), symbol: "circle.dashed", details: [])
        }
        // The worst problem is named in the footer; the rest are in the details.
        var problems: [(Level, String)] = []
        switch s.device.state {
        case .ok: break
        case .lost: problems.append((.alarm, loc("status.device.lost")))
        case .failed: problems.append((.alarm, loc("status.device.failed")))
        case .absent: problems.append((.warning, loc("status.device.absent")))
        default: problems.append((.warning, String(format: loc("status.device.other"), s.device.state.raw)))
        }
        if !s.disk.healthy { problems.append((.alarm, loc("status.disk.unhealthy"))) }
        if let load = s.engineLoad, load >= highLoad { problems.append((.warning, loc("status.load.high"))) }
        if s.xruns > 0 { problems.append((.warning, String(format: loc("status.xruns"), Int(s.xruns)))) }

        let level = problems.map(\.0).max() ?? .normal
        let summary: String
        switch level {
        case .alarm: summary = loc("status.alarm") + " " + (problems.first { $0.0 == .alarm }?.1 ?? "")
        case .warning: summary = loc("status.warning") + " " + (problems.first { $0.0 == .warning }?.1 ?? "")
        case .normal: summary = normalSummary(s)
        }
        let symbol =
            level == .alarm
            ? "exclamationmark.octagon.fill" : level == .warning ? "exclamationmark.triangle.fill" : "checkmark.circle"
        return StatusPresentation(level: level, summary: summary, symbol: symbol, details: details(s, problems))
    }

    /// Engine load at or above this fraction of the block time is flagged (the callback is close
    /// to missing its deadline).
    static let highLoad = 0.8

    private static func normalSummary(_ s: SystemStatus) -> String {
        var parts = [loc("status.ok")]
        if let cpu = s.cpuPercent { parts.append(String(format: loc("status.cpu"), cpu)) }
        return parts.joined(separator: "  ")
    }

    private static func details(_ s: SystemStatus, _ problems: [(Level, String)]) -> [String] {
        var out = problems.map(\.1)
        let d = s.device
        out.append(String(format: loc("status.detail.device"), d.name ?? loc("status.detail.none"), stateText(d.state)))
        var format = String(format: loc("status.detail.format"), Int(d.sampleRate), Int(d.channels))
        if let b = d.blockFrames { format += "  " + String(format: loc("status.detail.block"), Int(b)) }
        if let l = d.latencyMs { format += "  " + String(format: loc("status.detail.latency"), l) }
        out.append(format)
        out.append(
            String(
                format: loc("status.detail.disk"), Int(s.disk.streams), Int(s.disk.underrunFrames),
                loc(s.disk.healthy ? "status.disk.healthy" : "status.disk.unhealthy")))
        out.append(
            String(format: loc("status.detail.cpu"), s.cpuPercent.map { String(format: "%.1f%%", $0) } ?? "–"))
        out.append(
            String(format: loc("status.detail.load"), s.engineLoad.map { String(format: "%.0f%%", $0 * 100) } ?? "–"))
        out.append(String(format: loc("status.detail.xruns"), Int(s.xruns)))
        if s.logDropped > 0 { out.append(String(format: loc("status.detail.logDropped"), Int(s.logDropped))) }
        return out
    }

    private static func stateText(_ state: DeviceState) -> String {
        let key = "status.state.\(state.raw)"
        let missing = "\u{1}"
        let value = NSLocalizedString(key, value: missing, comment: "")
        return value == missing ? state.raw : value
    }
}
