// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation

/// The stage edits (VID-01; docs/protocol.md, core `video.rs`): each one engine command and one
/// undo step.
extension Command {
    /// `AddStage` at `index`: a new stage of `width` x `height` pixels, hidden between cues.
    static func addStage(id: ModelID, name: String, width: Int, height: Int, index: Int) -> Command {
        Command(
            .object([
                "type": .string("add_stage"), "index": .int(Int64(index)),
                "stage": .object([
                    "id": .string(id.raw), "name": .string(name), "width": .int(Int64(width)),
                    "height": .int(Int64(height)), "keep_black": .bool(false), "layer": .int(1),
                ]),
            ]))
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
    /// Reads the displays and this workspace's stages' displays and states again (they are then
    /// kept current by `video_outputs` pushes).
    @discardableResult
    func refreshVideoOutputs() async -> Result<VideoOutputs, EngineError> {
        let displays = await client.send { .request(id: $0, .listDisplays) }
        let stages = await client.send { .request(id: $0, .listStageMappings) }
        switch (displays, stages) {
        case (.failure(let e), _), (_, .failure(let e)):
            return .failure(e)
        case (.success(let d), .success(let s)):
            guard case .displays(let list, let video)? = d.data, case .stageMappings(let entries)? = s.data else {
                return .failure(.encoding("unexpected answer to the display lists"))
            }
            let outputs = VideoOutputs(displays: list, video: video, stages: entries)
            setVideoOutputs(outputs)
            return .success(outputs)
        }
    }

    /// Shows `stage` on `display` (nil: on none) on this machine.
    func setStageMapping(_ stage: ModelID, display: String?) async -> Result<OkBody, EngineError> {
        await client.send { .request(id: $0, .setStageMapping(stage: stage, display: display)) }
    }
}
