// SPDX-License-Identifier: GPL-3.0-or-later

//! Platforms with no video output yet (Windows, Linux): the stage host answers the engine,
//! reports `video: false` and no displays, and every route fails with that reason. Compiled on
//! every platform so its tests run everywhere; used where there is no other.

use openlab_core::stage_link::Display;

use crate::host::{Guide, Layer, Platform, Presented};

pub const NO_VIDEO: &str = "this computer's openLab has no video output";

pub struct NoVideo;

impl Platform for NoVideo {
    fn video(&self) -> bool {
        false
    }

    fn displays(&mut self) -> Vec<Display> {
        Vec::new()
    }

    fn present(&mut self, _: &Display, _: &[Layer], _: &[Guide]) -> Result<Presented, String> {
        Err(NO_VIDEO.into())
    }

    fn close(&mut self, _: &str) {}

    #[cfg(feature = "test-support")]
    fn probe(
        &mut self,
        _: (u32, u32),
        _: &[Layer],
        _: &[Guide],
        _: &[[f32; 2]],
    ) -> Result<Vec<[u8; 4]>, String> {
        Err(NO_VIDEO.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::Host;
    use openlab_core::StageId;
    use openlab_core::stage_link::StageOutput;
    use openlab_core::stage_link::{FromStage, LINK_VERSION, RouteState, ToStage};

    #[test]
    fn a_platform_without_video_says_so_and_fails_every_route() {
        let mut host = Host::new(NoVideo);
        let mut out = Vec::new();
        host.handle(
            ToStage::Hello {
                version: LINK_VERSION,
            },
            &mut out,
        );
        assert!(matches!(out[0], FromStage::Hello { video: false, .. }));
        out.clear();
        let id = StageId(uuid::Uuid::from_u128(1));
        let stage = StageOutput {
            id,
            width: 8,
            height: 8,
            regions: Vec::new(),
            keep_black: true,
            stack: 0,
            monitor: Some("Stage".into()),
        };
        host.handle(
            ToStage::Stages {
                stages: vec![stage],
                routes: vec![openlab_core::stage_link::RouteOutput {
                    id: openlab_core::RouteId(uuid::Uuid::from_u128(1)),
                    width: 8,
                    height: 8,
                    rotation: 0,
                    scaling: openlab_core::RouteScaling::Fill,
                    display: None,
                    guide: None,
                }],
            },
            &mut out,
        );
        assert!(matches!(
            &out[0],
            FromStage::RouteStatus {
                state: RouteState::Failed { .. },
                ..
            }
        ));
        // No monitor either: nothing to draw it with.
        out.clear();
        host.handle(
            ToStage::ProbeMonitor {
                stage: id,
                width: 64,
                height: 48,
                points: vec![[0.5, 0.5]],
            },
            &mut out,
        );
        assert!(matches!(
            out.last(),
            Some(FromStage::MonitorProbed { colours, .. }) if colours.is_empty()
        ));
    }
}
