// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

// The multi-workspace wire messages (protocol 2, M4.15f; docs/protocol.md section 2): `hello.workspace`,
// the app (unbound) `welcome` and its `workspaces` list, `opened` (result data), `closed`, `close`,
// the app-scope `panic` and `hotkey`, `already_open`. Written before the engine had them (hence
// the file's name); the shapes now match the engine's fixtures. A doc id is a JSON number on the
// wire (the engine's `DocId`, u64); the app keys stores and windows by it as a `ModelID` holding
// its decimal digits (`ModelID(doc:)`, `docNumber`).
//
// The app does not send `hotkey` yet: AppController fans a key press out as per-workspace
// `start` / `trigger_released` (#233). Esc goes out as
// `transport: panic` on the app connection, which the engine treats as the app-scope `panic`.

extension ModelID {
    /// A doc id as the engine sends it.
    init(doc: UInt64) { self.init(String(doc)) }

    /// The wire form of a doc id; nil when this is not one.
    var docNumber: UInt64? { UInt64(raw) }

    /// Encodes this doc id as the engine's number, or fails (a doc id is never anything else).
    func encodeDoc<K: CodingKey>(in c: inout KeyedEncodingContainer<K>, forKey key: K) throws {
        guard let n = docNumber else {
            throw EncodingError.invalidValue(
                raw, .init(codingPath: c.codingPath + [key], debugDescription: "not a doc id"))
        }
        try c.encode(n, forKey: key)
    }
}

/// `hello`. `workspace` binds the connection to one open workspace; absent (and omitted from the
/// JSON) it is the unbound app connection.
struct Hello: Hashable, Sendable, Codable {
    var protocolVersion: UInt32
    var token: String
    var clientName: String
    var workspace: ModelID?

    enum CodingKeys: String, CodingKey {
        case type, token, workspace
        case protocolVersion = "protocol_version"
        case clientName = "client_name"
    }

    init(protocolVersion: UInt32, token: String, clientName: String, workspace: ModelID? = nil) {
        self.protocolVersion = protocolVersion
        self.token = token
        self.clientName = clientName
        self.workspace = workspace
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        protocolVersion = try c.decode(UInt32.self, forKey: .protocolVersion)
        token = try c.decode(String.self, forKey: .token)
        clientName = try c.decode(String.self, forKey: .clientName)
        workspace = try c.decodeIfPresent(UInt64.self, forKey: .workspace).map(ModelID.init(doc:))
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode("hello", forKey: .type)
        try c.encode(protocolVersion, forKey: .protocolVersion)
        try c.encode(token, forKey: .token)
        try c.encode(clientName, forKey: .clientName)
        try workspace?.encodeDoc(in: &c, forKey: .workspace)
    }
}

/// One open workspace as the app connection lists it.
struct WorkspaceInfo: Hashable, Sendable, Decodable {
    let doc: ModelID
    let workspaceId: ModelID
    let name: String
    let path: String?
    let dirty: Bool
    enum CodingKeys: String, CodingKey {
        case doc, name, path, dirty
        case workspaceId = "workspace_id"
    }

    init(doc: ModelID, workspaceId: ModelID, name: String, path: String?, dirty: Bool) {
        self.doc = doc
        self.workspaceId = workspaceId
        self.name = name
        self.path = path
        self.dirty = dirty
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        doc = ModelID(doc: try c.decode(UInt64.self, forKey: .doc))
        workspaceId = try c.decode(ModelID.self, forKey: .workspaceId)
        name = try c.decode(String.self, forKey: .name)
        path = try c.decodeIfPresent(String.self, forKey: .path)
        dirty = try c.decode(Bool.self, forKey: .dirty)
    }
}

enum ProvisionalServerMessage: Sendable {
    /// `welcome` on an unbound connection: the open workspaces instead of a snapshot.
    case appWelcome(protocolVersion: UInt32, workspaces: [WorkspaceInfo])
    /// The list changed (a workspace opened, closed, renamed, saved or became dirty).
    case workspaces([WorkspaceInfo])
    /// The engine closed this workspace; its connections close next.
    case closed(doc: ModelID)

    /// nil for a type that is not one of these.
    init?(type: String, from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        switch type {
        case "welcome":
            self = .appWelcome(
                protocolVersion: try c.decode(UInt32.self, forKey: "protocol_version"),
                workspaces: try c.decode([WorkspaceInfo].self, forKey: "workspaces"))
        case "workspaces": self = .workspaces(try c.decode([WorkspaceInfo].self, forKey: "workspaces"))
        case "closed": self = .closed(doc: ModelID(doc: try c.decode(UInt64.self, forKey: "doc")))
        default: return nil
        }
    }
}

/// The modifier keys of a `hotkey` message (all four always on the wire).
struct HotkeyModifiers: Hashable, Sendable, Codable {
    var shift = false
    var control = false
    /// Option.
    var alt = false
    var command = false
}

enum ProvisionalClientMessage: Hashable, Sendable {
    /// Close a workspace: the bound one, or `doc` from the app connection. Refused with
    /// `unsaved_changes` unless `discardChanges`.
    case close(id: UInt64, doc: ModelID?, discardChanges: Bool)
    /// The app-scope Panic (Esc): every open workspace, on any connection.
    case panic(id: UInt64)
    /// A hotkey went down (`up` false) or up, app-scope: the cues with it start (or get their
    /// release) in every open workspace.
    case hotkey(id: UInt64, key: String, modifiers: HotkeyModifiers, up: Bool)

    init?(type: String, from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        switch type {
        case "close":
            self = .close(
                id: try c.decode(UInt64.self, forKey: "id"),
                doc: try c.decodeIfPresent(UInt64.self, forKey: "doc").map(ModelID.init(doc:)),
                discardChanges: try c.decodeIfPresent(Bool.self, forKey: "discard_changes") ?? false)
        case "panic": self = .panic(id: try c.decode(UInt64.self, forKey: "id"))
        case "hotkey":
            self = .hotkey(
                id: try c.decode(UInt64.self, forKey: "id"), key: try c.decode(String.self, forKey: "key"),
                modifiers: try c.decodeIfPresent(HotkeyModifiers.self, forKey: "modifiers") ?? HotkeyModifiers(),
                up: try c.decodeIfPresent(String.self, forKey: "phase") == "up")
        default: return nil
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        switch self {
        case .close(let id, let doc, let discard):
            try c.encode("close", forKey: "type")
            try c.encode(id, forKey: "id")
            try doc?.encodeDoc(in: &c, forKey: "doc")
            if discard { try c.encode(true, forKey: "discard_changes") }
        case .panic(let id):
            try c.encode("panic", forKey: "type")
            try c.encode(id, forKey: "id")
        case .hotkey(let id, let key, let modifiers, let up):
            try c.encode("hotkey", forKey: "type")
            try c.encode(id, forKey: "id")
            try c.encode(key, forKey: "key")
            try c.encode(modifiers, forKey: "modifiers")
            try c.encode(up ? "up" : "down", forKey: "phase")
        }
    }
}

extension ResultData {
    /// The doc a `new` / `open` / `open_recovery` opened (`result.ok.data { type: "opened", doc }`).
    var openedDoc: ModelID? {
        guard case .other("opened", let json) = self else { return nil }
        switch json["doc"] {
        case .int(let n)? where n >= 0: return ModelID(doc: UInt64(n))
        case .uint(let n)?: return ModelID(doc: n)
        default: return nil
        }
    }
}

extension ErrorCode {
    /// `open` of a path that is already open.
    static let alreadyOpen: ErrorCode = "already_open"
}
