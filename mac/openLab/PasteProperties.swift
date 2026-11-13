// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// A property Paste Cue Properties can copy from the copied cue onto the selected cues (ED-06; QLab 5,
/// manual p. 154), and the category the sheet lists it under. A property is offered when the copied
/// cue has it and at least one selected cue can take it; it is set on the cues that can and left
/// alone on the others. Pure: the editor supplies the cues.
enum PasteProperty: String, CaseIterable {
    case name, notes, preWait, postWait, duration, continueMode, colour, flagged, armed
    case secondTrigger, secondTriggerOnRelease
    case outputPatch
    case levels
    case fadeCurve, fadeMode, fadeStopTarget
    case groupMode
    case loadTime
    case devampMode, devampStartNext, devampStopTarget
    case networkDestination, networkMessage

    enum Category: String, CaseIterable {
        case basics, triggers, io, levels, fade, mode, loadTime, devamp, network

        var titleKey: String { "paste.category.\(rawValue)" }
        var properties: [PasteProperty] { PasteProperty.allCases.filter { $0.category == self } }
    }

    var category: Category {
        switch self {
        case .name, .notes, .preWait, .postWait, .duration, .continueMode, .colour, .flagged, .armed: return .basics
        case .secondTrigger, .secondTriggerOnRelease: return .triggers
        case .outputPatch: return .io
        case .levels: return .levels
        case .fadeCurve, .fadeMode, .fadeStopTarget: return .fade
        case .groupMode: return .mode
        case .loadTime: return .loadTime
        case .devampMode, .devampStartNext, .devampStopTarget: return .devamp
        case .networkDestination, .networkMessage: return .network
        }
    }

    var titleKey: String { "paste.property.\(rawValue)" }

    /// Whether a cue of kind `to` can take this property from a cue of kind `from`.
    func applies(from: String, to: String) -> Bool {
        switch category {
        case .basics:
            guard self == .duration else { return true }
            return InspectorEdit.durationKinds.contains(from) && InspectorEdit.durationKinds.contains(to)
        case .triggers: return true
        case .io: return from == "audio" && to == "audio"
        case .levels: return from == to && (from == "audio" || from == "fade")
        case .fade: return from == "fade" && to == "fade"
        case .mode: return from == "group" && to == "group"
        case .loadTime: return from == "load" && to == "load"
        case .devamp: return from == "devamp" && to == "devamp"
        case .network: return from == "network" && to == "network"
        }
    }

    /// The categories the sheet lists for `source` pasted onto `cues`, each with the properties at
    /// least one of the cues can take, in the sheet's order. Categories with none are left out.
    static func offered(from source: Cue, to cues: [Cue]) -> [(category: Category, properties: [PasteProperty])] {
        Category.allCases.compactMap { category in
            let properties = category.properties.filter { p in
                cues.contains { p.applies(from: source.kindType, to: $0.kindType) }
            }
            return properties.isEmpty ? nil : (category, properties)
        }
    }

    /// Every command that sets the `ticked` properties of `source` on `cues`, in list order and then
    /// the sheet's order: the caller sends them as one transaction (one undo step). A cue that cannot
    /// take a property is left out of it; one that has it already is sent it anyway (the engine
    /// counts that as no change).
    static func commands(_ ticked: Set<PasteProperty>, from source: Cue, to cues: [Cue]) -> [Command] {
        cues.flatMap { cue in allCases.filter(ticked.contains).flatMap { $0.commands(from: source, to: cue) } }
    }

    /// The commands that set this property of `source` on `cue`; none when `cue` cannot take it or
    /// `source` has nothing to give.
    func commands(from source: Cue, to cue: Cue) -> [Command] {
        guard applies(from: source.kindType, to: cue.kindType) else { return [] }
        let raw = source.raw
        let kind = raw["kind"] ?? .null
        let id = cue.id
        func set(_ p: CueProperty) -> [Command] { [.setProperty(id, p)] }
        func field(_ name: String) -> [Command] {
            guard let value = kind[name], value != .null else { return [] }
            return set(.kindFields([name: value]))
        }
        switch self {
        case .name: return set(.name(source.name))
        case .notes: return set(.notes(raw["notes"]?.stringValue ?? ""))
        case .preWait: return set(.preWait(raw["pre_wait"]?.doubleValue ?? 0))
        case .postWait: return set(.postWait(raw["post_wait"]?.doubleValue ?? 0))
        case .duration: return set(.duration(kind["duration"]?.doubleValue ?? 0))
        case .continueMode: return set(.continueMode(raw["continue_mode"]?.stringValue ?? "do_not_continue"))
        case .colour: return set(.color(raw["color"]?.stringValue ?? "none"))
        case .flagged: return set(.flagged(raw["flagged"] == .bool(true)))
        case .armed: return set(.armed(raw["armed"] != .bool(false)))
        case .secondTrigger: return set(.secondTrigger(raw["second_trigger"]?.stringValue ?? "nothing"))
        case .secondTriggerOnRelease:
            return set(.secondTriggerOnRelease(raw["second_trigger_on_release"] == .bool(true)))
        case .outputPatch: return field("patch")
        case .levels:
            if cue.kindType == "fade" { return field("levels") }
            // An Audio cue keeps its own outputs: the levels are fitted to them.
            guard let from = LevelSet(kind: kind), let to = LevelSet(kind: cue.raw["kind"] ?? .null) else { return [] }
            guard let levels = from.pasted(onto: to).json(into: .object([:]))["levels"] else { return [] }
            return set(.kindFields(["levels": levels]))
        case .fadeCurve: return field("curve")
        case .fadeMode: return field("mode")
        case .fadeStopTarget: return field("stop_target_when_done")
        case .groupMode:
            guard let mode = kind["mode"], let type = mode["type"]?.stringValue else { return [] }
            return set(
                .groupMode(type: type, loop: mode["loop"] == .bool(true), shuffle: mode["shuffle"] == .bool(true)))
        case .loadTime: return set(.loadToTime(kind["load_to_time"]?.doubleValue ?? 0))
        case .devampMode: return field("devamp_mode")
        case .devampStartNext: return field("start_next_cue")
        case .devampStopTarget: return field("stop_target_when_slice_ends")
        case .networkDestination:
            guard let destination = kind["destination"]?.stringValue else { return [] }
            return set(.destination(ModelID(destination)))
        case .networkMessage:
            return set(.address(kind["address"]?.stringValue ?? "")) + set(.args(kind["args"]?.arrayValue ?? []))
        }
    }
}

extension LevelSet {
    /// These levels as an Audio cue with `dest`'s outputs takes them: the main level, each output
    /// both have, the routing and the gangs to its outputs. An output the cue lacks is left out (it
    /// has no fader to set); one these levels lack keeps the cue's own.
    func pasted(onto dest: LevelSet) -> LevelSet {
        var d = dest
        d.master = master
        for o in d.outputs.indices where outputs.indices.contains(o) { d.outputs[o] = outputs[o] }
        let count = d.outputs.count
        d.crosspoints = crosspoints.filter { $0.key.output < count }
        d.gangs = gangs.map { $0.filter { $0 < count } }.filter { !$0.isEmpty }
        return d
    }

    /// What one action in the Levels tab did, so each of several cues can do the same to its own levels.
    enum Edit: Equatable {
        /// A fader, a drag or a typed value moved this level.
        case level(Slot)
        case mute(Slot)
        case gang(output: Int, letter: String)
        case silenceAll
        case defaults
    }

    /// These levels after `edit`, which left the shown levels as `shown`: the one level moved (an
    /// output with this cue's own gang, as a drag on it would), its mute, its gang, or the whole-set
    /// action. Nothing else is written, and a level this cue does not have is left alone.
    func taking(_ edit: Edit, from shown: LevelSet, limits: VolumeLimits, drag: Bool = false) -> LevelSet {
        var s = self
        func has(_ slot: Slot) -> Bool {
            switch slot {
            case .master: return true
            case .output(let o): return s.outputs.indices.contains(o)
            case .cross(_, let o): return s.outputs.indices.contains(o)
            }
        }
        switch edit {
        case .level(let slot):
            guard has(slot), let db = shown.db(slot) else { return s }
            s.set(slot, db: db, drag: drag, limits: limits)
        case .mute(let slot):
            guard has(slot) else { return s }
            s.setMute(slot, shown.isMuted(slot))
        case .gang(let o, let letter):
            s.setGang(of: o, letter: letter)
        case .silenceAll: s.silenceAll()
        case .defaults: s.setDefaults()
        }
        return s
    }

    /// The levels of a grid of `inputs` x `outputs` whose value or mute is not the same in all of `sets`.
    static func mixedSlots(_ sets: [LevelSet], inputs: Int, outputs: Int) -> Set<Slot> {
        guard let first = sets.first else { return [] }
        var slots: [Slot] = [.master] + (0..<max(0, outputs)).map { .output($0) }
        for i in 0..<max(0, inputs) { for o in 0..<max(0, outputs) { slots.append(.cross(input: i, output: o)) } }
        return Set(
            slots.filter { slot in
                sets.dropFirst().contains { $0.db(slot) != first.db(slot) || $0.isMuted(slot) != first.isMuted(slot) }
            })
    }
}
