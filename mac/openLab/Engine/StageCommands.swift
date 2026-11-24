// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The stage edits (VID-01; docs/protocol.md, core `video.rs`): each one engine command and one
/// undo step.
extension Command {
    /// `AddStage` at `index`: a new stage of `width` x `height` pixels, keeping black between cues
    /// once a picture has played on it, with one region covering it sent to `route` when one is
    /// given (`region` its id).
    static func addStage(
        id: ModelID, name: String, width: Int, height: Int, index: Int, route: ModelID? = nil,
        region: ModelID? = nil
    ) -> Command {
        var stage: [String: JSONValue] = [
            "id": .string(id.raw), "name": .string(name), "width": .int(Int64(width)),
            "height": .int(Int64(height)), "keep_black": .bool(true), "layer": .int(1),
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

    /// `SetStageProperty`: `property` is the property object (`name`, `size`, `keep_black`, `layer`).
    static func setStage(_ id: ModelID, _ property: JSONValue) -> Command {
        Command(.object(["type": .string("set_stage_property"), "id": .string(id.raw), "property": property]))
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
    func setRouteDisplay(_ route: ModelID, display: String?) async -> Result<OkBody, EngineError> {
        await client.send { .request(id: $0, .setRouteDisplay(route: route, display: display)) }
    }

    /// Opens `stage`'s monitor window (or brings it to the front), or closes it: a live copy of the
    /// stage the stage host draws in a window of its own.
    func setStageMonitor(_ stage: ModelID, open: Bool) async -> Result<OkBody, EngineError> {
        await client.send { .request(id: $0, .setStageMonitor(stage: stage, open: open)) }
    }
}
