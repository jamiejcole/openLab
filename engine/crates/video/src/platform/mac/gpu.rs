// SPDX-License-Identifier: GPL-3.0-or-later

//! Drawing a stage with Metal: clear to black, then each picture on it as a textured quad.
//! The same draw goes to an output window's drawable or to an offscreen texture a probe reads.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlendFactor, MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
    MTLCreateSystemDefaultDevice, MTLDevice, MTLLibrary, MTLLoadAction, MTLOrigin, MTLPixelFormat,
    MTLPrimitiveType, MTLRegion, MTLRenderCommandEncoder, MTLRenderPassDescriptor,
    MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLSize, MTLStoreAction, MTLTexture,
    MTLTextureDescriptor, MTLTextureUsage,
};
#[cfg(feature = "test-support")]
use objc2_metal::{MTLBlitCommandEncoder, MTLBuffer, MTLResourceOptions, MTLStorageMode};

use crate::host::{CuePicture, Layer, Picture};

/// A texture the stage drawing samples.
pub type Texture = Retained<ProtocolObject<dyn MTLTexture>>;

/// Where a cue's picture comes from when a stage is drawn: its texture and pixel size at the
/// picture's position, or `None` (not drawn; the source has noted why).
pub type Pictures<'a> = dyn FnMut(&CuePicture) -> Option<(Texture, (u32, u32))> + 'a;

/// One quad covering `rect` (x0, y0, x1, y1 in normalised device coordinates), sampling its
/// texture with nearest-neighbour filtering, at opacity `tint.a` over what is below.
const SHADER: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct Quad { float4 rect; float4 tint; };
struct V { float4 pos [[position]]; float2 uv; };
vertex V quad_vertex(uint vid [[vertex_id]], constant Quad& q [[buffer(0)]]) {
    float2 corner = float2(vid & 1, vid >> 1);
    V o;
    o.pos = float4(mix(q.rect.xy, q.rect.zw, corner), 0, 1);
    o.uv = float2(corner.x, 1 - corner.y);
    return o;
}
fragment float4 quad_fragment(V in [[stage_in]], texture2d<float> tex [[texture(0)]],
                              constant Quad& q [[buffer(0)]]) {
    constexpr sampler s(filter::nearest, address::clamp_to_edge);
    float4 c = tex.sample(s, in.uv);
    return float4(c.rgb, q.tint.a);
}
"#;

/// The largest offscreen probe target, in pixels a side.
#[cfg(feature = "test-support")]
const MAX_PROBE_SIDE: u32 = 1024;

/// Test cards kept, by frame number.
#[cfg(feature = "test-support")]
const MAX_CARDS: usize = 8;

pub struct Gpu {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    /// One black pixel: a stage keeping black is a black quad over what is below it.
    black: Retained<ProtocolObject<dyn MTLTexture>>,
    /// Test cards drawn lately, by frame number.
    #[cfg(feature = "test-support")]
    cards: Vec<(u16, Retained<ProtocolObject<dyn MTLTexture>>)>,
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
        // A fading picture is mixed over what is below it by its opacity.
        attachment.setBlendingEnabled(true);
        attachment.setSourceRGBBlendFactor(MTLBlendFactor::SourceAlpha);
        attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        attachment.setSourceAlphaBlendFactor(MTLBlendFactor::One);
        attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        let pipeline = device
            .newRenderPipelineStateWithDescriptor_error(&desc)
            .map_err(|e| format!("could not build the stage pipeline: {e}"))?;
        let black = texture(&device, 1, 1, &mut [0, 0, 0, 255])?;
        Ok(Self {
            device,
            queue,
            pipeline,
            black,
            #[cfg(feature = "test-support")]
            cards: Vec::new(),
        })
    }

    pub fn device(&self) -> &ProtocolObject<dyn MTLDevice> {
        &self.device
    }

    /// The device, held apart from `self` (drawing borrows `self`).
    pub fn device_owned(&self) -> Retained<ProtocolObject<dyn MTLDevice>> {
        self.device.clone()
    }

    /// Encodes a clear to black and then `layers` (bottom first) into `target`, each stage
    /// fitted to the target, shape kept, and on each stage its cues' pictures (earliest started
    /// first), each fitted to the stage at its opacity.
    pub fn draw(
        &mut self,
        target: &ProtocolObject<dyn MTLTexture>,
        layers: &[Layer],
        pictures: &mut Pictures<'_>,
    ) -> Result<Retained<ProtocolObject<dyn MTLCommandBuffer>>, String> {
        let size = (
            u32::try_from(target.width()).unwrap_or(u32::MAX),
            u32::try_from(target.height()).unwrap_or(u32::MAX),
        );
        let mut quads = Vec::with_capacity(layers.len());
        for layer in layers {
            let stage = fit((layer.width, layer.height), size);
            quads.push(([stage, [1.0; 4]], self.texture_for(layer.picture)?));
            for cue in &layer.cues {
                let Some((texture, picture)) = pictures(cue) else {
                    continue;
                };
                let inner = within(stage, fit(picture, (layer.width, layer.height)));
                quads.push((
                    [inner, [1.0, 1.0, 1.0, cue.opacity.clamp(0.0, 1.0)]],
                    texture,
                ));
            }
        }
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
        enc.setRenderPipelineState(&self.pipeline);
        for (rect, texture) in &quads {
            let bytes = NonNull::from(rect).cast::<c_void>();
            // SAFETY: `rect` is 32 bytes of plain floats matching the shader's `Quad`, alive
            // for the call (Metal copies inline bytes); texture index 0 is the shader's `tex`;
            // a strip of 4 vertices is the shader's quad.
            unsafe {
                enc.setVertexBytes_length_atIndex(bytes, size_of::<[[f32; 4]; 2]>(), 0);
                enc.setFragmentBytes_length_atIndex(bytes, size_of::<[[f32; 4]; 2]>(), 0);
                enc.setFragmentTexture_atIndex(Some(texture), 0);
                enc.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::TriangleStrip, 0, 4);
            }
        }
        enc.endEncoding();
        Ok(cb)
    }

    /// The texture `picture` is drawn from.
    fn texture_for(
        &mut self,
        picture: Picture,
    ) -> Result<Retained<ProtocolObject<dyn MTLTexture>>, String> {
        match picture {
            Picture::Black => Ok(self.black.clone()),
            #[cfg(feature = "test-support")]
            Picture::TestCard(frame) => {
                if let Some((_, t)) = self.cards.iter().find(|(f, _)| *f == frame) {
                    return Ok(t.clone());
                }
                let mut px = crate::card::pixels(frame);
                // The card is RGBA; the texture is BGRA (the drawable's format).
                for p in px.as_chunks_mut::<4>().0 {
                    p.swap(0, 2);
                }
                let t = texture(
                    &self.device,
                    crate::card::WIDTH,
                    crate::card::HEIGHT,
                    &mut px,
                )?;
                if self.cards.len() >= MAX_CARDS {
                    self.cards.remove(0);
                }
                self.cards.push((frame, t.clone()));
                Ok(t)
            }
        }
    }

    /// Draws `layers` offscreen on a target of `size` (scaled down to at most
    /// [`MAX_PROBE_SIDE`] a side) and reads the colour (RGBA) at `points` (fractions of the
    /// target from the top left). Waits for the GPU: tests only.
    #[cfg(feature = "test-support")]
    pub fn probe(
        &mut self,
        size: (u32, u32),
        layers: &[Layer],
        points: &[[f32; 2]],
        pictures: &mut Pictures<'_>,
    ) -> Result<Vec<[u8; 4]>, String> {
        let (w, h) = probe_size(size);
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
        let cb = self.draw(&target, layers, pictures)?;
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

/// Replaces `texture`'s pixels (of `size`) with `px`, rows of `stride` bytes.
pub fn replace(
    texture: &Texture,
    size: (u32, u32),
    px: &mut [u8],
    stride: usize,
) -> Result<(), String> {
    let (w, h) = (size.0 as usize, size.1 as usize);
    if texture.width() != w || texture.height() != h || stride < w * 4 || px.len() < stride * h {
        return Err("a frame of the wrong size".into());
    }
    let region = MTLRegion {
        origin: MTLOrigin { x: 0, y: 0, z: 0 },
        size: MTLSize {
            width: w,
            height: h,
            depth: 1,
        },
    };
    let bytes = NonNull::new(px.as_mut_ptr().cast::<c_void>()).ok_or("no pixels")?;
    // SAFETY: `px` holds at least h rows of `stride` bytes, each starting with w BGRA pixels
    // (checked above), the texture's size and format; Metal copies them before returning.
    unsafe { texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(region, 0, bytes, stride) };
    Ok(())
}

/// A shader-readable BGRA texture of `width` x `height`, its pixels not yet written.
pub fn empty_texture(
    device: &ProtocolObject<dyn MTLDevice>,
    width: usize,
    height: usize,
) -> Result<Texture, String> {
    if width == 0 || height == 0 {
        return Err("a texture of no size".into());
    }
    // SAFETY: a plain 2D descriptor; the sizes were checked against the picture limits.
    let desc = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            MTLPixelFormat::BGRA8Unorm,
            width,
            height,
            false,
        )
    };
    desc.setUsage(MTLTextureUsage::ShaderRead);
    device
        .newTextureWithDescriptor(&desc)
        .ok_or_else(|| "could not create a texture".into())
}

fn texture(
    device: &ProtocolObject<dyn MTLDevice>,
    width: usize,
    height: usize,
    px: &mut [u8],
) -> Result<Retained<ProtocolObject<dyn MTLTexture>>, String> {
    if px.len() != width * height * 4 {
        return Err("texture pixels of the wrong size".into());
    }
    // SAFETY: a plain 2D descriptor of a small size.
    let desc = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            MTLPixelFormat::BGRA8Unorm,
            width,
            height,
            false,
        )
    };
    desc.setUsage(MTLTextureUsage::ShaderRead);
    let texture = device
        .newTextureWithDescriptor(&desc)
        .ok_or("could not create a texture")?;
    let region = MTLRegion {
        origin: MTLOrigin { x: 0, y: 0, z: 0 },
        size: MTLSize {
            width,
            height,
            depth: 1,
        },
    };
    let bytes = NonNull::new(px.as_mut_ptr().cast::<c_void>()).ok_or("no pixels")?;
    // SAFETY: `px` holds exactly width x height BGRA pixels in rows of width*4 bytes (checked
    // above), the texture's size and format; Metal copies them before returning.
    unsafe { texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(region, 0, bytes, width * 4) };
    Ok(texture)
}

/// `inner` (normalised to -1..1 within a rect) placed in `outer` (normalised device
/// coordinates).
fn within(outer: [f32; 4], inner: [f32; 4]) -> [f32; 4] {
    let (cx, cy) = ((outer[0] + outer[2]) / 2.0, (outer[1] + outer[3]) / 2.0);
    let (hw, hh) = ((outer[2] - outer[0]) / 2.0, (outer[3] - outer[1]) / 2.0);
    [
        cx + inner[0] * hw,
        cy + inner[1] * hh,
        cx + inner[2] * hw,
        cy + inner[3] * hh,
    ]
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

    /// Stacked on one target the top stage covers the one below it where it lies; a black
    /// stage on top covers everything below with black.
    #[cfg(feature = "test-support")]
    #[test]
    fn the_gpu_stacks_stages_bottom_first() {
        use crate::card;
        let mut gpu = Gpu::new().unwrap();
        let layer = |n: u128, picture| Layer {
            stage: openlab_core::StageId(uuid::Uuid::from_u128(n)),
            width: 1920,
            height: 1080,
            picture,
            cues: Vec::new(),
        };
        let points = card::barcode_points();
        let read = |gpu: &mut Gpu, layers: &[Layer]| {
            card::read_frame(
                &gpu.probe((1920, 1080), layers, &points, &mut |_| None)
                    .unwrap(),
            )
        };
        let a = layer(1, Picture::TestCard(1));
        let b = layer(2, Picture::TestCard(2));
        assert_eq!(read(&mut gpu, &[a.clone(), b.clone()]), Some(2));
        assert_eq!(read(&mut gpu, &[b, a.clone()]), Some(1));
        assert_eq!(read(&mut gpu, &[a, layer(3, Picture::Black)]), None);
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
        let layer = |picture| Layer {
            stage: openlab_core::StageId(uuid::Uuid::nil()),
            width: 1280,
            height: 720,
            picture,
            cues: Vec::new(),
        };
        let black = gpu
            .probe((1920, 1080), &[layer(Picture::Black)], &quads, &mut |_| {
                None
            })
            .unwrap();
        assert_eq!(black, vec![[0, 0, 0, 255]; 4]);
        let mut points = card::barcode_points().to_vec();
        points.extend_from_slice(&quads);
        let read = gpu
            .probe(
                (1280, 720),
                &[layer(Picture::TestCard(4242))],
                &points,
                &mut |_| None,
            )
            .unwrap();
        assert_eq!(card::read_frame(&read[..card::CELLS]), Some(4242));
        assert_eq!(
            read[card::CELLS..],
            [card::RED, card::GREEN, card::BLUE, card::YELLOW]
        );
    }
}
