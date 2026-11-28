// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The stage edits (VID-01; docs/protocol.md, core `video.rs`): each one engine command and one
/// undo step.
extension Command {
    /// `AddStage` at `index`: a new stage of `width` x `height` pixels on the top layer, keeping
    /// black between cues once a picture has played on it, with one region covering it sent to
    /// `route` when one is given (`region` its id).
    static func addStage(
        id: ModelID, name: String, width: Int, height: Int, index: Int, route: ModelID? = nil,
        region: ModelID? = nil
    ) -> Command {
        var stage: [String: JSONValue] = [
            "id": .string(id.raw), "name": .string(name), "width": .int(Int64(width)),
            "height": .int(Int64(height)), "keep_black": .bool(true), "layer": .int(Int64(StageLayer.top)),
        ]
        if let route, let region {
            stage["regions"] = .array([
                .object([
                    "id": .string(region.raw), "route": .string(route.raw), "x": .int(0), "y": .int(0),
                    "width": .int(Int64(width)), "height": .int(Int64(height)),
                ])
            ])
        }
        return Command(.object(["type": .string("add_stage"), "index": .int(Int64(index)), "stage": .object(stage)]))
    }

    /// `RemoveStage`. Its display choice on this machine is kept, so an undo brings it back there.
    static func removeStage(_ id: ModelID) -> Command {
        Command(.object(["type": .string("remove_stage"), "id": .string(id.raw)]))
    }

    /// `MoveStage` to `index` among the stages, counted after taking it out.
    static func moveStage(_ id: ModelID, to index: Int) -> Command {
        Command(.object(["type": .string("move_stage"), "id": .string(id.raw), "index": .int(Int64(index))]))
    }

    /// `SetStageProperty` `size` with `regions` set exactly as given (each with its id and route),
    /// not scaled: one undo step for the size and the regions together.
    static func resizeStage(_ id: ModelID, width: Int, height: Int, regions: [RegionRow]) -> Command {
        let list: [JSONValue] = regions.map { r in
            var region = r.bounds.json
            region["id"] = .string(r.id.raw)
            if let route = r.route { region["route"] = .string(route.raw) }
            return .object(region)
        }
        return setStage(
            id,
            .object([
                "type": .string("size"), "width": .int(Int64(width)), "height": .int(Int64(height)),
                "regions": .array(list),
            ]))
    }

    /// `SetStageProperty`: `property` is the property object (`name`, `size`, `keep_black`, `layer`).
    static func setStage(_ id: ModelID, _ property: JSONValue) -> Command {
        Command(.object(["type": .string("set_stage_property"), "id": .string(id.raw), "property": property]))
    }

    /// `AddRoute` at `index`: a route of `width` x `height`, not turned, filling its display (the
    /// display itself is this machine's choice, `set_route_display`, not part of the command).
    static func addRoute(id: ModelID, name: String, width: Int, height: Int, index: Int) -> Command {
        Command(
            .object([
                "type": .string("add_route"), "index": .int(Int64(index)),
                "route": .object([
                    "id": .string(id.raw), "name": .string(name), "width": .int(Int64(width)),
                    "height": .int(Int64(height)), "rotation": .int(0), "scaling": .string("fill"),
                ]),
            ]))
    }

    /// `RemoveRoute`: its regions are left with no route; the undo puts them back on it.
    static func removeRoute(_ id: ModelID) -> Command {
        Command(.object(["type": .string("remove_route"), "id": .string(id.raw)]))
    }

    /// `MoveRoute` to `index` among the routes, counted after taking it out.
    static func moveRoute(_ id: ModelID, to index: Int) -> Command {
        Command(.object(["type": .string("move_route"), "id": .string(id.raw), "index": .int(Int64(index))]))
    }

    /// `SetRouteProperty`: `property` is the property object (`name`, `size`, `rotation`, `scaling`).
    static func setRoute(_ id: ModelID, _ property: JSONValue) -> Command {
        Command(.object(["type": .string("set_route_property"), "id": .string(id.raw), "property": property]))
    }

    /// `SetRouteProperty` `guide_colour`: the colour its guides are drawn in (`#rrggbb`; nil: a default
    /// no other route has). Saved with the show, but no undo step.
    static func setRouteGuideColour(_ id: ModelID, hex: String?) -> Command {
        setRoute(id, .object(["type": .string("guide_colour"), "value": hex.map { .string($0) } ?? .null]))
    }

    /// `AddRegion` at `index` of `stage`: `bounds` in stage pixels from its top left, sent to
    /// `route` (nil: none).
    static func addRegion(stage: ModelID, index: Int, id: ModelID, route: ModelID?, bounds: RegionBounds) -> Command {
        var region = bounds.json
        region["id"] = .string(id.raw)
        if let route { region["route"] = .string(route.raw) }
        return Command(
            .object([
                "type": .string("add_region"), "stage": .string(stage.raw), "index": .int(Int64(index)),
                "region": .object(region),
            ]))
    }

    static func removeRegion(stage: ModelID, _ id: ModelID) -> Command {
        Command(.object(["type": .string("remove_region"), "stage": .string(stage.raw), "id": .string(id.raw)]))
    }

    /// `SetRegionProperty` `bounds`: where the region is on its stage.
    static func setRegionBounds(stage: ModelID, _ id: ModelID, _ bounds: RegionBounds) -> Command {
        var property = bounds.json
        property["type"] = .string("bounds")
        return setRegion(stage: stage, id, .object(property))
    }

    /// `SetRegionProperty` `route`: the route it is sent to (nil: none).
    static func setRegionRoute(stage: ModelID, _ id: ModelID, route: ModelID?) -> Command {
        setRegion(
            stage: stage, id, .object(["type": .string("route"), "value": route.map { .string($0.raw) } ?? .null]))
    }

    private static func setRegion(stage: ModelID, _ id: ModelID, _ property: JSONValue) -> Command {
        Command(
            .object([
                "type": .string("set_region_property"), "stage": .string(stage.raw), "id": .string(id.raw),
                "property": property,
            ]))
    }
}

/// A stage's layer (docs/file-format.md §4.7): 0 at the bottom to 1000 on top.
enum StageLayer {
    static let top = 1000
    static let bottom = 0
}

/// A region's place on its stage (docs/file-format.md §4.8): stage pixels from the top left.
struct RegionBounds: Equatable, Sendable {
    var x: Int
    var y: Int
    var width: Int
    var height: Int

    var json: [String: JSONValue] {
        [
            "x": .int(Int64(x)), "y": .int(Int64(y)), "width": .int(Int64(width)),
            "height": .int(Int64(height)),
        ]
    }

    /// Whether it lies wholly inside a stage of `width` x `height`, at least a pixel each way.
    func inside(width stageWidth: Int, height stageHeight: Int) -> Bool {
        // Compared by subtraction, which cannot overflow once x and y are at least 0.
        x >= 0 && y >= 0 && width >= 1 && height >= 1 && width <= stageWidth - x && height <= stageHeight - y
    }
}

extension WorkspaceStore {
    /// Reads the displays, this workspace's routes' displays and states and its stages' black
    /// again (they are then kept current by `video_outputs` pushes).
    @discardableResult
    func refreshVideoOutputs() async -> Result<VideoOutputs, EngineError> {
        let displays = await client.send { .request(id: $0, .listDisplays) }
        let routes = await client.send { .request(id: $0, .listRouteDisplays) }
        switch (displays, routes) {
        case (.failure(let e), _), (_, .failure(let e)):
            return .failure(e)
        case (.success(let d), .success(let r)):
            guard case .displays(let list, let video)? = d.data,
                case .routeDisplays(let routes, let stages)? = r.data
            else {
                return .failure(.encoding("unexpected answer to the display lists"))
            }
            let outputs = VideoOutputs(displays: list, video: video, routes: routes, stages: stages)
            setVideoOutputs(outputs)
            return .success(outputs)
        }
    }

    /// Shows output `route` on `display` (nil: on none) on this machine: not an undo step.
    func setRouteDisplay(_ route: ModelID, display: String?, partial: DisplayPart? = nil) async
        -> Result<OkBody, EngineError>
    {
        await client.send { .request(id: $0, .setRouteDisplay(route: route, display: display, partial: partial)) }
    }

    /// Opens `stage`'s monitor window (or brings it to the front), or closes it: a live copy of the
    /// stage the stage host draws in a window of its own.
    func setStageMonitor(_ stage: ModelID, open: Bool) async -> Result<OkBody, EngineError> {
        await client.send { .request(id: $0, .setStageMonitor(stage: stage, open: open)) }
    }

    /// Shows or hides the alignment guides of `routes` on their displays: a view aid the engine
    /// keeps while the workspace is open, never saved and no undo step.
    func setRouteGuides(_ routes: [ModelID], show: Bool) async -> Result<OkBody, EngineError> {
        await client.send { .request(id: $0, .setRouteGuides(routes: routes, show: show)) }
    }
}
