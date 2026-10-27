// SPDX-License-Identifier: GPL-3.0-or-later

import AppKit

/// Finder drops, copy / cut / paste and Delete Numbers (M5.10b). Same rules as `CueEditor`: refuse in
/// Show Mode, commit the inspector first, one transaction per gesture.
extension CueEditor {
    static let pasteType = NSPasteboard.PasteboardType("dev.openlab.cue-data")
    /// What the engine's decoder reads (docs/ARCHITECTURE.md §12: WAV, AIFF, FLAC, MP3, AAC, OGG).
    static let audioExtensions: Set<String> = ["wav", "aif", "aiff", "flac", "mp3", "aac", "m4a", "ogg"]

    // MARK: Pure rules

    /// The dropped files when every one is a plain audio file; nil when any is a folder or other kind.
    static func audioFiles(_ urls: [URL]) -> [URL]? {
        guard !urls.isEmpty else { return nil }
        for url in urls {
            var isDir: ObjCBool = false
            guard url.isFileURL, audioExtensions.contains(url.pathExtension.lowercased()),
                !(FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir) && isDir.boolValue)
            else { return nil }
        }
        return urls
    }

    /// A media path as core stores it: absolute, forward slashes, NFC (docs/file-format.md §4.3).
    /// The engine owns turning it into a show-relative path.
    static func mediaPath(_ url: URL) -> String { url.standardizedFileURL.path.precomposedStringWithCanonicalMapping }

    /// A cue and its descendants as one JSON cue (the model keeps children apart from the kind).
    static func fullJSON(_ node: CueNode) -> JSONValue {
        var cue = node.cue.raw
        if node.isGroup, var kind = cue["kind"] {
            kind.set("children", .array(node.children.map(fullJSON)))
            cue.set("kind", kind)
        }
        return cue
    }

    /// Gives every cue in the tree a new id; a target that pointed into the copy follows it.
    static func withFreshIDs(_ cue: JSONValue, newID: () -> ModelID = { ModelID(UUID().uuidString.lowercased()) })
        -> JSONValue
    {
        var map: [String: String] = [:]
        func rename(_ v: JSONValue) -> JSONValue {
            var v = v
            if let old = v["id"]?.stringValue {
                let new = newID().raw
                map[old] = new
                v.set("id", .string(new))
            }
            if var kind = v["kind"], let kids = kind["children"]?.arrayValue {
                kind.set("children", .array(kids.map(rename)))
                v.set("kind", kind)
            }
            return v
        }
        func retarget(_ v: JSONValue) -> JSONValue {
            var v = v
            guard var kind = v["kind"] else { return v }
            if let t = kind["target"]?.stringValue, let new = map[t] { kind.set("target", .string(new)) }
            if let kids = kind["children"]?.arrayValue { kind.set("children", .array(kids.map(retarget))) }
            v.set("kind", kind)
            return v
        }
        return retarget(rename(cue))
    }

    // MARK: Finder drops

    /// One audio cue per file, in drop order, at child `index` of `parent`; one undo step.
    @discardableResult
    func addFiles(_ urls: [URL], parent: ModelID?, index: Int) async -> [ModelID] {
        guard await begin(), let list = cueList.listID else { return [] }
        guard let files = Self.audioFiles(urls) else {
            notify(loc("edit.error.dropNotAudio"))
            return []
        }
        var ids: [ModelID] = []
        var commands: [Command] = []
        for (n, url) in files.enumerated() {
            let id = ModelID(UUID().uuidString.lowercased())
            guard var cue = NewCueKind.audio.cueJSON(id: id, patch: store.model?.patches.first),
                var kind = cue["kind"]
            else {
                notify(loc("edit.error.noPatch"))
                return []
            }
            kind.set("media", .string(Self.mediaPath(url)))
            cue.set("kind", kind)
            cue.set("name", .string(url.deletingPathExtension().lastPathComponent))
            commands.append(.addCue(cue, list: list, parent: parent, index: index + n))
            ids.append(id)
        }
        guard let command = Command.transaction(commands), report(await store.send(command: command)) else {
            return []
        }
        cueList.selectAsUser(ids)
        return ids
    }

    /// A file dropped on an audio cue replaces its file; a running cue is left alone.
    func replaceFile(of id: ModelID, with url: URL) async {
        guard await begin() else { return }
        guard let files = Self.audioFiles([url]) else { return notify(loc("edit.error.dropNotAudio")) }
        guard let node = cueList.node(id), node.cue.kindType == "audio", var kind = node.cue.raw["kind"] else {
            return
        }
        if isBusy(node) { return notify(loc("edit.error.replaceBusy")) }
        kind.set("media", .string(Self.mediaPath(files[0])))
        _ = report(await store.send(command: .setProperty(id, .kind(kind))))
    }

    // MARK: Copy, cut, paste

    /// Puts the selected cues (with their children) on the pasteboard.
    @discardableResult
    func copySelection() async -> Bool {
        guard await begin() else { return false }
        return writePasteboard(Self.topLevel(selectedNodes()))
    }

    private func writePasteboard(_ nodes: [CueNode]) -> Bool {
        guard !nodes.isEmpty,
            let text = try? JSONValue.object(["cues": .array(nodes.map(Self.fullJSON))]).text()
        else { return false }
        pasteboard.clearContents()
        return pasteboard.setString(text, forType: Self.pasteType)
    }

    /// Copy, then delete; nothing happens (pasteboard included) if a selected cue is busy.
    func cutSelection() async {
        guard await begin() else { return }
        let nodes = Self.topLevel(selectedNodes())
        if nodes.contains(where: isBusy) { return notify(loc("edit.error.deleteBusy")) }
        guard writePasteboard(nodes) else { return }
        await deleteSelection()
    }

    var canPaste: Bool { !isLocked() && pasteboard.string(forType: Self.pasteType) != nil }

    /// Pastes after the selection (see `insertion`) as one undo step, with fresh ids and automatic numbers.
    func paste() async {
        guard await begin(), let list = cueList.listID else { return }
        guard let text = pasteboard.string(forType: Self.pasteType),
            let cues = JSONValue.parse(text)?["cues"]?.arrayValue, !cues.isEmpty
        else { return NSSound.beep() }
        let place = Self.insertion(selected: selectedNodes(), roots: cueList.roots)
        var ids: [ModelID] = []
        var commands: [Command] = []
        for (n, cue) in cues.enumerated() {
            let fresh = Self.withFreshIDs(cue)
            guard let id = fresh["id"]?.stringValue else { return NSSound.beep() }
            ids.append(ModelID(id))
            commands.append(.addCue(fresh, list: list, parent: place.parent, index: place.index + n))
        }
        guard let command = Command.transaction(commands), report(await store.send(command: command)) else { return }
        cueList.selectAsUser(ids)
    }

    // MARK: Delete numbers

    /// Clears the number of every selected cue (QLab: Tools > Delete Numbers of Selected Cues, Cmd-D).
    func deleteNumbers() async {
        guard await begin() else { return }
        let ids = cueList.selectedIDs().filter { cueList.node($0)?.cue.number != nil }
        guard !ids.isEmpty else { return }
        _ = report(await store.send(command: .clearNumbers(ids)))
    }
}
