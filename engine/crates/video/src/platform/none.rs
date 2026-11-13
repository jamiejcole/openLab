// SPDX-License-Identifier: GPL-3.0-or-later

//! Platforms with no video output yet (Windows, Linux): the stage host answers the engine,
//! reports `video: false` and no displays, and every stage fails with that reason. Compiled on
//! every platform so its tests run everywhere; used where there is no other.

use openlab_core::StageId;
use openlab_core::stage_link::{Display, StageOutput};

use crate::host::{Picture, Platform};

pub const NO_VIDEO: &str = "this computer's openLab has no video output";

pub struct NoVideo;

impl Platform for NoVideo {
    fn video(&self) -> bool {
        false
    }

    fn displays(&mut self) -> Vec<Display> {
        Vec::new()
    }

    fn show(&mut self, _: &StageOutput, _: &Display, _: Picture) -> Result<(), String> {
        Err(NO_VIDEO.into())
    }

    fn hide(&mut self, _: StageId) {}

    #[cfg(feature = "test-support")]
    fn probe(
        &mut self,
        _: &StageOutput,
        _: Picture,
        _: &[[f32; 2]],
    ) -> Result<Vec<[u8; 4]>, String> {
        Err(NO_VIDEO.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::Host;
    use openlab_core::stage_link::{FromStage, LINK_VERSION, StageState, ToStage};

    #[test]
    fn a_platform_without_video_says_so_and_fails_every_stage() {
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
            display: None,
            keep_black: true,
        };
        host.handle(
            ToStage::Stages {
                stages: vec![stage],
            },
            &mut out,
        );
        assert!(matches!(
            &out[0],
            FromStage::StageStatus {
                state: StageState::Failed { .. },
                ..
            }
        ));
    }
}
