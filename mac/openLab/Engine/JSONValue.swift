// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// A JSON document held as data. The model objects the UI does not interpret yet (cues, patches,
/// settings, `Command`) travel as this, so the codec round-trips them exactly while the typed
/// model grows with the editing tasks.
enum JSONValue: Hashable, Sendable, Codable {
    case null
    case bool(Bool)
    case int(Int64)
    /// Only for integers above Int64.max (request ids are u64).
    case uint(UInt64)
    case double(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() {
            self = .null
        } else if let v = try? c.decode(Bool.self) {
            self = .bool(v)
        } else if let v = try? c.decode(Int64.self) {
            self = .int(v)
        } else if let v = try? c.decode(UInt64.self) {
            self = .uint(v)
        } else if let v = try? c.decode(Double.self) {
            self = .double(v)
        } else if let v = try? c.decode(String.self) {
            self = .string(v)
        } else if let v = try? c.decode([JSONValue].self) {
            self = .array(v)
        } else {
            self = .object(try c.decode([String: JSONValue].self))
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case .bool(let v): try c.encode(v)
        case .int(let v): try c.encode(v)
        case .uint(let v): try c.encode(v)
        case .double(let v): try c.encode(v)
        case .string(let v): try c.encode(v)
        case .array(let v): try c.encode(v)
        case .object(let v): try c.encode(v)
        }
    }

    subscript(key: String) -> JSONValue? {
        get {
            if case .object(let o) = self { return o[key] }
            return nil
        }
        set {
            if case .object(var o) = self {
                o[key] = newValue
                self = .object(o)
            }
        }
    }

    var stringValue: String? {
        if case .string(let s) = self { return s }
        return nil
    }

    /// Any JSON number as a Double (a whole-number time arrives as an integer).
    var doubleValue: Double? {
        switch self {
        case .double(let v): return v
        case .int(let v): return Double(v)
        case .uint(let v): return Double(v)
        default: return nil
        }
    }

    var arrayValue: [JSONValue]? {
        if case .array(let a) = self { return a }
        return nil
    }

    /// Parses JSON text; nil if it is not valid JSON.
    static func parse(_ text: String) -> JSONValue? {
        try? JSONDecoder().decode(JSONValue.self, from: Data(text.utf8))
    }

    func text() throws -> String {
        let enc = JSONEncoder()
        enc.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return String(decoding: try enc.encode(self), as: UTF8.self)
    }
}

extension JSONValue {
    /// Sets `key` on an object value (no-op on anything else).
    mutating func set(_ key: String, _ value: JSONValue) {
        if case .object(var o) = self {
            o[key] = value
            self = .object(o)
        }
    }
}
