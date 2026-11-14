// SPDX-License-Identifier: GPL-3.0-or-later

//! Drawing a stage with Metal: clear to black, then each picture on it as a textured quad.
//! The same draw goes to an output window's drawable or to an offscreen texture a probe reads.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
#[cfg(feature = "test-support")]
use objc2_metal::{
    MTLBlitCommandEncoder, MTLBuffer, MTLOrigin, MTLResourceOptions, MTLSize, MTLStorageMode,
    MTLTextureDescriptor, MTLTextureUsage,
};
use objc2_metal::{
    MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
    MTLCreateSystemDefaultDevice, MTLDevice, MTLLibrary, MTLLoadAction, MTLPixelFormat,
    MTLPrimitiveType, MTLRenderCommandEncoder, MTLRenderPassDescriptor,
    MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLStoreAction, MTLTexture,
};

use crate::host::Picture;

/// One quad covering `rect` (x0, y0, x1, y1 in normalised device coordinates), sampling its
/// texture with nearest-neighbour filtering.
const SHADER: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct Quad { float4 rect; };
struct V { float4 pos [[position]]; float2 uv; };
vertex V quad_vertex(uint vid [[vertex_id]], constant Quad& q [[buffer(0)]]) {
    float2 corner = float2(vid & 1, vid >> 1);
    V o;
    o.pos = float4(mix(q.rect.xy, q.rect.zw, corner), 0, 1);
    o.uv = float2(corner.x, 1 - corner.y);
    return o;
}
fragment float4 quad_fragment(V in [[stage_in]], texture2d<float> tex [[texture(0)]]) {
    constexpr sampler s(filter::nearest, address::clamp_to_edge);
    return tex.sample(s, in.uv);
}
"#;

/// The largest offscreen probe target, in pixels a side.
#[cfg(feature = "test-support")]
const MAX_PROBE_SIDE: u32 = 1024;

pub struct Gpu {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    /// The test card last drawn, by frame number.
    #[cfg(feature = "test-support")]
    card: Option<(u16, Retained<ProtocolObject<dyn MTLTexture>>)>,
}

impl Gpu {
    /// The system's Metal device with the stage pipeline built.
    pub fn new() -> Result<Self, String> {
        let device = MTLCreateSystemDefaultDevice().ok_or("this computer has no Metal device")?;
        let queue = device
            .newCommandQueue()
            .ok_or("could not create a Metal command queue")?;
        let library = device
            .newLibraryWithSource_options_error(&NSString::from_str(SHADER), None)
            .map_err(|e| format!("could not build the stage shader: {e}"))?;
        let function = |name: &str| {
            library
                .newFunctionWithName(&NSString::from_str(name))
                .ok_or_else(|| format!("stage shader has no {name}"))
        };
        let desc = MTLRenderPipelineDescriptor::new();
        let vertex = function("quad_vertex")?;
        let fragment = function("quad_fragment")?;
        desc.setVertexFunction(Some(&vertex));
        desc.setFragmentFunction(Some(&fragment));
        // SAFETY: colour attachment 0 always exists on a pipeline descriptor.
        let attachment = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
        attachment.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        let pipeline = device
            .newRenderPipelineStateWithDescriptor_error(&desc)
            .map_err(|e| format!("could not build the stage pipeline: {e}"))?;
        Ok(Self {
            device,
            queue,
            pipeline,
            #[cfg(feature = "test-support")]
            card: None,
        })
    }

    pub fn device(&self) -> &ProtocolObject<dyn MTLDevice> {
        &self.device
    }

    /// Encodes the stage drawing `picture` into `target`, the stage filling `rect` of it.
    pub fn draw(
        &mut self,
        target: &ProtocolObject<dyn MTLTexture>,
        rect: [f32; 4],
        picture: Picture,
    ) -> Result<Retained<ProtocolObject<dyn MTLCommandBuffer>>, String> {
        let texture = self.texture_for(picture)?;
        let cb = self
            .queue
            .commandBuffer()
            .ok_or("could not create a Metal command buffer")?;
        let pass = MTLRenderPassDescriptor::new();
        // SAFETY: colour attachment 0 always exists on a render pass descriptor.
        let a = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        a.setTexture(Some(target));
        a.setLoadAction(MTLLoadAction::Clear);
        a.setClearColor(MTLClearColor {
            red: 0.0,
            green: 0.0,
            blue: 0.0,
            alpha: 1.0,
        });
        a.setStoreAction(MTLStoreAction::Store);
        let enc = cb
            .renderCommandEncoderWithDescriptor(&pass)
            .ok_or("could not create a Metal render encoder")?;
        if let Some(texture) = texture {
            enc.setRenderPipelineState(&self.pipeline);
            let quad = rect;
            let bytes = NonNull::from(&quad).cast::<c_void>();
            // SAFETY: `quad` is 16 bytes of plain floats matching the shader's `Quad`, alive
            // for the call (Metal copies inline bytes); texture index 0 is the shader's `tex`;
            // a strip of 4 vertices is the shader's quad.
            unsafe {
                enc.setVertexBytes_length_atIndex(bytes, size_of::<[f32; 4]>(), 0);
                enc.setFragmentTexture_atIndex(Some(&texture), 0);
                enc.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::TriangleStrip, 0, 4);
            }
        }
        enc.endEncoding();
        Ok(cb)
    }

    /// The texture `picture` is drawn from; `None` for black (nothing to draw over the clear).
    fn texture_for(
        &mut self,
        picture: Picture,
    ) -> Result<Option<Retained<ProtocolObject<dyn MTLTexture>>>, String> {
        match picture {
            Picture::Black => Ok(None),
            #[cfg(feature = "test-support")]
            Picture::TestCard(frame) => {
                if let Some((f, t)) = &self.card
                    && *f == frame
                {
                    return Ok(Some(t.clone()));
                }
                let t = self.card_texture(frame)?;
                self.card = Some((frame, t.clone()));
                Ok(Some(t))
            }
        }
    }

    #[cfg(feature = "test-support")]
    fn card_texture(&self, frame: u16) -> Result<Retained<ProtocolObject<dyn MTLTexture>>, String> {
        use crate::card;
        // The card is RGBA; the texture is BGRA (the drawable's format): swap red and blue.
        let mut px = card::pixels(frame);
        for p in px.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
        }
        // SAFETY: a plain 2D descriptor of the card's size.
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                card::WIDTH,
                card::HEIGHT,
                false,
            )
        };
        desc.setUsage(MTLTextureUsage::ShaderRead);
        let texture = self
            .device
            .newTextureWithDescriptor(&desc)
            .ok_or("could not create the test card texture")?;
        let region = objc2_metal::MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize {
                width: card::WIDTH,
                height: card::HEIGHT,
                depth: 1,
            },
        };
        let bytes = NonNull::new(px.as_mut_ptr().cast::<c_void>()).ok_or("no card pixels")?;
        // SAFETY: `px` holds exactly WIDTH x HEIGHT BGRA pixels, rows of WIDTH*4 bytes, the
        // texture's size and format; Metal copies them before returning.
        unsafe {
            texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                region,
                0,
                bytes,
                card::WIDTH * 4,
            );
        }
        Ok(texture)
    }

    /// Draws `stage_size` with `picture` offscreen and reads the colour (RGBA) at `points`
    /// (fractions of the stage from the top left). Waits for the GPU: tests only.
    #[cfg(feature = "test-support")]
    pub fn probe(
        &mut self,
        stage_size: (u32, u32),
        picture: Picture,
        points: &[[f32; 2]],
    ) -> Result<Vec<[u8; 4]>, String> {
        let (w, h) = probe_size(stage_size);
        // SAFETY: a plain 2D descriptor within Metal's size limits (probe_size caps it).
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                w,
                h,
                false,
            )
        };
        desc.setUsage(MTLTextureUsage::RenderTarget);
        desc.setStorageMode(MTLStorageMode::Private);
        let target = self
            .device
            .newTextureWithDescriptor(&desc)
            .ok_or("could not create the probe target")?;
        let bytes = w * h * 4;
        let buffer = self
            .device
            .newBufferWithLength_options(bytes, MTLResourceOptions::StorageModeShared)
            .ok_or("could not create the probe buffer")?;
        let cb = self.draw(&target, [-1.0, -1.0, 1.0, 1.0], picture)?;
        let blit = cb
            .blitCommandEncoder()
            .ok_or("could not create a Metal blit encoder")?;
        // SAFETY: copies the whole w x h BGRA target into a buffer of exactly w*h*4 bytes,
        // rows of w*4 bytes.
        unsafe {
            blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                &target,
                0,
                0,
                MTLOrigin { x: 0, y: 0, z: 0 },
                MTLSize { width: w, height: h, depth: 1 },
                &buffer,
                0,
                w * 4,
                bytes,
            );
        }
        blit.endEncoding();
        cb.commit();
        cb.waitUntilCompleted();
        if let Some(e) = cb.error() {
            return Err(format!("the GPU failed the probe: {e}"));
        }
        // SAFETY: a shared buffer of `bytes` bytes the finished command buffer wrote; nothing
        // else writes it while this slice lives.
        let px =
            unsafe { std::slice::from_raw_parts(buffer.contents().as_ptr().cast::<u8>(), bytes) };
        Ok(points
            .iter()
            .map(|&[x, y]| {
                let col = ((x * w as f32) as usize).min(w - 1);
                let row = ((y * h as f32) as usize).min(h - 1);
                let i = (row * w + col) * 4;
                match px.get(i..i + 4) {
                    Some(&[b, g, r, a]) => [r, g, b, a],
                    _ => [0, 0, 0, 0],
                }
            })
            .collect())
    }
}

/// The offscreen size a probe draws a stage at: the stage's shape, at most
/// [`MAX_PROBE_SIDE`] a side, at least 1.
#[cfg(feature = "test-support")]
fn probe_size((w, h): (u32, u32)) -> (usize, usize) {
    let (w, h) = (f64::from(w.max(1)), f64::from(h.max(1)));
    let scale = (f64::from(MAX_PROBE_SIDE) / w.max(h)).min(1.0);
    (
        ((w * scale).round() as usize).max(1),
        ((h * scale).round() as usize).max(1),
    )
}

/// Where a stage of `stage` pixels sits in a display of `display` pixels: as large as fits,
/// its shape kept, centred (black bars elsewhere), in normalised device coordinates.
pub fn fit((sw, sh): (u32, u32), (dw, dh): (u32, u32)) -> [f32; 4] {
    let s = f64::from(sw.max(1)) / f64::from(sh.max(1));
    let d = f64::from(dw.max(1)) / f64::from(dh.max(1));
    let (x, y) = if s > d { (1.0, d / s) } else { (s / d, 1.0) };
    [-x as f32, -y as f32, x as f32, y as f32]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stage_fits_its_display_keeping_its_shape() {
        assert_eq!(fit((1920, 1080), (1920, 1080)), [-1.0, -1.0, 1.0, 1.0]);
        assert_eq!(
            fit((1080, 1080), (1920, 1080)),
            [-0.5625, -1.0, 0.5625, 1.0]
        );
        assert_eq!(fit((3840, 1080), (1920, 1080)), [-1.0, -0.5, 1.0, 0.5]);
        assert_eq!(fit((0, 0), (0, 0)), [-1.0, -1.0, 1.0, 1.0]);
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn probes_draw_at_most_the_largest_side_in_the_stage_shape() {
        assert_eq!(probe_size((1920, 1080)), (1024, 576));
        assert_eq!(probe_size((16, 9)), (16, 9));
        assert_eq!(probe_size((16_384, 1)), (1024, 1));
    }

    /// The draw itself, on this machine's GPU: a black stage reads black; the test card reads
    /// its frame number and its four colours. (Fails, not skips, on a Mac without Metal.)
    #[cfg(feature = "test-support")]
    #[test]
    fn the_gpu_draws_black_and_the_test_card_as_they_read_back() {
        use crate::card;
        let mut gpu = Gpu::new().unwrap();
        let quads = card::quadrant_points();
        let black = gpu.probe((1920, 1080), Picture::Black, &quads).unwrap();
        assert_eq!(black, vec![[0, 0, 0, 255]; 4]);
        let mut points = card::barcode_points().to_vec();
        points.extend_from_slice(&quads);
        let read = gpu
            .probe((1280, 720), Picture::TestCard(4242), &points)
            .unwrap();
        assert_eq!(card::read_frame(&read[..card::CELLS]), Some(4242));
        assert_eq!(
            read[card::CELLS..],
            [card::RED, card::GREEN, card::BLUE, card::YELLOW]
        );
    }
}
