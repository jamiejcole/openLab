// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// The Triggers tab's wall-clock trigger (TRG-05; QLab 5 manual p. 129): a box, the time of day, the
/// clock it is shown and typed on (24-hour, AM or PM), and a button naming the days it fires on
/// whose menu ticks and unticks each day. Several cues: each control acts on every cue, from its
/// own trigger; a value that differs shows mixed.
extension InspectorViewController {
    /// The clock pop-up's items, in order.
    static let wallClockFormats = ["24h", "am", "pm"]
    /// The days menu's day items, Monday first (day numbers, 0 Sunday).
    static let wallClockMenuDays = [1, 2, 3, 4, 5, 6, 0]
    /// The days menu's item that ticks every day (a day item's represented object is its number).
    static let everyDayItem = "every"

    func wallClockRow(label: NSView, indent: CGFloat) -> WrappingRow {
        wallClockCheck.setAX(AX.inspectorWallClock, label: loc("inspector.wallClock.enabled"))
        wallClockCheck.toolTip = loc("inspector.wallClock.enabled")
        wallClockCheck.target = self
        wallClockCheck.action = #selector(wallClockCheckChanged)
        wallClockField.setAccessibilityLabel(loc("inspector.wallClock.time"))
        wallClockField.placeholderString = "00:00:00"
        wallClockField.widthAnchor.constraint(equalToConstant: 72).isActive = true
        wallClockFormatPopup.setAX(AX.inspectorWallClockFormat, label: loc("inspector.wallClock.format"))
        for key in Self.wallClockFormats {
            wallClockFormatPopup.menu?.addItem(
                NSMenuItem(title: loc("inspector.wallClock.format.\(key)"), action: nil, keyEquivalent: ""))
        }
        wallClockFormatPopup.target = self
        wallClockFormatPopup.action = #selector(wallClockFormatChanged)
        wallClockDaysPopup.setAX(AX.inspectorWallClockDays, label: loc("inspector.wallClock.days"))
        let menu = NSMenu()
        // A pull-down's first item is its title.
        menu.addItem(NSMenuItem(title: loc("inspector.wallClock.everyDay"), action: nil, keyEquivalent: ""))
        let every = NSMenuItem(title: loc("inspector.wallClock.everyDay"), action: nil, keyEquivalent: "")
        every.representedObject = Self.everyDayItem
        menu.addItem(every)
        menu.addItem(.separator())
        for day in Self.wallClockMenuDays {
            let item = NSMenuItem(
                title: loc("inspector.wallClock.day.\(WallClockTrigger.dayNames[day])"), action: nil, keyEquivalent: "")
            item.representedObject = day
            menu.addItem(item)
        }
        wallClockDaysPopup.menu = menu
        wallClockDaysPopup.target = self
        wallClockDaysPopup.action = #selector(wallClockDaysChosen)
        for p in [wallClockFormatPopup, wallClockDaysPopup] {
            p.controlSize = .small
            p.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        }
        // The days button names the days; a long list is cut short rather than widening the column.
        wallClockDaysPopup.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        (wallClockDaysPopup.cell as? NSPopUpButtonCell)?.lineBreakMode = .byTruncatingTail
        // The label, box, time and clock on one line; the days button follows on that line where
        // the tab is wide, else sits under the time.
        return WrappingRow(
            head: [label, wallClockCheck, wallClockField, wallClockFormatPopup], tail: [wallClockDaysPopup],
            indent: indent)
    }

    /// One cue's trigger: its box, its time (faint while off, as a hotkey switched off), its clock
    /// and its days.
    func renderWallClock(_ cue: Cue) {
        let t = WallClockTrigger(json: cue.raw["triggers"]?["wall_clock"])
        wallClockCheck.state = t.enabled ? .on : .off
        wallClockField.textColor = t.enabled ? .controlTextColor : .disabledControlTextColor
        wallClockFormatPopup.selectItem(at: Self.formatIndex(t))
        showDays([t.days])
        enableWallClock()
    }

    /// Several cues: the box ticked, unticked or mixed; the time, clock and days shown when every
    /// cue has the same (`renderBatch` fills the time field).
    func renderBatchWallClock(_ cues: [Cue]) {
        let triggers = cues.map { WallClockTrigger(json: $0.raw["triggers"]?["wall_clock"]) }
        showTicks(wallClockCheck, triggers.map(\.enabled))
        wallClockField.textColor = .controlTextColor
        showChoice(
            wallClockFormatPopup, Self.wallClockFormats, triggers.map { Self.wallClockFormats[Self.formatIndex($0)] })
        showDays(triggers.map(\.days))
        enableWallClock()
    }

    private func enableWallClock() {
        for c in [wallClockCheck, wallClockField, wallClockFormatPopup, wallClockDaysPopup] as [NSControl] {
            c.isEnabled = !isLocked
        }
    }

    static func formatIndex(_ t: WallClockTrigger) -> Int {
        t.twelveHour ? (t.isPM ? 2 : 1) : 0
    }

    /// The days button's title and each day's tick: on when every cue fires that day, mixed when
    /// some do.
    private func showDays(_ sets: [Set<Int>]) {
        guard let menu = wallClockDaysPopup.menu, let title = menu.items.first else { return }
        let same = Set(sets).count == 1
        var shown = WallClockTrigger()
        shown.days = sets.first ?? WallClockTrigger.everyDay
        title.title = same ? shown.daysSummary : loc("inspector.mixed")
        for item in menu.items.dropFirst() where item.representedObject != nil {
            let on: Int
            if let day = item.representedObject as? Int {
                on = sets.filter { $0.contains(day) }.count
            } else {
                on = sets.filter { $0 == WallClockTrigger.everyDay }.count
            }
            item.state = on == sets.count ? .on : on == 0 ? .off : .mixed
        }
    }

    /// The text the field shows once `command` lands: what was typed, as read (`10:30` is 00:10:30).
    static func wallClockShown(_ command: Command) -> String? {
        command.json["property"]?["value"]?["wall_clock"].map { WallClockTrigger(json: $0).display }
    }

    /// The box: on or off for every cue shown (a mixed box sets it on). Time and days are kept.
    @objc func wallClockCheckChanged() {
        guard !isLocked, !editCues.isEmpty else { return render() }
        let on = wallClockCheck.state != .off
        wallClockCheck.allowsMixedState = false
        wallClockCheck.state = on ? .on : .off
        sendEach(key: "wallClock") { cue in InspectorEdit.setWallClock(cue) { $0.enabled = on } }
        render()
    }

    /// The clock: 24-hour shows the same time from 0 to 23; AM or PM puts the time in that half of
    /// the day (7:25 PM chosen as AM is 7:25 AM), on each cue.
    @objc func wallClockFormatChanged() {
        let index = wallClockFormatPopup.indexOfSelectedItem
        guard !isLocked, !editCues.isEmpty, Self.wallClockFormats.indices.contains(index) else { return render() }
        sendEach(key: "wallClock") { cue in
            InspectorEdit.setWallClock(cue) { t in
                switch index {
                case 0: t.twelveHour = false
                case 1:
                    t.twelveHour = true
                    if t.hours >= 12 { t.hours -= 12 }
                default:
                    t.twelveHour = true
                    if t.hours < 12 { t.hours += 12 }
                }
            }
        }
        render()
    }

    /// A day in the days menu: ticked on every cue when any cue lacks it, else unticked on every cue.
    /// A cue is never left with no day: unticking its last one is refused, with a message. The
    /// every-day item ticks all seven.
    @objc func wallClockDaysChosen() {
        let cues = editCues
        guard !isLocked, !cues.isEmpty, let item = wallClockDaysPopup.selectedItem, item.representedObject != nil
        else { return render() }
        let triggers = cues.map { WallClockTrigger(json: $0.raw["triggers"]?["wall_clock"]) }
        guard let day = item.representedObject as? Int else {
            sendEach(key: "wallClock") { cue in
                InspectorEdit.setWallClock(cue) { $0.days = WallClockTrigger.everyDay }
            }
            return render()
        }
        let on = !triggers.allSatisfy { $0.days.contains(day) }
        if !on, triggers.contains(where: { $0.days == [day] }) {
            showRefusal(loc("inspector.wallClock.lastDay"))
            return render()
        }
        sendEach(key: "wallClock") { cue in
            InspectorEdit.setWallClock(cue) { t in
                if on { t.days.insert(day) } else { t.days.remove(day) }
            }
        }
        render()
    }
}
