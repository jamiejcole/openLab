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

    /// The extension test alone, cheap enough for every drag update.
    static func hasAudioExtensions(_ urls: [URL]) -> Bool {
        !urls.isEmpty && urls.allSatisfy { $0.isFileURL && audioExtensions.contains($0.pathExtension.lowercased()) }
    }

    /// The private pasteboard payload; bump `version` when its shape changes.
    static let clipboardVersion = 1

    /// The cues of a pasteboard payload; nil for anything that is not exactly what `copySelection` writes.
    static func decodeClipboard(_ text: String) -> [JSONValue]? {
        guard let root = JSONValue.parse(text), case .int(let v)? = root["version"], v == Int64(clipboardVersion),
            let cues = root["cues"]?.arrayValue, !cues.isEmpty
        else { return nil }
        func valid(_ cue: JSONValue) -> Bool {
            guard case .object = cue, cue["id"]?.stringValue != nil, case .object? = cue["kind"] else { return false }
            return (cue["kind"]?["children"]?.arrayValue ?? []).allSatisfy(valid)
        }
        return cues.allSatisfy(valid) ? cues : nil
    }

    /// Core's `prepare_paste` (docs/commands.md; the wire has only `paste`, which wants cues already
    /// prepared): fresh ids across the whole clipboard, a target kept if it is inside the clipboard
    /// (follows the copy) or exists in the workspace, else cleared; numbers already used (or empty)
    /// cleared; a missing audio patch or network destination becomes the workspace's first.
    static func preparePaste(
        _ clipboard: [JSONValue], in model: WorkspaceModel,
        newID: () -> ModelID = { ModelID(UUID().uuidString.lowercased()) }
    ) -> [JSONValue] {
        var renamed: [String: String] = [:]
        func rename(_ v: JSONValue) -> JSONValue {
            var v = v
            if let old = v["id"]?.stringValue {
                let new = newID().raw
                renamed[old] = new
                v.set("id", .string(new))
            }
            if var kind = v["kind"], let kids = kind["children"]?.arrayValue {
                kind.set("children", .array(kids.map(rename)))
                v.set("kind", kind)
            }
            return v
        }
        var existing = Set<String>()
        var numbers = Set<String>()
        func scan(_ cues: [Cue]) {
            for c in cues {
                existing.insert(c.id.raw)
                if let n = c.number { numbers.insert(n) }
                scan(c.children)
            }
        }
        for list in model.cueLists { scan(list.cues) }
        let patches = Set(model.patches.map(\.id.raw))
        let networks = Set(model.networkPatches.map(\.id.raw))
        func fix(_ v: JSONValue) -> JSONValue {
            var v = v
            if let n = v["number"]?.stringValue, n.isEmpty || !numbers.insert(n).inserted {
                v.set("number", .null)
            }
            guard var kind = v["kind"] else { return v }
            if let t = kind["target"]?.stringValue {
                if let new = renamed[t] {
                    kind.set("target", .string(new))
                } else if !existing.contains(t) {
                    kind.set("target", .null)
                }
            }
            if let p = kind["patch"]?.stringValue, !patches.contains(p), let first = model.patches.first {
                kind.set("patch", .string(first.id.raw))
            }
            if let d = kind["destination"]?.stringValue, !networks.contains(d), let first = model.networkPatches.first {
                kind.set("destination", .string(first.id.raw))
            }
            if let kids = kind["children"]?.arrayValue { kind.set("children", .array(kids.map(fix))) }
            v.set("kind", kind)
            return v
        }
        return clipboard.map(rename).map(fix)
    }

    // MARK: Finder drops

    /// One audio cue per file, in drop order, at child `index` of `parent`; one undo step.
    @discardableResult
    func addFiles(_ urls: [URL], parent: ModelID?, index: Int) async -> [ModelID] {
        // The inspector commit below can take a while and the list can change meanwhile: remember the cue
        // the drop landed before and find its place again afterwards.
        let anchor = children(of: parent).dropFirst(max(0, index)).first?.id
        guard await begin(), let list = cueList?.listID else { return [] }
        let siblings = children(of: parent)
        if let parent, cueList?.node(parent) == nil { return [] }
        let index = anchor.flatMap { a in siblings.firstIndex { $0.id == a } } ?? siblings.count
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
            // No name: the cue follows its file (M2.29c), so a renamed file shows at once.
            commands.append(.addCue(cue, list: list, parent: parent, index: index + n))
            ids.append(id)
        }
        guard let command = Command.transaction(commands), report(await store.send(command: command)) else {
            return []
        }
        cueList?.selectAsUser(ids)
        return ids
    }

    private func children(of parent: ModelID?) -> [CueNode] {
        parent.flatMap { cueList?.node($0) }?.children ?? cueList?.roots ?? []
    }

    /// A file dropped on an audio cue replaces its file; a running cue is left alone.
    func replaceFile(of id: ModelID, with url: URL) async {
        guard await begin() else { return }
        guard let files = Self.audioFiles([url]) else { return notify(loc("edit.error.dropNotAudio")) }
        guard let node = cueList?.node(id), node.cue.kindType == "audio", var kind = node.cue.raw["kind"] else {
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
            let text = try? JSONValue.object([
                "version": .int(Int64(Self.clipboardVersion)), "cues": .array(nodes.map(Self.fullJSON)),
            ]).text()
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
        guard await begin(), let list = cueList?.listID else { return }
        guard let text = pasteboard.string(forType: Self.pasteType), let clipboard = Self.decodeClipboard(text),
            let model = store.model
        else { return NSSound.beep() }
        let place = Self.insertion(selected: selectedNodes(), roots: cueList?.roots ?? [])
        let cues = Self.preparePaste(clipboard, in: model)
        let ids = cues.compactMap { $0["id"]?.stringValue.map { ModelID($0) } }
        guard ids.count == cues.count else { return NSSound.beep() }
        let at = Command.position(list: list, parent: place.parent, index: place.index)
        guard report(await store.send(command: .paste(at: at, cues: cues))) else { return }
        cueList?.selectAsUser(ids)
    }

    // MARK: Delete numbers

    /// Clears the number of every selected cue (QLab: Tools > Delete Numbers of Selected Cues, Cmd-D).
    func deleteNumbers() async {
        guard await begin() else { return }
        let ids = (cueList?.selectedIDs() ?? []).filter { cueList?.node($0)?.cue.number != nil }
        guard !ids.isEmpty else { return }
        _ = report(await store.send(command: .clearNumbers(ids)))
    }
}
