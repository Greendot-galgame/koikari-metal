//! Metal renderer: one textured-quad pipeline plus a texture cache.
//!
//! Everything the game shows — backgrounds, character layers, UI panels and
//! text bitmaps — is a quad. Keeping a single pipeline makes frame cost
//! predictable and lets the CPU-side draw list stay declarative.

use std::ffi::c_void;

#[path = "texture_cache.rs"]
mod texture_cache;
#[path = "ui.rs"]
mod ui;

use texture_cache::TextureCache;
pub use ui::{
    Viewport, choice_at, choice_rect, contains, message_text_left, show_dialogue, title_menu_rect,
};

use anyhow::{Result, anyhow};
use metal::{
    Buffer, CommandQueue, CompileOptions, Device, MTLBlendFactor, MTLPixelFormat, MTLRegion,
    MTLResourceOptions, MTLSamplerAddressMode, MTLSamplerMinMagFilter, MTLTextureType,
    MTLTextureUsage, RenderPassDescriptor, RenderPipelineDescriptor, RenderPipelineState,
    SamplerDescriptor, SamplerState, Texture, TextureDescriptor,
};

const SHADER_SOURCE: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct VertexIn {
    float2 position [[attribute(0)]];
    float2 uv [[attribute(1)]];
};

struct VertexOut {
    float4 position [[position]];
    float2 uv;
};

vertex VertexOut vs_main(uint vid [[vertex_id]],
                         const device VertexIn* verts [[buffer(0)]]) {
    VertexOut out;
    out.position = float4(verts[vid].position, 0.0, 1.0);
    out.uv = verts[vid].uv;
    return out;
}

fragment float4 fs_main(VertexOut in [[stage_in]],
                        texture2d<float> tex [[texture(0)]],
                        sampler samp [[sampler(0)]],
                        constant float& opacity [[buffer(0)]]) {
    float4 c = tex.sample(samp, in.uv);
    return c * opacity;
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Vertex {
    position: [f32; 2],
    uv: [f32; 2],
}

#[derive(Clone)]
struct QueuedQuad {
    texture: String,
    dst: [f32; 4],
    uv: [f32; 4],
    opacity: f32,
}

struct CachedTexture {
    texture: Texture,
    width: u32,
    height: u32,
}

/// Bytes available before eviction. A frame's live working set may exceed it.
pub const TEXTURE_BUDGET: usize = 768 * 1024 * 1024;

/// Maximum quads in one frame; protects against runaway draw lists.
const MAX_QUADS: usize = 8192;

pub struct Renderer {
    device: Device,
    queue: CommandQueue,
    pipeline: RenderPipelineState,
    sampler: SamplerState,
    vertex_buffer: Buffer,
    vertex_capacity: usize,
    textures: TextureCache<CachedTexture>,
    quads: Vec<QueuedQuad>,
    viewport: Viewport,
    white: Texture,
}

impl Renderer {
    /// Build the pipeline against the system Metal device.
    pub fn new() -> Result<Self> {
        Self::with_texture_budget(TEXTURE_BUDGET)
    }

    /// A smaller budget is useful for deterministic eviction regression runs.
    pub fn with_texture_budget(texture_budget: usize) -> Result<Self> {
        let device =
            Device::system_default().ok_or_else(|| anyhow!("no Metal device available"))?;
        let queue = device.new_command_queue();
        let library = device
            .new_library_with_source(SHADER_SOURCE, &CompileOptions::new())
            .map_err(|e| anyhow!("Metal shader compilation failed: {e}"))?;
        let vertex_fn = library
            .get_function("vs_main", None)
            .map_err(|e| anyhow!("vs_main missing: {e}"))?;
        let fragment_fn = library
            .get_function("fs_main", None)
            .map_err(|e| anyhow!("fs_main missing: {e}"))?;

        let descriptor = RenderPipelineDescriptor::new();
        descriptor.set_vertex_function(Some(&vertex_fn));
        descriptor.set_fragment_function(Some(&fragment_fn));
        if let Some(attachment) = descriptor.color_attachments().object_at(0) {
            attachment.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
            attachment.set_blending_enabled(true);
            attachment.set_source_rgb_blend_factor(MTLBlendFactor::One);
            attachment.set_destination_rgb_blend_factor(MTLBlendFactor::OneMinusSourceAlpha);
            attachment.set_source_alpha_blend_factor(MTLBlendFactor::One);
            attachment.set_destination_alpha_blend_factor(MTLBlendFactor::OneMinusSourceAlpha);
        }
        let pipeline = device
            .new_render_pipeline_state(&descriptor)
            .map_err(|e| anyhow!("render pipeline state creation failed: {e}"))?;

        let sampler_descriptor = SamplerDescriptor::new();
        sampler_descriptor.set_min_filter(MTLSamplerMinMagFilter::Linear);
        sampler_descriptor.set_mag_filter(MTLSamplerMinMagFilter::Linear);
        sampler_descriptor.set_address_mode_s(MTLSamplerAddressMode::ClampToEdge);
        sampler_descriptor.set_address_mode_t(MTLSamplerAddressMode::ClampToEdge);
        let sampler = device.new_sampler(&sampler_descriptor);

        let vertex_capacity = MAX_QUADS * 6;
        let vertex_buffer = device.new_buffer(
            (vertex_capacity * std::mem::size_of::<Vertex>()) as u64,
            MTLResourceOptions::StorageModeManaged,
        );

        let white = make_texture(&device, 1, 1, &[255, 255, 255, 255])?;

        Ok(Self {
            device,
            queue,
            pipeline,
            sampler,
            vertex_buffer,
            vertex_capacity,
            textures: TextureCache::new(texture_budget),
            quads: Vec::new(),
            viewport: Viewport::new(
                (crate::CANVAS_WIDTH, crate::CANVAS_HEIGHT),
                (crate::CANVAS_WIDTH, crate::CANVAS_HEIGHT),
            ),
            white,
        })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Update the letterbox transform for the current drawable size.
    pub fn set_surface_size(&mut self, width: f32, height: f32) {
        self.viewport = Viewport::new(
            (crate::CANVAS_WIDTH, crate::CANVAS_HEIGHT),
            (width.max(1.0), height.max(1.0)),
        );
    }

    /// Map canvas coordinates to the window, exposed for hit-testing.
    pub fn canvas_to_window(&self, x: f32, y: f32) -> (f32, f32) {
        self.viewport.canvas_to_surface(x, y)
    }

    pub fn has_texture(&self, key: &str) -> bool {
        self.textures.contains_key(key)
    }

    /// Insert or replace an RGBA8 texture. `rgba` must be `width * height * 4`.
    pub fn upload_rgba(&mut self, key: &str, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
        if width == 0 || height == 0 {
            return Err(anyhow!("texture '{key}' has a zero dimension"));
        }
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| anyhow!("texture '{key}' size overflows address space"))?;
        if rgba.len() < expected {
            return Err(anyhow!(
                "texture '{key}' needs {expected} bytes, received {}",
                rgba.len()
            ));
        }
        let texture = make_texture(&self.device, width, height, rgba)?;
        self.textures.insert(
            key.to_string(),
            CachedTexture {
                texture,
                width,
                height,
            },
            expected,
        );
        Ok(())
    }

    /// Size of a cached texture, if present.
    pub fn texture_size(&self, key: &str) -> Option<(u32, u32)> {
        self.textures.get(key).map(|t| (t.width, t.height))
    }

    /// Start a frame: clears the draw list and sets the letterbox transform.
    pub fn begin_frame(&mut self, surface_width: f32, surface_height: f32) {
        self.quads.clear();
        self.textures.begin_frame();
        self.set_surface_size(surface_width, surface_height);
    }

    /// Queue a solid or textured axis-aligned quad in canvas pixel space.
    pub fn draw(&mut self, texture: Option<&str>, dst: [f32; 4], opacity: f32) {
        if opacity <= 0.0 || dst[2] <= 0.0 || dst[3] <= 0.0 {
            return;
        }
        if self.quads.len() >= MAX_QUADS {
            return;
        }
        let key = match texture {
            Some(name) if self.textures.pin(name) => name.to_string(),
            Some(_) => return,
            None => "__white".to_string(),
        };
        self.quads.push(QueuedQuad {
            texture: key,
            dst,
            uv: [0.0, 0.0, 1.0, 1.0],
            opacity: opacity.min(1.0),
        });
    }

    /// Queue a quad using an explicit UV sub-rectangle (for sprite sheets).
    pub fn draw_uv(&mut self, texture: &str, dst: [f32; 4], uv: [f32; 4], opacity: f32) {
        if opacity <= 0.0 || dst[2] <= 0.0 || dst[3] <= 0.0 {
            return;
        }
        if self.quads.len() >= MAX_QUADS || !self.textures.pin(texture) {
            return;
        }
        self.quads.push(QueuedQuad {
            texture: texture.to_string(),
            dst,
            uv,
            opacity: opacity.min(1.0),
        });
    }

    /// Encode and submit the current draw list to a window drawable.
    pub fn present(&mut self, drawable: &metal::MetalDrawableRef, clear: [f64; 4]) -> Result<()> {
        self.present_inner(drawable, clear, None)
    }

    /// Present normally, then read the drawable back into a PNG. Used to verify
    /// the real window path (not only the offscreen one).
    pub fn present_capture(
        &mut self,
        drawable: &metal::MetalDrawableRef,
        clear: [f64; 4],
        path: &std::path::Path,
    ) -> Result<()> {
        self.present_inner(drawable, clear, Some(path))
    }

    fn present_inner(
        &mut self,
        drawable: &metal::MetalDrawableRef,
        clear: [f64; 4],
        capture: Option<&std::path::Path>,
    ) -> Result<()> {
        let ranges = self.build_vertices();
        let pass = RenderPassDescriptor::new();
        if let Some(attachment) = pass.color_attachments().object_at(0) {
            attachment.set_texture(Some(drawable.texture()));
            attachment.set_load_action(metal::MTLLoadAction::Clear);
            attachment.set_clear_color(metal::MTLClearColor::new(
                clear[0], clear[1], clear[2], clear[3],
            ));
            attachment.set_store_action(metal::MTLStoreAction::Store);
        }
        let command_buffer = self.queue.new_command_buffer();
        let encoder = command_buffer.new_render_command_encoder(&pass);
        self.draw_ranges(&encoder, &ranges);
        encoder.end_encoding();
        command_buffer.present_drawable(drawable);
        command_buffer.commit();
        // This renderer owns one CPU-written vertex buffer. Do not overwrite it
        // on the next frame while an earlier GPU command is still reading it.
        command_buffer.wait_until_completed();
        if let Some(path) = capture {
            let texture = drawable.texture();
            let width = texture.width() as u32;
            let height = texture.height() as u32;
            let stride = width as usize * 4;
            let mut bgra = vec![0u8; stride * height as usize];
            texture.get_bytes(
                bgra.as_mut_ptr() as *mut c_void,
                stride as u64,
                MTLRegion::new_3d(0, 0, 0, width as u64, height as u64, 1),
                0,
            );
            let mut rgba = vec![0u8; bgra.len()];
            for (src, dst) in bgra.chunks_exact(4).zip(rgba.chunks_exact_mut(4)) {
                dst[0] = src[2];
                dst[1] = src[1];
                dst[2] = src[0];
                dst[3] = src[3];
            }
            image::save_buffer(path, &rgba, width, height, image::ExtendedColorType::Rgba8)?;
            println!("window frame captured -> {}", path.display());
        }
        Ok(())
    }

    /// Render the current draw list into an offscreen texture and return RGBA8
    /// pixels in top-to-bottom row order.
    ///
    /// This is the same encode path the window uses, which makes it a truthful
    /// way to verify a frame without a window (screenshots and tests).
    pub fn render_offscreen(
        &mut self,
        width: u32,
        height: u32,
        clear: [f64; 4],
    ) -> Result<Vec<u8>> {
        // Offline replay has no AppKit event-loop autorelease pool. Releasing
        // completed command buffers here also releases their retained targets,
        // so hundreds of replay frames do not retain hundreds of GPU textures.
        objc::rc::autoreleasepool(|| self.render_offscreen_inner(width, height, clear))
    }

    fn render_offscreen_inner(
        &mut self,
        width: u32,
        height: u32,
        clear: [f64; 4],
    ) -> Result<Vec<u8>> {
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err(anyhow!("offscreen size {width}x{height} is not supported"));
        }
        self.set_surface_size(width as f32, height as f32);
        let ranges = self.build_vertices();
        let descriptor = TextureDescriptor::new();
        descriptor.set_texture_type(MTLTextureType::D2);
        descriptor.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        descriptor.set_width(width as u64);
        descriptor.set_height(height as u64);
        descriptor.set_usage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
        descriptor.set_storage_mode(metal::MTLStorageMode::Shared);
        let target = self.device.new_texture(&descriptor);
        let pass = RenderPassDescriptor::new();
        if let Some(attachment) = pass.color_attachments().object_at(0) {
            attachment.set_texture(Some(&target));
            attachment.set_load_action(metal::MTLLoadAction::Clear);
            attachment.set_clear_color(metal::MTLClearColor::new(
                clear[0], clear[1], clear[2], clear[3],
            ));
            attachment.set_store_action(metal::MTLStoreAction::Store);
        }
        let command_buffer = self.queue.new_command_buffer();
        let encoder = command_buffer.new_render_command_encoder(&pass);
        self.draw_ranges(&encoder, &ranges);
        encoder.end_encoding();
        command_buffer.commit();
        command_buffer.wait_until_completed();

        let stride = width as usize * 4;
        let mut bgra = vec![0u8; stride * height as usize];
        target.get_bytes(
            bgra.as_mut_ptr() as *mut c_void,
            stride as u64,
            MTLRegion::new_3d(0, 0, 0, width as u64, height as u64, 1),
            0,
        );
        // Metal stores BGRA8; convert to the RGBA order `image` expects.
        let mut rgba = vec![0u8; bgra.len()];
        for (src, dst) in bgra.chunks_exact(4).zip(rgba.chunks_exact_mut(4)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = src[3];
        }
        Ok(rgba)
    }

    /// Upload the pending quads and return per-draw ranges.
    fn build_vertices(&mut self) -> Vec<(std::ops::Range<usize>, String, f32)> {
        let mut vertices: Vec<Vertex> = Vec::with_capacity(self.quads.len() * 6);
        let mut ranges: Vec<(std::ops::Range<usize>, String, f32)> = Vec::new();
        for quad in &self.quads {
            let [x, y, w, h] = quad.dst;
            let [u0, v0, u1, v1] = quad.uv;
            let tl = self.viewport.ndc(x, y);
            let tr = self.viewport.ndc(x + w, y);
            let br = self.viewport.ndc(x + w, y + h);
            let bl = self.viewport.ndc(x, y + h);
            let start = vertices.len();
            let corners = [
                (tl, [u0, v0]),
                (br, [u1, v1]),
                (tr, [u1, v0]),
                (tl, [u0, v0]),
                (bl, [u0, v1]),
                (br, [u1, v1]),
            ];
            for (position, uv) in corners {
                vertices.push(Vertex { position, uv });
            }
            ranges.push((start..vertices.len(), quad.texture.clone(), quad.opacity));
        }
        if vertices.is_empty() {
            return ranges;
        }
        if vertices.len() > self.vertex_capacity {
            vertices.truncate(self.vertex_capacity);
            ranges.retain(|(range, _, _)| range.end <= self.vertex_capacity);
        }
        // SAFETY: the buffer is allocated for `vertex_capacity` vertices and the
        // copy length is bounded by that capacity.
        unsafe {
            let destination = self.vertex_buffer.contents() as *mut Vertex;
            std::ptr::copy_nonoverlapping(vertices.as_ptr(), destination, vertices.len());
        }
        self.vertex_buffer.did_modify_range(metal::NSRange::new(
            0,
            (vertices.len() * std::mem::size_of::<Vertex>()) as u64,
        ));
        ranges
    }

    fn draw_ranges(
        &self,
        encoder: &metal::RenderCommandEncoderRef,
        ranges: &[(std::ops::Range<usize>, String, f32)],
    ) {
        encoder.set_render_pipeline_state(&self.pipeline);
        encoder.set_vertex_buffer(0, Some(&self.vertex_buffer), 0);
        encoder.set_fragment_sampler_state(0, Some(&self.sampler));
        for (range, texture_key, opacity) in ranges {
            let texture = if texture_key == "__white" {
                &self.white
            } else {
                match self.textures.get(texture_key) {
                    Some(entry) => &entry.texture,
                    None => continue,
                }
            };
            encoder.set_fragment_texture(0, Some(texture));
            encoder.set_fragment_bytes(0, 4, opacity as *const f32 as *const c_void);
            encoder.draw_primitives(
                metal::MTLPrimitiveType::Triangle,
                range.start as u64,
                (range.end - range.start) as u64,
            );
        }
    }
}

fn make_texture(device: &Device, width: u32, height: u32, rgba: &[u8]) -> Result<Texture> {
    let descriptor = TextureDescriptor::new();
    descriptor.set_texture_type(MTLTextureType::D2);
    descriptor.set_pixel_format(MTLPixelFormat::RGBA8Unorm);
    descriptor.set_width(width as u64);
    descriptor.set_height(height as u64);
    descriptor.set_usage(MTLTextureUsage::ShaderRead);
    let texture = device.new_texture(&descriptor);
    if texture.width() != width as u64 || texture.height() != height as u64 {
        return Err(anyhow!("Metal refused a {width}x{height} texture"));
    }
    // Metal requires the source rows of `replace_region` to be 256-byte aligned
    // on macOS. A 934 px wide sprite is 3736 bytes per row, so uploading the
    // tightly packed pixels directly shears the image; pad to the alignment.
    let tight = width as usize * 4;
    let stride = (tight + 255) & !255;
    if stride == tight {
        texture.replace_region(
            MTLRegion::new_3d(0, 0, 0, width as u64, height as u64, 1),
            0,
            rgba.as_ptr() as *const c_void,
            tight as u64,
        );
    } else {
        let mut padded = vec![0u8; stride * height as usize];
        for row in 0..height as usize {
            let source = &rgba[row * tight..(row + 1) * tight];
            padded[row * stride..row * stride + tight].copy_from_slice(source);
        }
        texture.replace_region(
            MTLRegion::new_3d(0, 0, 0, width as u64, height as u64, 1),
            0,
            padded.as_ptr() as *const c_void,
            stride as u64,
        );
    }
    Ok(texture)
}
