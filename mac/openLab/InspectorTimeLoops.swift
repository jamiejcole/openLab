// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The timing and loops tab of an Audio cue (AUD-11, AUD-12, AUD-14), after Basics and Triggers as
/// in QLab. Trims and the play count are fields of the cue's `kind`, so an edit sends the whole kind
/// (core's `CueProperty::Kind`, as the Levels tab does) with only that field changed.
extension InspectorViewController {
    func buildTimeLoopsTab() {
        timeLoops.onChange = { [weak self] change in self?.sendTimeLoops(change) }
        timeLoops.onInvalid = { [weak self] key in self?.messageLabel.stringValue = loc(key) }
        timeLoops.loadPeaks = { [weak self] cue, start, end, buckets in
            guard let self else { return .failure(.notConnected) }
            return await self.store.waveform(cue: cue, start: start, end: end, buckets: buckets)
        }
        let item = NSTabViewItem(identifier: "timeLoops")
        item.label = loc("inspector.tab.timeLoops")
        let box = NSView()
        let content = timeLoops.view
        content.translatesAutoresizingMaskIntoConstraints = false
        box.addSubview(content)
        NSLayoutConstraint.activate([
            content.topAnchor.constraint(equalTo: box.topAnchor, constant: 8),
            content.leadingAnchor.constraint(equalTo: box.leadingAnchor, constant: 12),
            content.trailingAnchor.constraint(equalTo: box.trailingAnchor, constant: -12),
            content.bottomAnchor.constraint(equalTo: box.bottomAnchor, constant: -8),
        ])
        item.view = box
        timeLoopsItem = item
    }

    /// Shows the tab for an Audio cue, third after Basics and Triggers; hides it otherwise.
    func renderTimeLoops(_ cue: Cue) {
        guard let item = timeLoopsItem else { return }
        let audio = cue.kindType == "audio"
        let present = tabView.tabViewItems.contains { $0 === item }
        if audio, !present {
            tabView.insertTabViewItem(item, at: min(2, tabView.numberOfTabViewItems))
        } else if !audio, present {
            tabView.removeTabViewItem(item)
        }
        guard audio else { return }
        timeLoops.show(cue: cue.id, kind: cue.raw["kind"] ?? .null, duration: store.mediaDurations[cue.id]?.file)
        updateTimeLoopsProgress()
    }

    /// The playhead line follows the selected cue's progress.
    func updateTimeLoopsProgress() {
        guard let cue, cue.kindType == "audio" else { return }
        timeLoops.showProgress(elapsed: store.progress[cue.id]?.elapsed)
    }

    func sendTimeLoops(_ change: TimeLoopsPanel.Change) {
        sendKind(
            { kind in
                var kind = kind
                switch change {
                case .start(let s): kind.set("start_time", .double(s))
                case .end(let e): kind.set("end_time", e.map { .double($0) } ?? .null)
                case .playCount(let n): kind.set("last_slice_play_count", .int(Int64(n)))
                }
                return kind
            }, key: "timeLoops")
    }
}
