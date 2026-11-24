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
    MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLScissorRect, MTLSize, MTLStoreAction,
    MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};
#[cfg(feature = "test-support")]
use objc2_metal::{MTLBlitCommandEncoder, MTLBuffer, MTLResourceOptions, MTLStorageMode};

#[cfg(all(test, feature = "test-support"))]
use openlab_core::VideoGeometry;
use openlab_core::{Quad, quad};

use crate::hap::Variant;
use crate::host::{CuePicture, Layer, Picture, Placement};

/// A texture the stage drawing samples.
pub type Texture = Retained<ProtocolObject<dyn MTLTexture>>;

/// How a texture holds its colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Colour {
    /// BGRA premultiplied by its alpha, or opaque.
    #[default]
    Premultiplied,
    /// Colour alone (HAP's DXT1, an RGB format): drawn opaque, whatever alpha the texture
    /// samples (a DXT1 block in its three-colour mode samples one of its colours as
    /// transparent black).
    Opaque,
    /// Colour and straight alpha (HAP Alpha): premultiplied as it is drawn.
    Straight,
    /// HAP Q's scaled YCoCg: Co, Cg and the block's scale in red, green and blue, luma in
    /// alpha; turned into opaque RGB as it is drawn.
    ScaledYCoCg,
}

impl Colour {
    /// The shader's `mode` for it.
    fn mode(self) -> f32 {
        match self {
            Self::Premultiplied => 0.0,
            Self::Straight => 1.0,
            Self::ScaledYCoCg => 2.0,
            Self::Opaque => 3.0,
        }
    }
}

/// The texture format and colour a HAP flavour's frames are drawn from.
pub fn hap_texture(variant: Variant) -> (MTLPixelFormat, Colour) {
    match variant {
        Variant::Hap => (MTLPixelFormat::BC1_RGBA, Colour::Opaque),
        Variant::HapAlpha => (MTLPixelFormat::BC3_RGBA, Colour::Straight),
        Variant::HapQ => (MTLPixelFormat::BC3_RGBA, Colour::ScaledYCoCg),
    }
}

/// Where a cue's picture comes from when a stage is drawn: its texture, pixel size and how the
/// texture holds its colour at the picture's position, or `None` (not drawn; the source has
/// noted why).
pub type Pictures<'a> = dyn FnMut(&CuePicture) -> Option<(Texture, (u32, u32), Colour)> + 'a;

/// One quad: four corners (top left, top right, bottom left, bottom right; normalised device
/// coordinates) and the texture coordinates at each, sampled smooth or nearest-neighbour. The
/// colour sampled is made premultiplied RGBA first (`mode`, [`Colour`]): straight alpha is
/// multiplied in, colour alone is made opaque, and HAP Q's scaled YCoCg (the format's published
/// conversion) becomes opaque RGB. The whole colour is then scaled by the opacity and blended "over": a pixel the file
/// leaves transparent stays so.
const SHADER: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct Quad { float2 pos[4]; float2 uv[4]; float opacity; float smooth; float mode; float pad; };
struct V { float4 pos [[position]]; float2 uv; };
vertex V quad_vertex(uint vid [[vertex_id]], constant Quad& q [[buffer(0)]]) {
    V o;
    o.pos = float4(q.pos[vid], 0, 1);
    o.uv = q.uv[vid];
    return o;
}
fragment float4 quad_fragment(V in [[stage_in]], texture2d<float> tex [[texture(0)]],
                              constant Quad& q [[buffer(0)]]) {
    constexpr sampler nearest(filter::nearest, address::clamp_to_edge);
    constexpr sampler linear(filter::linear, address::clamp_to_edge);
    float4 c = q.smooth > 0.5 ? tex.sample(linear, in.uv) : tex.sample(nearest, in.uv);
    if (q.mode > 2.5) {
        c.a = 1.0;
    } else if (q.mode > 1.5) {
        float scale = c.b * (255.0 / 8.0) + 1.0;
        float co = (c.r - 128.0 / 255.0) / scale;
        float cg = (c.g - 128.0 / 255.0) / scale;
        float y = c.a;
        c = float4(saturate(float3(y + co - cg, y + cg, y - co - cg)), 1.0);
    } else if (q.mode > 0.5) {
        c.rgb *= c.a;
    }
    return c * q.opacity;
}
"#;

/// The bytes of the shader's `Quad`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct QuadBytes {
    pos: [[f32; 2]; 4],
    uv: [[f32; 2]; 4],
    opacity: f32,
    smooth: f32,
    mode: f32,
    pad: f32,
}

impl QuadBytes {
    /// `quad` (stage pixels from the top left, y down: top left, top right, bottom left, bottom
    /// right) placed on a target of `target` pixels by `place`.
    fn placed(
        quad: &Quad,
        place: &Placement,
        target: (u32, u32),
        opacity: f32,
        smooth: bool,
        colour: Colour,
    ) -> Self {
        let ndc = |point: [f64; 2]| {
            let [x, y] = place.map(target, point);
            [(x * 2.0 - 1.0) as f32, (1.0 - y * 2.0) as f32]
        };
        let [u0, v0, u1, v1] = quad.uv.map(|v| v as f32);
        Self {
            pos: quad.corners.map(ndc),
            uv: [[u0, v0], [u1, v0], [u0, v1], [u1, v1]],
            opacity,
            smooth: if smooth { 1.0 } else { 0.0 },
            mode: colour.mode(),
            pad: 0.0,
        }
    }
}

/// Where `place` puts its region on a target of `target` pixels, in normalised device
/// coordinates (x0, y0, x1, y1; y up), past the target where the scaling overhangs it.
fn dest_ndc(place: &Placement, target: (u32, u32)) -> [f32; 4] {
    let [l, t, r, b] = place.dest(target);
    [
        (l * 2.0 - 1.0) as f32,
        (1.0 - b * 2.0) as f32,
        (r * 2.0 - 1.0) as f32,
        (1.0 - t * 2.0) as f32,
    ]
}

/// The pixels of a target of `size` that `ndc` (normalised device coordinates) covers, as a
/// scissor rect: a region's pictures never draw outside where the region lands.
fn scissor(ndc: [f32; 4], (w, h): (u32, u32)) -> MTLScissorRect {
    let (w, h) = (f64::from(w), f64::from(h));
    let px = |v: f32, side: f64| ((f64::from(v) + 1.0) / 2.0 * side).round().clamp(0.0, side);
    let (x0, x1) = (px(ndc[0], w), px(ndc[2], w));
    // Device y is up; the scissor's rows count down from the top.
    let (y0, y1) = (h - px(ndc[3], h), h - px(ndc[1], h));
    MTLScissorRect {
        x: x0 as usize,
        y: y0 as usize,
        width: (x1 - x0).max(0.0) as usize,
        height: (y1 - y0).max(0.0) as usize,
    }
}

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
        // Premultiplied "over": the shader has scaled the whole colour by the opacity.
        attachment.setBlendingEnabled(true);
        attachment.setSourceRGBBlendFactor(MTLBlendFactor::One);
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

    /// Encodes a clear to black and then `layers` (bottom first) into `target`, each stage's
    /// part placed by the layer's placement (its region filling its route, turned and scaled),
    /// and on each its cues' pictures (bottom first), each placed by its geometry, clipped to
    /// where the region lands, at its opacity.
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
            let place = &layer.place;
            let clip = scissor(dest_ndc(place, size), size);
            let (w, h) = (f64::from(layer.width), f64::from(layer.height));
            let whole = Quad {
                corners: [[0.0, 0.0], [w, 0.0], [0.0, h], [w, h]],
                uv: [0.0, 0.0, 1.0, 1.0],
            };
            quads.push((
                QuadBytes::placed(&whole, place, size, 1.0, false, Colour::Premultiplied),
                self.texture_for(layer.picture)?,
                clip,
            ));
            for cue in &layer.cues {
                let Some((texture, picture, colour)) = pictures(cue) else {
                    continue;
                };
                let g = &cue.geometry;
                let Some(quad) = quad(g, picture, (layer.width, layer.height)) else {
                    continue;
                };
                let opacity = (cue.opacity * g.opacity as f32).clamp(0.0, 1.0);
                quads.push((
                    QuadBytes::placed(&quad, place, size, opacity, g.smooth, colour),
                    texture,
                    clip,
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
        for (quad, texture, clip) in &quads {
            if clip.width == 0 || clip.height == 0 {
                continue;
            }
            let bytes = NonNull::from(quad).cast::<c_void>();
            // SAFETY: `quad` is `QuadBytes`, plain `repr(C)` floats laid out as the shader's
            // `Quad` (80 bytes), alive for the call (Metal copies inline bytes); texture index 0
            // is the shader's `tex`; a strip of 4 vertices is the shader's quad; the scissor
            // rect lies within the target (`scissor` clamps it).
            unsafe {
                enc.setScissorRect(*clip);
                enc.setVertexBytes_length_atIndex(bytes, size_of::<QuadBytes>(), 0);
                enc.setFragmentBytes_length_atIndex(bytes, size_of::<QuadBytes>(), 0);
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

/// The bytes of one row of `width` pixels (of 4 x 4 blocks, for a block-compressed format)
/// and the rows in `height`, in `format`.
fn rows_of(format: MTLPixelFormat, width: usize, height: usize) -> (usize, usize) {
    let blocks = |block_bytes: usize| (width.div_ceil(4) * block_bytes, height.div_ceil(4));
    if format == MTLPixelFormat::BC1_RGBA {
        blocks(8)
    } else if format == MTLPixelFormat::BC3_RGBA {
        blocks(16)
    } else {
        (width * 4, height)
    }
}

/// Replaces `texture`'s pixels (of `size`) with `px`, rows of `stride` bytes (rows of 4 x 4
/// blocks for a block-compressed texture), in the texture's own format.
pub fn replace(
    texture: &Texture,
    size: (u32, u32),
    px: &mut [u8],
    stride: usize,
) -> Result<(), String> {
    let (w, h) = (size.0 as usize, size.1 as usize);
    let (row, rows) = rows_of(texture.pixelFormat(), w, h);
    let fits = stride
        .checked_mul(rows)
        .is_some_and(|need| px.len() >= need);
    if texture.width() != w || texture.height() != h || stride < row || !fits {
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
    // SAFETY: `px` holds at least `rows` rows of `stride` bytes, each starting with a row of
    // the texture's format at its width (checked above: w BGRA pixels, or w / 4 blocks rounded
    // up), the texture's size; Metal copies them before returning. The region is the whole
    // texture, so a block-compressed one's last blocks may run past its edge.
    unsafe { texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(region, 0, bytes, stride) };
    Ok(())
}

/// A shader-readable texture of `width` x `height` in `format` (BGRA, or a HAP frame's
/// block-compressed one), its pixels not yet written.
pub fn empty_texture(
    device: &ProtocolObject<dyn MTLDevice>,
    width: usize,
    height: usize,
    format: MTLPixelFormat,
) -> Result<Texture, String> {
    if width == 0 || height == 0 {
        return Err("a texture of no size".into());
    }
    let compressed = format == MTLPixelFormat::BC1_RGBA || format == MTLPixelFormat::BC3_RGBA;
    if compressed && !device.supportsBCTextureCompression() {
        return Err("this computer's graphics cannot draw HAP movies".into());
    }
    // SAFETY: a plain 2D descriptor; the sizes were checked against the picture limits.
    let desc = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            format, width, height, false,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_stage_fits_its_target_keeping_its_shape() {
        let fit = |stage: (u32, u32), target| dest_ndc(&Placement::whole(stage.0, stage.1), target);
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
            place: Placement::whole(1920, 1080),
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

    /// On this machine's GPU: a region draws only its part of the stage, filling where its
    /// route lands; a quarter turn is clockwise (the card's red top left lands top right); fit
    /// leaves black where the route does not reach.
    #[cfg(feature = "test-support")]
    #[test]
    fn a_region_draws_its_part_turned_and_scaled_by_its_route() {
        use crate::card::{BLUE, GREEN, RED, YELLOW};
        let mut gpu = Gpu::new().unwrap();
        let corners = [[0.1, 0.4], [0.9, 0.4], [0.1, 0.9], [0.9, 0.9]];
        let draw = |gpu: &mut Gpu, place: Placement, target: (u32, u32)| {
            let layer = Layer {
                stage: openlab_core::StageId(uuid::Uuid::nil()),
                width: 1920,
                height: 1080,
                place,
                picture: Picture::TestCard(1),
                cues: Vec::new(),
            };
            gpu.probe(target, &[layer], &corners, &mut |_| None)
                .unwrap()
        };
        let place = |region, route, rotation, scaling| Placement {
            region,
            route,
            rotation,
            scaling,
            screen: None,
        };
        // The stage's left half alone, filling a route of its own shape: red above, blue below.
        let left = draw(
            &mut gpu,
            place(
                [0, 0, 960, 1080],
                (960, 1080),
                0,
                openlab_core::RouteScaling::Fill,
            ),
            (960, 1080),
        );
        assert_eq!(left, [RED, RED, BLUE, BLUE]);
        // The whole stage turned a quarter clockwise on a portrait target.
        let turned = draw(
            &mut gpu,
            place(
                [0, 0, 1920, 1080],
                (1920, 1080),
                90,
                openlab_core::RouteScaling::Fill,
            ),
            (1080, 1920),
        );
        let probe = |gpu: &mut Gpu, points: &[[f32; 2]]| {
            let layer = Layer {
                stage: openlab_core::StageId(uuid::Uuid::nil()),
                width: 1920,
                height: 1080,
                place: place(
                    [0, 0, 1920, 1080],
                    (1920, 1080),
                    90,
                    openlab_core::RouteScaling::Fill,
                ),
                picture: Picture::TestCard(1),
                cues: Vec::new(),
            };
            gpu.probe((1080, 1920), &[layer], points, &mut |_| None)
                .unwrap()
        };
        assert_eq!(turned.len(), 4);
        // Below the barcode band (now down the right edge): top left blue, top right red,
        // bottom left yellow, bottom right green.
        assert_eq!(
            probe(
                &mut gpu,
                &[[0.3, 0.25], [0.6, 0.25], [0.3, 0.75], [0.6, 0.75]]
            ),
            [BLUE, RED, YELLOW, GREEN]
        );
        // Fit on a square target: black above and below the route.
        let fit = draw(
            &mut gpu,
            place(
                [0, 0, 1920, 1080],
                (1920, 1080),
                0,
                openlab_core::RouteScaling::Fit,
            ),
            (1000, 1000),
        );
        assert_eq!(fit[0], RED);
        let edges = {
            let layer = Layer {
                stage: openlab_core::StageId(uuid::Uuid::nil()),
                width: 1920,
                height: 1080,
                place: place(
                    [0, 0, 1920, 1080],
                    (1920, 1080),
                    0,
                    openlab_core::RouteScaling::Fit,
                ),
                picture: Picture::TestCard(1),
                cues: Vec::new(),
            };
            gpu.probe(
                (1000, 1000),
                &[layer],
                &[[0.5, 0.05], [0.5, 0.95]],
                &mut |_| None,
            )
            .unwrap()
        };
        assert_eq!(edges, [[0, 0, 0, 255], [0, 0, 0, 255]]);
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
            place: Placement::whole(1280, 720),
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

    /// Draws stage-sized pictures over black on a stage of `stage` in a target of `target`,
    /// and reads `points` (fractions of the target).
    #[cfg(feature = "test-support")]
    fn draw_pictures(
        stage: (u32, u32),
        target: (u32, u32),
        cues: &[(Vec<u8>, (u32, u32), VideoGeometry)],
        points: &[[f32; 2]],
    ) -> Vec<[u8; 4]> {
        let mut gpu = Gpu::new().unwrap();
        let textures: Vec<Texture> = cues
            .iter()
            .map(|(px, (w, h), _)| {
                texture(gpu.device(), *w as usize, *h as usize, &mut px.clone()).unwrap()
            })
            .collect();
        let layer = Layer {
            stage: openlab_core::StageId(uuid::Uuid::nil()),
            width: stage.0,
            height: stage.1,
            place: Placement::whole(stage.0, stage.1),
            picture: Picture::Black,
            cues: cues
                .iter()
                .enumerate()
                .map(|(i, (_, _, geometry))| CuePicture {
                    picture: i as u64,
                    position: 0.0,
                    left: f64::INFINITY,
                    next: None,
                    held: false,
                    last: false,
                    pass: 0,
                    opacity: 1.0,
                    geometry: *geometry,
                })
                .collect(),
        };
        gpu.probe(target, &[layer], points, &mut |c| {
            let i = c.picture as usize;
            Some((textures[i].clone(), cues[i].1, Colour::Premultiplied))
        })
        .unwrap()
    }

    /// BGRA pixels, premultiplied, from RGBA.
    #[cfg(feature = "test-support")]
    fn bgra(rgba: &[[u8; 4]]) -> Vec<u8> {
        rgba.iter()
            .flat_map(|[r, g, b, a]| [*b, *g, *r, *a])
            .collect()
    }

    /// A pixel the file leaves transparent stays transparent at full opacity (what is below
    /// shows through); at half opacity an opaque pixel is half over what is below (VID-04).
    #[cfg(feature = "test-support")]
    #[test]
    fn transparent_pixels_stay_transparent_and_opacity_blends_over_what_is_below() {
        let green = (bgra(&[[0, 255, 0, 255]]), (1, 1), VideoGeometry::default());
        let stretch = VideoGeometry {
            fill_style: openlab_core::FillStyle::Stretch,
            smooth: false,
            ..VideoGeometry::default()
        };
        let half_clear = bgra(&[[255, 0, 0, 255], [0, 0, 0, 0]]);
        let points = [[0.25, 0.5], [0.75, 0.5]];
        let read = draw_pictures(
            (64, 64),
            (64, 64),
            &[green.clone(), (half_clear.clone(), (2, 1), stretch)],
            &points,
        );
        assert_eq!(read, vec![[255, 0, 0, 255], [0, 255, 0, 255]]);
        let half = VideoGeometry {
            opacity: 0.5,
            ..stretch
        };
        let read = draw_pictures(
            (64, 64),
            (64, 64),
            &[green, (half_clear, (2, 1), half)],
            &points,
        );
        let [r, g, _, _] = read[0];
        assert!(
            (126..=129).contains(&r) && (126..=129).contains(&g),
            "{read:?}"
        );
        assert_eq!(read[1], [0, 255, 0, 255]);
    }

    /// Smoothing off: a scaled-up picture keeps hard pixel edges; on, the edge is blended.
    #[cfg(feature = "test-support")]
    #[test]
    fn smooth_off_keeps_hard_pixel_edges() {
        let red_blue = bgra(&[[255, 0, 0, 255], [0, 0, 255, 255]]);
        let g = |smooth| VideoGeometry {
            fill_style: openlab_core::FillStyle::Stretch,
            smooth,
            ..VideoGeometry::default()
        };
        let near_middle = [[0.45, 0.5]];
        let hard = draw_pictures(
            (64, 64),
            (64, 64),
            &[(red_blue.clone(), (2, 1), g(false))],
            &near_middle,
        );
        assert_eq!(hard, vec![[255, 0, 0, 255]]);
        let soft = draw_pictures(
            (64, 64),
            (64, 64),
            &[(red_blue, (2, 1), g(true))],
            &near_middle,
        );
        let [r, _, b, _] = soft[0];
        assert!(r < 255 && b > 0, "{soft:?}");
    }

    /// A picture that overflows its stage (fill, or moved off it) is not drawn outside the
    /// stage's part of the display.
    #[cfg(feature = "test-support")]
    #[test]
    fn a_picture_never_draws_outside_its_stage() {
        let wide_red = bgra(&[[255, 0, 0, 255]; 4]);
        let fill = VideoGeometry {
            fill_style: openlab_core::FillStyle::Fill,
            ..VideoGeometry::default()
        };
        // A square stage in a wide display: bars at the sides stay black.
        let read = draw_pictures(
            (100, 100),
            (200, 100),
            &[(wide_red, (4, 1), fill)],
            &[[0.1, 0.5], [0.5, 0.5], [0.9, 0.5]],
        );
        assert_eq!(read, vec![[0, 0, 0, 255], [255, 0, 0, 255], [0, 0, 0, 255]]);
    }

    /// Draws one picture of `size` from `texture` (holding `colour`) stretched over a 64 x 64
    /// stage, nearest-neighbour, and reads `points`.
    #[cfg(feature = "test-support")]
    fn draw_texture(
        gpu: &mut Gpu,
        texture: &Texture,
        size: (u32, u32),
        colour: Colour,
        points: &[[f32; 2]],
    ) -> Vec<[u8; 4]> {
        let layer = Layer {
            stage: openlab_core::StageId(uuid::Uuid::nil()),
            width: 64,
            height: 64,
            place: Placement::whole(64, 64),
            picture: Picture::Black,
            cues: vec![CuePicture {
                picture: 0,
                position: 0.0,
                left: f64::INFINITY,
                next: None,
                held: false,
                last: false,
                pass: 0,
                opacity: 1.0,
                geometry: VideoGeometry {
                    fill_style: openlab_core::FillStyle::Stretch,
                    smooth: false,
                    ..VideoGeometry::default()
                },
            }],
        };
        gpu.probe((64, 64), &[layer], points, &mut |_| {
            Some((texture.clone(), size, colour))
        })
        .unwrap()
    }

    /// HAP, HAP Alpha and HAP Q textures (made by the test writer) drawn as the stage draws a
    /// movie's frame: each quadrant's colour, HAP Alpha's half alpha blended once over black
    /// (premultiplied as it is drawn), HAP Q's YCoCg turned back into RGB, HAP's and HAP Q's
    /// alpha ignored (opaque; HAP's three-colour-mode blocks:
    /// `a_hap_dxt1_block_in_three_colour_mode_draws_opaque_black`). A size that is not a whole
    /// number of blocks draws too.
    #[cfg(feature = "test-support")]
    #[test]
    fn hap_textures_draw_their_colours() {
        use crate::hap::write;
        let mut gpu = Gpu::new().unwrap();
        // A GPU without BC textures fails this test (as every gpu.rs test fails without Metal):
        // HAP could not be drawn on such a machine, and the test must not pass there.
        assert!(
            gpu.device().supportsBCTextureCompression(),
            "this GPU cannot sample BC textures: HAP movies cannot be drawn here"
        );
        let (w, h) = (8u32, 8u32);
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                rgba.extend(match (x >= w / 2, y >= h / 2) {
                    (false, false) => [255, 0, 0, 255],
                    (true, false) => [0, 255, 0, 255],
                    (false, true) => [0, 0, 255, 255],
                    (true, true) => [255, 255, 0, 128],
                });
            }
        }
        let points = [[0.25, 0.25], [0.75, 0.25], [0.25, 0.75], [0.75, 0.75]];
        let near = |got: &[[u8; 4]], want: &[[u8; 4]], by: u8| {
            got.iter()
                .zip(want)
                .all(|(g, w)| g.iter().zip(w).all(|(a, b)| a.abs_diff(*b) <= by))
        };
        let opaque = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 0, 255],
        ];
        let half = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [128, 128, 0, 255],
        ];
        for (variant, want, by) in [
            (Variant::Hap, opaque, 0),
            (Variant::HapAlpha, half, 2),
            (Variant::HapQ, opaque, 6),
        ] {
            let (format, colour) = hap_texture(variant);
            let t = empty_texture(gpu.device(), w as usize, h as usize, format).unwrap();
            let mut px = write::texture(variant, w, h, &rgba);
            replace(&t, (w, h), &mut px, variant.row_bytes(w)).unwrap();
            let read = draw_texture(&mut gpu, &t, (w, h), colour, &points);
            assert!(near(&read, &want, by), "{variant:?}: {read:?}");
        }
        // 6 x 5: the last row and column of blocks run past the picture's edge.
        let green: Vec<u8> = [0u8, 255, 0, 255].repeat(30);
        for variant in [Variant::Hap, Variant::HapAlpha, Variant::HapQ] {
            let (format, colour) = hap_texture(variant);
            let t = empty_texture(gpu.device(), 6, 5, format).unwrap();
            let mut px = write::texture(variant, 6, 5, &green);
            replace(&t, (6, 5), &mut px, variant.row_bytes(6)).unwrap();
            let read = draw_texture(&mut gpu, &t, (6, 5), colour, &[[0.9, 0.9]]);
            assert!(near(&read, &[[0, 255, 0, 255]], 6), "{variant:?}: {read:?}");
            // Too few bytes for the texture: refused, never read past.
            assert!(replace(&t, (6, 5), &mut px[..8], variant.row_bytes(6)).is_err());
        }
    }

    /// A HAP (DXT1) block in its three-colour mode (endpoint 0 not above endpoint 1) whose
    /// texels all take index 3 samples as transparent black; HAP is an RGB format, so it is
    /// drawn opaque black over a red picture beneath, never letting the red through.
    #[cfg(feature = "test-support")]
    #[test]
    fn a_hap_dxt1_block_in_three_colour_mode_draws_opaque_black() {
        let mut gpu = Gpu::new().unwrap();
        let red = texture(gpu.device(), 1, 1, &mut [0, 0, 255, 255]).unwrap();
        let (format, colour) = hap_texture(Variant::Hap);
        let hap = empty_texture(gpu.device(), 4, 4, format).unwrap();
        // Endpoints white then black (0xFFFF > 0x0000 would be four-colour mode: reversed
        // here), every index 3.
        let mut block = [0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        replace(&hap, (4, 4), &mut block, Variant::Hap.row_bytes(4)).unwrap();
        let cue = |picture| CuePicture {
            picture,
            position: 0.0,
            left: f64::INFINITY,
            next: None,
            held: false,
            last: false,
            pass: 0,
            opacity: 1.0,
            geometry: VideoGeometry {
                fill_style: openlab_core::FillStyle::Stretch,
                smooth: false,
                ..VideoGeometry::default()
            },
        };
        let layer = Layer {
            stage: openlab_core::StageId(uuid::Uuid::nil()),
            width: 64,
            height: 64,
            place: Placement::whole(64, 64),
            picture: Picture::Black,
            cues: vec![cue(0), cue(1)],
        };
        let read = gpu
            .probe((64, 64), &[layer], &[[0.5, 0.5]], &mut |c| {
                Some(if c.picture == 0 {
                    (red.clone(), (1, 1), Colour::Premultiplied)
                } else {
                    (hap.clone(), (4, 4), colour)
                })
            })
            .unwrap();
        assert_eq!(read, vec![[0, 0, 0, 255]]);
    }
}
