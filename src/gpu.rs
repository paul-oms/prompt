//! Optional GPU renderer.
//!
//! Prompt historically painted into a tiny-skia pixmap and presented it with
//! softbuffer. That is fine for a modest window, but every frame copies the
//! whole window, and large grids on 4K/120 Hz displays become CPU bound.
//!
//! This module paints the same primitives (solid and rounded rectangles, and
//! text) directly on the GPU with wgpu. The CPU renderer stays as a fallback so
//! platforms without a usable adapter, screenshots, and tests keep working.
//!
//! The painter emits one interleaved instance list so rectangle and glyph
//! order is preserved exactly. Glyphs live in a shared R8/RGBA atlas built
//! from cosmic-text's swash cache.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use bytemuck::Zeroable;
use cosmic_text::{CacheKey, SwashContent, SwashImage};
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::theme::Rgb;

const ATLAS_SIZE: u32 = 2048;

/// One draw item. Rectangles and glyphs share the layout so a single draw call
/// preserves painting order.
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct Instance {
    /// x, y, width, height in physical pixels.
    pub rect: [f32; 4],
    /// Straight sRGB colour with alpha in 0..=1.
    pub color: [f32; 4],
    /// radius, stroke width, mode (0 = rect, 1 = glyph), colour glyph flag.
    pub params: [f32; 4],
    /// x, y, width, height; width/height <= 0 means no clip.
    pub clip: [f32; 4],
    /// Texture coordinates (glyph only).
    pub uv: [f32; 4],
}

const MODE_RECT: f32 = 0.0;
const MODE_GLYPH: f32 = 1.0;

#[allow(clippy::too_many_arguments)]
pub fn rect(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    stroke: f32,
    color: Rgb,
    alpha: f32,
) -> Instance {
    Instance {
        rect: [x, y, w, h],
        color: [
            color.0 as f32 / 255.0,
            color.1 as f32 / 255.0,
            color.2 as f32 / 255.0,
            alpha.clamp(0.0, 1.0),
        ],
        params: [radius.max(0.0), stroke.max(0.0), MODE_RECT, 0.0],
        clip: [0.0; 4],
        uv: [0.0; 4],
    }
}

#[allow(clippy::too_many_arguments)]
pub fn glyph(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    uv: [f32; 4],
    color: Rgb,
    alpha: f32,
    clip: crate::font::Clip,
    is_color: bool,
) -> Instance {
    Instance {
        rect: [x, y, w, h],
        color: [
            color.0 as f32 / 255.0,
            color.1 as f32 / 255.0,
            color.2 as f32 / 255.0,
            alpha.clamp(0.0, 1.0),
        ],
        params: [0.0, 0.0, MODE_GLYPH, if is_color { 1.0 } else { 0.0 }],
        clip: [clip.x, clip.y, clip.w, clip.h],
        uv,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AtlasEntry {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
    pub is_color: bool,
}

/// A simple shelf-packed glyph atlas shared between the frame layers.
pub struct GlyphAtlas {
    size: u32,
    data: Vec<u8>,
    entries: HashMap<CacheKey, AtlasEntry>,
    pending: Vec<AtlasEntry>,
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
    full: bool,
}

impl GlyphAtlas {
    pub fn new() -> Self {
        Self {
            size: ATLAS_SIZE,
            data: vec![0; (ATLAS_SIZE * ATLAS_SIZE * 4) as usize],
            entries: HashMap::new(),
            pending: Vec::new(),
            shelf_x: 0,
            shelf_y: 0,
            shelf_h: 0,
            full: false,
        }
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    pub fn take_pending(&mut self) -> Vec<AtlasEntry> {
        std::mem::take(&mut self.pending)
    }

    /// Copy one entry's pixels out of the atlas, row by row, so they can be
    /// uploaded without sending the full texture.
    pub fn entry_data(&self, entry: &AtlasEntry) -> Vec<u8> {
        let row = entry.w as usize * 4;
        let mut out = Vec::with_capacity(row * entry.h as usize);
        for y in 0..entry.h {
            let start = (((entry.y + y) * self.size + entry.x) * 4) as usize;
            out.extend_from_slice(&self.data[start..start + row]);
        }
        out
    }

    /// Add a glyph, returning its atlas placement. Glyphs are cached by cache
    /// key, so repeated frames only pay for the lookup.
    pub fn intern(&mut self, key: CacheKey, image: &SwashImage) -> Option<AtlasEntry> {
        if let Some(entry) = self.entries.get(&key) {
            return Some(*entry);
        }
        if self.full {
            return None;
        }
        let w = image.placement.width;
        let h = image.placement.height;
        if w == 0 || h == 0 {
            return None;
        }
        // One pixel of padding keeps nearest-neighbour sampling away from the
        // neighbouring glyph.
        if self.shelf_x + w + 1 > self.size {
            self.shelf_x = 0;
            self.shelf_y += self.shelf_h + 1;
            self.shelf_h = 0;
        }
        if self.shelf_y + h + 1 > self.size {
            self.full = true;
            eprintln!(
                "prompt: glyph atlas is full ({}x{}); new glyphs are skipped",
                self.size, self.size
            );
            return None;
        }

        let is_color = matches!(image.content, SwashContent::Color);
        let x = self.shelf_x;
        let y = self.shelf_y;
        let atlas_size = self.size as usize;
        let dst_stride = atlas_size * 4;
        let src_stride = match image.content {
            SwashContent::Mask => w as usize,
            SwashContent::Color => w as usize * 4,
            SwashContent::SubpixelMask => w as usize * 3,
        };
        for row in 0..h as usize {
            for col in 0..w as usize {
                let (r, g, b, a) = match image.content {
                    SwashContent::Mask => {
                        let a = image.data[row * src_stride + col];
                        (255, 255, 255, a)
                    }
                    SwashContent::Color => {
                        let i = row * src_stride + col * 4;
                        (
                            image.data[i],
                            image.data[i + 1],
                            image.data[i + 2],
                            image.data[i + 3],
                        )
                    }
                    SwashContent::SubpixelMask => {
                        let a = image.data[row * src_stride + col * 3 + 1];
                        (255, 255, 255, a)
                    }
                };
                let i = (y as usize + row) * dst_stride + (x as usize + col) * 4;
                self.data[i] = r;
                self.data[i + 1] = g;
                self.data[i + 2] = b;
                self.data[i + 3] = a;
            }
        }

        self.shelf_x += w + 1;
        self.shelf_h = self.shelf_h.max(h);
        let size = self.size as f32;
        let entry = AtlasEntry {
            x,
            y,
            w,
            h,
            u0: x as f32 / size,
            v0: y as f32 / size,
            u1: (x + w) as f32 / size,
            v1: (y + h) as f32 / size,
            is_color,
        };
        self.entries.insert(key, entry);
        self.pending.push(entry);
        Some(entry)
    }
}

impl Default for GlyphAtlas {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Renderer {
    #[allow(dead_code)]
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    target_format: wgpu::TextureFormat,
    atlas_texture: wgpu::Texture,
    atlas: Rc<RefCell<GlyphAtlas>>,
    uniform_buffer: wgpu::Buffer,
    vertex_buffer: wgpu::Buffer,
    vertex_capacity: usize,
}

impl Renderer {
    pub fn new(
        window: Arc<Window>,
        display: Option<winit::event_loop::OwnedDisplayHandle>,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        pollster::block_on(Self::new_async(
            window,
            display,
            width.max(1),
            height.max(1),
        ))
    }

    async fn new_async(
        window: Arc<Window>,
        display: Option<winit::event_loop::OwnedDisplayHandle>,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
            flags: Default::default(),
            memory_budget_thresholds: Default::default(),
            backend_options: Default::default(),
            display: display
                .map(|handle| Box::new(handle) as Box<dyn wgpu::wgt::WgpuHasDisplayHandle>),
        });
        let surface = instance.create_surface(window).ok()?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .ok()?;
        let info = adapter.get_info();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("prompt"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;

        let caps = surface.get_capabilities(&adapter);
        let is_8bit = |format: wgpu::TextureFormat| {
            matches!(
                format.remove_srgb_suffix(),
                wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
            )
        };
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|format| is_8bit(*format) && !format.is_srgb())
            .or_else(|| caps.formats.iter().copied().find(|format| is_8bit(*format)))
            .or_else(|| caps.formats.first().copied())?;
        let mut config = surface.get_default_config(&adapter, width, height)?;
        config.format = format;
        config.present_mode = wgpu::PresentMode::AutoVsync;
        // Draw sRGB-encoded values straight into a non-sRGB view: colours then
        // match the tiny-skia output pixel for pixel instead of picking up an
        // extra linearisation round trip.
        let view_format = format.remove_srgb_suffix();
        config.view_formats = vec![view_format, view_format.add_srgb_suffix()];
        surface.configure(&device, &config);

        let atlas = Rc::new(RefCell::new(GlyphAtlas::new()));
        let atlas_size = atlas.borrow().size();
        let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("prompt glyph atlas"),
            size: wgpu::Extent3d {
                width: atlas_size,
                height: atlas_size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("prompt glyph sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("prompt uniforms"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("prompt bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("prompt bind group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("prompt painter shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("prompt pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("prompt painter pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x4,
                        1 => Float32x4,
                        2 => Float32x4,
                        3 => Float32x4,
                        4 => Float32x4,
                    ],
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: view_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let vertex_capacity = 4096;
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("prompt instances"),
            contents: bytemuck::cast_slice(&vec![Instance::zeroed(); vertex_capacity]),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });

        let mut renderer = Self {
            instance,
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            target_format: view_format,
            atlas_texture,
            atlas,
            uniform_buffer: uniform,
            vertex_buffer,
            vertex_capacity,
        };
        renderer.upload_uniforms();
        eprintln!(
            "prompt: GPU renderer: {} ({:?}, {:?})",
            info.name, info.backend, format
        );
        Some(renderer)
    }

    pub fn atlas(&self) -> Rc<RefCell<GlyphAtlas>> {
        self.atlas.clone()
    }

    fn upload_uniforms(&mut self) {
        let data = [
            self.config.width as f32,
            self.config.height as f32,
            0.0,
            0.0,
        ];
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::cast_slice(&data));
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if self.config.width == width && self.config.height == height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    fn prepare(&mut self, instances: &[Instance]) {
        self.upload_glyphs();
        self.upload_uniforms();

        let needed = instances.len();
        if needed > self.vertex_capacity {
            self.vertex_capacity = needed.next_power_of_two();
            self.vertex_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("prompt instances"),
                size: (self.vertex_capacity * std::mem::size_of::<Instance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        self.queue
            .write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(instances));
    }

    pub fn render(&mut self, instances: &[Instance]) {
        if instances.is_empty() {
            return;
        }
        self.prepare(instances);

        use wgpu::CurrentSurfaceTexture;
        let frame = match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) | CurrentSurfaceTexture::Suboptimal(frame) => {
                frame
            }
            CurrentSurfaceTexture::Lost | CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                match self.surface.get_current_texture() {
                    CurrentSurfaceTexture::Success(frame)
                    | CurrentSurfaceTexture::Suboptimal(frame) => frame,
                    _ => return,
                }
            }
            CurrentSurfaceTexture::Timeout
            | CurrentSurfaceTexture::Occluded
            | CurrentSurfaceTexture::Validation => return,
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.config.format.remove_srgb_suffix()),
            ..Default::default()
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("prompt frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("prompt painter"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.draw(0..6, 0..instances.len() as u32);
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
    }

    /// Render the current scene to an offscreen texture and return it as a
    /// pixmap. Used by `PROMPT_SCREENSHOT` when the GPU renderer is active.
    pub fn screenshot(&mut self, instances: &[Instance], path: &std::path::Path) -> bool {
        if instances.is_empty() {
            return false;
        }
        if !matches!(
            self.target_format,
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
        ) {
            return false;
        }
        self.prepare(instances);
        let width = self.config.width;
        let height = self.config.height;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("prompt screenshot"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let padded_row = (width * 4).div_ceil(256) * 256;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("prompt screenshot readback"),
            size: (padded_row * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("prompt screenshot"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("prompt screenshot"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.draw(0..6, 0..instances.len() as u32);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let (tx, rx) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        if self
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .is_err()
        {
            return false;
        }
        if !matches!(rx.recv(), Ok(Ok(()))) {
            return false;
        }

        let Ok(mapped) = readback.slice(..).get_mapped_range() else {
            return false;
        };
        let Some(mut pixmap) = tiny_skia::Pixmap::new(width, height) else {
            return false;
        };
        let bgra = self.target_format == wgpu::TextureFormat::Bgra8Unorm;
        for y in 0..height as usize {
            let src = y * padded_row as usize;
            let dst = y * width as usize * 4;
            let pixels = &mapped[src..src + width as usize * 4];
            let out = &mut pixmap.data_mut()[dst..dst + width as usize * 4];
            if bgra {
                for (rgba, bgra) in out.chunks_exact_mut(4).zip(pixels.chunks_exact(4)) {
                    rgba[0] = bgra[2];
                    rgba[1] = bgra[1];
                    rgba[2] = bgra[0];
                    rgba[3] = bgra[3];
                }
            } else {
                out.copy_from_slice(pixels);
            }
        }
        drop(mapped);
        readback.unmap();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        pixmap.save_png(path).is_ok()
    }

    fn upload_glyphs(&mut self) {
        let pending = self.atlas.borrow_mut().take_pending();
        if pending.is_empty() {
            return;
        }
        for entry in pending {
            let data = self.atlas.borrow().entry_data(&entry);
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: entry.x,
                        y: entry.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(entry.w * 4),
                    rows_per_image: Some(entry.h),
                },
                wgpu::Extent3d {
                    width: entry.w,
                    height: entry.h,
                    depth_or_array_layers: 1,
                },
            );
        }
    }
}

const SHADER: &str = r#"
struct Uniforms {
    screen: vec2<f32>,
    pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct Instance {
    @location(0) rect: vec4<f32>,
    @location(1) color: vec4<f32>,
    @location(2) params: vec4<f32>,
    @location(3) clip: vec4<f32>,
    @location(4) uv: vec4<f32>,
};

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) params: vec4<f32>,
    @location(3) clip: vec4<f32>,
    @location(4) uv: vec2<f32>,
    @location(5) world: vec2<f32>,
    @location(6) size: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32, inst: Instance) -> VertexOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let corner = corners[index];
    let world = inst.rect.xy + corner * inst.rect.zw;
    var out: VertexOut;
    out.position = vec4<f32>(
        world.x / u.screen.x * 2.0 - 1.0,
        1.0 - world.y / u.screen.y * 2.0,
        0.0,
        1.0,
    );
    out.local = corner * inst.rect.zw;
    out.world = world;
    out.size = inst.rect.zw;
    out.color = inst.color;
    out.params = inst.params;
    out.clip = inst.clip;
    out.uv = mix(inst.uv.xy, inst.uv.zw, corner);
    return out;
}

fn sd_round_box(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half + vec2<f32>(r, r);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    if (in.clip.z > 0.0 && in.clip.w > 0.0) {
        if (in.world.x < in.clip.x || in.world.y < in.clip.y ||
            in.world.x >= in.clip.x + in.clip.z || in.world.y >= in.clip.y + in.clip.w) {
            discard;
        }
    }

    var color = in.color;
    if (in.params.z > 0.5) {
        let texel = textureSample(atlas, atlas_sampler, in.uv);
        if (in.params.w > 0.5) {
            // Colour glyph (emoji): keep its RGB, apply the caller's alpha.
            color = vec4<f32>(texel.rgb, texel.a * color.a);
        } else {
            // Coverage mask: tint with the requested colour.
            color = vec4<f32>(color.rgb, color.a * texel.a);
        }
    } else {
        let half = in.size * 0.5;
        let p = in.local - half;
        let radius = min(in.params.x, min(half.x, half.y));
        let dist = sd_round_box(p, half, radius);
        let aa = max(fwidth(dist), 0.0001) * 0.5;
        var coverage = 1.0 - smoothstep(-aa, aa, dist);
        if (in.params.y > 0.0) {
            let ring = abs(dist) - in.params.y * 0.5;
            coverage = 1.0 - smoothstep(-aa, aa, ring);
        }
        color.a = color.a * coverage;
    }

    if (color.a <= 0.0) {
        discard;
    }
    return color;
}
"#;
