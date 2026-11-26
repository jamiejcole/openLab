// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// A Fade cue's curve as the engine stores it (`kind.curve`, docs/file-format.md §5.2): the audio
/// domain and a rising and a falling shape, each a type with its parameter and control points. The
/// drawing maths copy the engine's (`fade_curve.rs`), so a graph shows what the fade does.
struct FadeCurveSpec: Equatable {
    enum Kind: String, CaseIterable {
        case sCurve = "s_curve"
        case custom
        case parametric
        case linear

        /// The types whose control points are edited by hand.
        var hasPoints: Bool { self == .custom || self == .linear }
    }

    enum Domain: String, CaseIterable {
        case slider
        case decibel
        case linear
    }

    /// A control point as drawn: `t` from the fade's start (0) to its end (1), `v` from the bottom
    /// of the graph (0) to its top (1).
    struct Point: Equatable {
        var t: Double
        var v: Double
    }

    struct Shape: Equatable {
        var kind: Kind = .sCurve
        var parameter: Double = 1
        var points: [Point] = []

        var json: JSONValue {
            .object([
                "type": .string(kind.rawValue),
                "parameter": .double(parameter),
                "points": .array(points.map { .object(["t": .double($0.t), "v": .double($0.v)]) }),
            ])
        }
    }

    /// The engine's limits (`MAX_CURVE_POINTS`, `CURVE_PARAMETER_RANGE`).
    static let maxPoints = 64
    static let parameterRange = 0.01...100.0

    var domain: Domain = .slider
    var rising = Shape()
    var falling = Shape()

    init() {}

    /// The curve `json` holds; anything it does not say (or says wrongly) is the default's.
    init(json: JSONValue?) {
        domain = json?["audio_domain"]?.stringValue.flatMap(Domain.init(rawValue:)) ?? .slider
        rising = Self.shape(json?["rising"])
        falling = Self.shape(json?["falling"])
    }

    private static func shape(_ json: JSONValue?) -> Shape {
        Shape(
            kind: json?["type"]?.stringValue.flatMap(Kind.init(rawValue:)) ?? .sCurve,
            parameter: json?["parameter"]?.doubleValue ?? 1,
            points: (json?["points"]?.arrayValue ?? []).compactMap { p in
                guard let t = p["t"]?.doubleValue, let v = p["v"]?.doubleValue else { return nil }
                return Point(t: t, v: v)
            })
    }

    var json: JSONValue {
        .object(["audio_domain": .string(domain.rawValue), "rising": rising.json, "falling": falling.json])
    }

    /// `points` of one graph as the other graph draws them mirrored (time reversed, same height).
    static func mirrored(_ points: [Point]) -> [Point] {
        points.reversed().map { Point(t: 1 - $0.t, v: $0.v) }
    }

    /// The height (0 bottom, 1 top) `shape` is drawn at at `t` in the rising or the falling graph:
    /// a rising graph runs from the bottom left to the top right, a falling one from the top left to
    /// the bottom right.
    static func drawn(_ shape: Shape, rising: Bool, at t: Double) -> Double {
        let t = t.isNaN ? 0 : min(max(t, 0), 1)
        let u = rising ? t : 1 - t
        let v: Double
        switch shape.kind {
        case .sCurve: v = (1 - cos(u * .pi)) * 0.5
        case .parametric: v = pow(sin(u * .pi / 2), shape.parameter)
        case .linear, .custom:
            let ts = [0] + shape.points.map(\.t) + [1]
            let vs = [rising ? 0.0 : 1.0] + shape.points.map(\.v) + [rising ? 1.0 : 0.0]
            v = shape.kind == .linear ? lines(ts, vs, t) : cubic(ts, vs, tangents(ts, vs), t)
        }
        return min(max(v, 0), 1)
    }

    /// Fritsch-Carlson tangents, as the engine's.
    static func tangents(_ ts: [Double], _ vs: [Double]) -> [Double] {
        let n = ts.count
        func slope(_ i: Int) -> Double {
            guard i >= 0, i + 1 < n, ts[i + 1] > ts[i] else { return 0 }
            return (vs[i + 1] - vs[i]) / (ts[i + 1] - ts[i])
        }
        return (0..<n).map { i in
            if i == 0 { return slope(0) }
            if i + 1 == n { return slope(i - 1) }
            let (d0, d1) = (slope(i - 1), slope(i))
            if d0 * d1 <= 0 { return 0 }
            let (h0, h1) = (ts[i] - ts[i - 1], ts[i + 1] - ts[i])
            let (w1, w2) = (2 * h1 + h0, h1 + 2 * h0)
            return (w1 + w2) / (w1 / d0 + w2 / d1)
        }
    }

    private static func interval(_ ts: [Double], _ t: Double) -> Int {
        let i = ts.firstIndex { $0 > t } ?? ts.count
        return min(max(i - 1, 0), max(ts.count - 2, 0))
    }

    private static func lines(_ ts: [Double], _ vs: [Double], _ t: Double) -> Double {
        let i = interval(ts, t)
        guard i + 1 < ts.count, ts[i + 1] > ts[i] else { return t }
        return vs[i] + (vs[i + 1] - vs[i]) * ((t - ts[i]) / (ts[i + 1] - ts[i]))
    }

    private static func cubic(_ ts: [Double], _ vs: [Double], _ ms: [Double], _ t: Double) -> Double {
        let i = interval(ts, t)
        guard i + 1 < ts.count, ts[i + 1] > ts[i] else { return t }
        let h = ts[i + 1] - ts[i]
        let s = (t - ts[i]) / h
        let (s2, s3) = (s * s, s * s * s)
        let h00 = 2 * s3 - 3 * s2 + 1
        let h10 = s3 - 2 * s2 + s
        let h01 = -2 * s3 + 3 * s2
        let h11 = s3 - s2
        return h00 * vs[i] + h10 * h * ms[i] + h01 * vs[i + 1] + h11 * h * ms[i + 1]
    }
}
