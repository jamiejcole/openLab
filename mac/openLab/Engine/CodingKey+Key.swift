// SPDX-License-Identifier: GPL-3.0-or-later

/// A string coding key, for the protocol's `{"type": ..., ...fields}` envelopes.
struct Key: CodingKey, ExpressibleByStringLiteral {
    let stringValue: String
    var intValue: Int? { nil }
    init(_ s: String) { stringValue = s }
    init(stringLiteral s: String) { stringValue = s }
    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { nil }
}
