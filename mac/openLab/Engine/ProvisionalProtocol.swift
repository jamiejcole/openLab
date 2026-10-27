// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

// PROVISIONAL. Every multi-workspace wire name lives in this file: `hello.workspace`, the app
// (unbound) `welcome` and its `workspaces` list, `opened` (result data), `closed`, `close`,
// `already_open`. The names follow ARCHITECTURE section 6 on PR #18 (m4-15a-multi-workspace-docs)
// and are NOT final until M4.15f lands the real types and fixtures. Fixing a name is a change
// here only. Today's engine has one workspace and never sends these; the session layer then
// degrades to single-workspace behaviour (EngineSession).
//
// App-scope `panic` needs no name of its own: it is the ordinary `transport: panic`, which the
// multi-workspace engine applies to every open workspace.

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
        workspace = try c.decodeIfPresent(ModelID.self, forKey: .workspace)
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode("hello", forKey: .type)
        try c.encode(protocolVersion, forKey: .protocolVersion)
        try c.encode(token, forKey: .token)
        try c.encode(clientName, forKey: .clientName)
        try c.encodeIfPresent(workspace, forKey: .workspace)
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
        case "closed": self = .closed(doc: try c.decode(ModelID.self, forKey: "doc"))
        default: return nil
        }
    }
}

enum ProvisionalClientMessage: Hashable, Sendable {
    /// Close a workspace: the bound one, or `doc` from the app connection. Refused with
    /// `unsaved_changes` unless `discardChanges`.
    case close(id: UInt64, doc: ModelID?, discardChanges: Bool)

    init?(type: String, from decoder: Decoder) throws {
        guard type == "close" else { return nil }
        let c = try decoder.container(keyedBy: Key.self)
        self = .close(
            id: try c.decode(UInt64.self, forKey: "id"),
            doc: try c.decodeIfPresent(ModelID.self, forKey: "doc"),
            discardChanges: try c.decodeIfPresent(Bool.self, forKey: "discard_changes") ?? false)
    }

    func encode(to encoder: Encoder) throws {
        switch self {
        case .close(let id, let doc, let discard):
            var c = encoder.container(keyedBy: Key.self)
            try c.encode("close", forKey: "type")
            try c.encode(id, forKey: "id")
            try c.encodeIfPresent(doc, forKey: "doc")
            if discard { try c.encode(true, forKey: "discard_changes") }
        }
    }
}

extension ResultData {
    /// The doc a `new` / `open` / `open_recovery` opened (`result.ok.data { type: "opened", doc }`).
    /// nil on today's engine, where those requests replace the one workspace in place.
    var openedDoc: ModelID? {
        if case .other("opened", let json) = self, let doc = json["doc"]?.stringValue { return ModelID(doc) }
        return nil
    }
}

extension ErrorCode {
    /// `open` of a path that is already open.
    static let alreadyOpen: ErrorCode = "already_open"
}
