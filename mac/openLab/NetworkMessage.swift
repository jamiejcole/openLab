// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// A Network cue's message as one line of text: the OSC address, then its arguments separated by
/// spaces. Digits are an integer, digits with a point a float, text in double quotes a string (quotes
/// keep spaces), `\T` and `\F` true and false (QLab 5, manual pp. 366-367). Pure: the inspector
/// turns what is typed into the `address` and `args` properties and shows the model's back.
enum NetworkMessage {
    enum ParseError: Error, Equatable {
        /// A quote that is never closed.
        case unterminatedQuote
        /// `\I` and `\N` (OSC impulse and nil) are not argument types the engine can send.
        case unsupportedArgument
    }

    struct Message: Equatable {
        let address: String
        let args: [JSONValue]
    }

    /// The address and typed arguments (`{type, value}` objects as the engine's `OscArg`) for `text`.
    /// The address is not checked here: the engine accepts a bad one and shows the cue broken.
    static func parse(_ text: String) -> Result<Message, ParseError> {
        var tokens: [(text: String, quoted: Bool)] = []
        var chars = Array(text)[...]
        while true {
            while let c = chars.first, c.isWhitespace { chars = chars.dropFirst() }
            guard let first = chars.first else { break }
            if first == "\"" {
                chars = chars.dropFirst()
                var value = ""
                var closed = false
                while let c = chars.first {
                    chars = chars.dropFirst()
                    if c == "\"" {
                        closed = true
                        break
                    }
                    // A backslash before a quote or a backslash is that character.
                    if c == "\\", let next = chars.first, next == "\"" || next == "\\" {
                        value.append(next)
                        chars = chars.dropFirst()
                    } else {
                        value.append(c)
                    }
                }
                guard closed else { return .failure(.unterminatedQuote) }
                tokens.append((value, true))
            } else {
                var value = ""
                while let c = chars.first, !c.isWhitespace {
                    value.append(c)
                    chars = chars.dropFirst()
                }
                tokens.append((value, false))
            }
        }
        guard let head = tokens.first else { return .success(Message(address: "", args: [])) }
        var args: [JSONValue] = []
        for token in tokens.dropFirst() {
            if token.quoted {
                args.append(arg("string", .string(token.text)))
            } else if token.text == "\\T" || token.text == "\\F" {
                args.append(arg("bool", .bool(token.text == "\\T")))
            } else if token.text == "\\I" || token.text == "\\N" {
                return .failure(.unsupportedArgument)
            } else if isInteger(token.text), let n = Int32(token.text) {
                args.append(arg("int", .int(Int64(n))))
            } else if isFloat(token.text), let d = Float(token.text), d.isFinite {
                args.append(arg("float", .double(Double(d))))
            } else {
                // Includes a number too big for 32 bits: kept as the text typed rather than changed.
                args.append(arg("string", .string(token.text)))
            }
        }
        return .success(Message(address: head.text, args: args))
    }

    /// `address` and `args` as the line the user would type, so parsing it gives them back.
    static func format(address: String, args: [JSONValue]) -> String {
        var parts = [address.contains(where: \.isWhitespace) || address.contains("\"") ? quoted(address) : address]
        for a in args {
            switch (a["type"]?.stringValue, a["value"]) {
            case ("int", let v?): parts.append(v.doubleValue.map { String(Int64($0)) } ?? "0")
            case ("float", let v?): parts.append(v.doubleValue.map(floatText) ?? "0.0")
            case ("bool", let v?): parts.append(v == .bool(true) ? "\\T" : "\\F")
            case ("string", let v?): parts.append(stringText(v.stringValue ?? ""))
            default: continue
            }
        }
        return address.isEmpty && args.isEmpty ? "" : parts.joined(separator: " ")
    }

    private static func arg(_ type: String, _ value: JSONValue) -> JSONValue {
        .object(["type": .string(type), "value": value])
    }

    private static func isInteger(_ s: String) -> Bool {
        let digits = s.hasPrefix("-") ? s.dropFirst() : s[...]
        return !digits.isEmpty && digits.allSatisfy { $0.isASCII && $0.isNumber }
    }

    private static func isFloat(_ s: String) -> Bool {
        let body = s.hasPrefix("-") ? s.dropFirst() : s[...]
        let halves = body.split(separator: ".", maxSplits: 1, omittingEmptySubsequences: false)
        guard halves.count == 2, !(halves[0].isEmpty && halves[1].isEmpty) else { return false }
        return halves.allSatisfy { $0.allSatisfy { $0.isASCII && $0.isNumber } }
    }

    private static func floatText(_ d: Double) -> String {
        // A float is held in 32 bits: show the shortest text that reads back as the same one.
        let text = "\(Float(d))"
        // Exponent form would read back as a string; spell it out.
        return text.contains("e") ? String(format: "%.10f", d) : text
    }

    private static func quoted(_ s: String) -> String {
        "\"" + s.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"") + "\""
    }

    /// A string is quoted when typing it bare would read back as something else.
    private static func stringText(_ s: String) -> String {
        let plain =
            !s.isEmpty && !s.contains(where: \.isWhitespace) && !s.contains("\"") && !s.hasPrefix("\\")
            && !isInteger(s) && !isFloat(s)
        return plain ? s : quoted(s)
    }
}
