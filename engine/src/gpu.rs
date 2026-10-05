//! Drawing with wgpu. The playfield goes into the scene texture, whose glowing parts are blurred
//! into the bloom. The scene and bloom are composed into the frame with their effects, the player
//! and the interface's shapes and text go on top, and the frame goes to the window, inside the
//! black bars if there are any. The scene and the frame are multisampled when antialiasing.
//!
//! Everything is linear light (see colour.rs), kept in floats until the window's surface encodes it
//! to sRGB.

use std::error::Error;

use bytemuck::{Pod, Zeroable};
use sdl3::video::Window;
use wgpu::rwh::{HasDisplayHandle, HasWindowHandle};
use wgpu::util::DeviceExt;

use crate::colour::Linear;
use crate::save::{VSYNCS, Vsync};
use crate::text::{ATLAS, GlyphUpload};

/// A vertex in pixels of the frame. Shapes ignore `uv`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    pub color: Linear,
}

const FRAME_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const BLOOM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// The most bloom levels, each half the size of the last, from half the frame's size down.
const BLOOM_LEVELS: usize = 6;

/// Shader code prepended to the shaders that use it.
const FULLSCREEN: &str = include_str!("fullscreen.wgsl");
const EFFECTS: &str = include_str!("effects.wgsl");

/// What to draw this frame.
pub struct Frame<'a> {
    pub clear: Linear,
    /// The playfield.
    pub scene: &'a [Vertex],
    pub player: &'a [Vertex],
    /// The interface's shapes.
    pub gui: &'a [Vertex],
    pub text: &'a [Vertex],
    pub glyphs: &'a mut Vec<GlyphUpload>,
    /// The frame's size in pixels.
    pub size: (u32, u32),
    /// Where the frame goes in the window.
    pub at: (u32, u32),
    pub samples: u32,
    pub aberration: f32,
    /// How much bloom is added, or 0 for none.
    pub bloom: f32,
    /// The lightness of the lighter background colour, which glowing parts must be lighter than.
    pub backdrop: f32,
}

/// A screenshot: RGBA rows, top first.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A texture drawn into, and its bind group for drawing from it.
struct Level {
    view: wgpu::TextureView,
    source: wgpu::BindGroup,
}

/// The offscreen textures, at one size and sample count.
struct Target {
    size: (u32, u32),
    samples: u32,
    /// What the scene and the frame are drawn into when multisampling, resolving into them.
    msaa: Option<wgpu::TextureView>,
    scene: Level,
    /// Largest first.
    bloom: Vec<Level>,
    frame: Level,
    /// The scene and the bloom, for composing.
    compose: wgpu::BindGroup,
}

/// The pipelines that draw into multisampled targets, for one sample count.
struct Multisampled {
    samples: u32,
    shapes: wgpu::RenderPipeline,
    compose: wgpu::RenderPipeline,
}

pub struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// The antialiasing sample counts the frame supports, ascending from 1.
    sample_counts: Vec<u32>,
    /// The vsync modes the surface supports, in the menu's order.
    vsyncs: Vec<Vsync>,

    // what pipelines for other sample counts are made from
    draw_module: wgpu::ShaderModule,
    shapes_layout: wgpu::PipelineLayout,
    compose_module: wgpu::ShaderModule,
    compose_layout: wgpu::PipelineLayout,

    multisampled: Vec<Multisampled>,
    text: wgpu::RenderPipeline,
    bloom_bright: wgpu::RenderPipeline,
    bloom_down: wgpu::RenderPipeline,
    bloom_up: wgpu::RenderPipeline,
    copy: wgpu::RenderPipeline,

    /// A texture and sampler to draw from.
    source_layout: wgpu::BindGroupLayout,
    compose_textures_layout: wgpu::BindGroupLayout,
    linear: wgpu::Sampler,

    /// The frame's size, for the vertex shader.
    screen: wgpu::Buffer,
    screen_group: wgpu::BindGroup,
    effects: wgpu::Buffer,
    effects_group: wgpu::BindGroup,
    atlas: wgpu::Texture,
    atlas_group: wgpu::BindGroup,
    vertices: wgpu::Buffer,
    target: Option<Target>,
}

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4];
    wgpu::VertexBufferLayout { array_stride: size_of::<Vertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &ATTRS }
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None }
}

/// A shader module from WGSL sources, joined in order.
fn module(device: &wgpu::Device, label: &str, sources: &[&str]) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(sources.concat().into()) })
}

/// A pipeline drawing vertices from the vertex buffer.
fn vertex_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    fragment: &str,
    samples: u32,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(fragment),
        layout: Some(layout),
        vertex: wgpu::VertexState { module, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[Some(vertex_layout())] },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState { count: samples, ..Default::default() },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fragment),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format: FRAME_FORMAT, blend, write_mask: wgpu::ColorWrites::ALL })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// A pipeline drawing one triangle over its target, from a texture.
fn fullscreen_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    fragment: &str,
    samples: u32,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(fragment),
        layout: Some(layout),
        vertex: wgpu::VertexState { module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState { count: samples, ..Default::default() },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fragment),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// One render pass into `view` (resolving into `resolve` if multisampled).
fn begin<'e>(enc: &'e mut wgpu::CommandEncoder, view: &wgpu::TextureView, resolve: Option<&wgpu::TextureView>, load: wgpu::LoadOp<wgpu::Color>) -> wgpu::RenderPass<'e> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: resolve,
            ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
        })],
        ..Default::default()
    })
}

/// What to draw into to reach `level`: the multisampled texture resolving into it, if there is one.
fn attachment<'a>(msaa: Option<&'a wgpu::TextureView>, level: &'a Level) -> (&'a wgpu::TextureView, Option<&'a wgpu::TextureView>) {
    match msaa {
        Some(msaa) => (msaa, Some(&level.view)),
        None => (&level.view, None),
    }
}

fn present_mode(vsync: Vsync) -> wgpu::PresentMode {
    match vsync {
        Vsync::Off => wgpu::PresentMode::Immediate,
        Vsync::On => wgpu::PresentMode::Fifo,
        Vsync::Mailbox => wgpu::PresentMode::Mailbox,
    }
}

impl Gpu {
    /// # Safety
    /// The window must outlive the `Gpu`.
    pub unsafe fn new(window: &Window, vsync: Vsync) -> Result<Gpu, Box<dyn Error>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(window.display_handle()?.as_raw()),
            raw_window_handle: window.window_handle()?.as_raw(),
        };
        // SAFETY: the caller keeps the window alive for as long as the surface.
        let surface = unsafe { instance.create_surface_unsafe(target)? };
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))?;
        // 1 and 4 samples are always supported; other counts need this feature
        let specific = adapter.features() & wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { required_features: specific, ..Default::default() }))?;
        let sample_counts = if specific.is_empty() {
            vec![1, 4]
        } else {
            adapter.get_texture_format_features(FRAME_FORMAT).flags.supported_sample_counts()
        };

        let caps = surface.get_capabilities(&adapter);
        let vsyncs: Vec<Vsync> = VSYNCS.into_iter().filter(|&v| caps.present_modes.contains(&present_mode(v))).collect();
        // 8 bits a channel, sRGB
        let format = [wgpu::TextureFormat::Bgra8UnormSrgb, wgpu::TextureFormat::Rgba8UnormSrgb]
            .into_iter()
            .find(|f| caps.formats.contains(f))
            .ok_or("no 8-bit sRGB surface format")?;
        let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        if caps.usages.contains(wgpu::TextureUsages::COPY_SRC) {
            usage |= wgpu::TextureUsages::COPY_SRC;
        }
        let (w, h) = window.size_in_pixels();
        let config = wgpu::SurfaceConfiguration {
            usage,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: w.max(1),
            height: h.max(1),
            present_mode: present_mode(if vsyncs.contains(&vsync) { vsync } else { Vsync::On }),
            // no frames queued behind the one being shown, so what's shown is as fresh as can be
            desired_maximum_frame_latency: 1,
            alpha_mode: caps.alpha_modes[0],
            view_formats: Vec::new(),
        };
        surface.configure(&device, &config);

        let group_layout = |label, entries: &[wgpu::BindGroupLayoutEntry]| device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries });
        let screen_layout = group_layout("screen", &[uniform_entry(0, wgpu::ShaderStages::VERTEX)]);
        let source_layout = group_layout("source", &[texture_entry(0), sampler_entry(1)]);
        let effects_layout = group_layout("effects", &[uniform_entry(0, wgpu::ShaderStages::FRAGMENT)]);
        let compose_textures_layout = group_layout("compose", &[texture_entry(0), sampler_entry(1), texture_entry(2)]);
        let layout = |label, groups: &[Option<&wgpu::BindGroupLayout>]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(label), bind_group_layouts: groups, immediate_size: 0 })
        };
        let shapes_layout = layout("shapes", &[Some(&screen_layout)]);
        let text_layout = layout("text", &[Some(&screen_layout), Some(&source_layout)]);
        let source_pipeline_layout = layout("source", &[Some(&source_layout)]);
        let bright_layout = layout("bright", &[Some(&source_layout), Some(&effects_layout)]);
        let compose_layout = layout("compose", &[Some(&compose_textures_layout), Some(&effects_layout)]);

        let uniform = |label| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(&[0f32; 4]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            })
        };
        let uniform_group = |label, layout: &wgpu::BindGroupLayout, buffer: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some(label), layout, entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }] })
        };
        let screen = uniform("screen");
        let screen_group = uniform_group("screen", &screen_layout, &screen);
        let effects = uniform("effects");
        let effects_group = uniform_group("effects", &effects_layout, &effects);

        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas"),
            size: wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let nearest = device.create_sampler(&wgpu::SamplerDescriptor::default());
        let linear = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let atlas_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas"),
            layout: &source_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&atlas.create_view(&Default::default())) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&nearest) },
            ],
        });

        let draw_module = module(&device, "draw", &[include_str!("draw.wgsl")]);
        let bloom_module = module(&device, "bloom", &[FULLSCREEN, EFFECTS, include_str!("bloom.wgsl")]);
        let compose_module = module(&device, "compose", &[FULLSCREEN, EFFECTS, include_str!("compose.wgsl")]);
        let copy_module = module(&device, "copy", &[FULLSCREEN, include_str!("copy.wgsl")]);
        let text = vertex_pipeline(&device, &text_layout, &draw_module, "fs_text", 1, Some(wgpu::BlendState::ALPHA_BLENDING));
        let add = wgpu::BlendState {
            color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add },
            alpha: wgpu::BlendComponent::REPLACE,
        };
        let bloom_bright = fullscreen_pipeline(&device, &bright_layout, &bloom_module, "fs_bright", 1, BLOOM_FORMAT, None);
        let bloom_down = fullscreen_pipeline(&device, &source_pipeline_layout, &bloom_module, "fs_down", 1, BLOOM_FORMAT, None);
        let bloom_up = fullscreen_pipeline(&device, &source_pipeline_layout, &bloom_module, "fs_up", 1, BLOOM_FORMAT, Some(add));
        let copy = fullscreen_pipeline(&device, &source_pipeline_layout, &copy_module, "fs", 1, format, None);
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vertices"),
            size: 1 << 20,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Gpu {
            surface,
            device,
            queue,
            config,
            sample_counts,
            vsyncs,
            draw_module,
            shapes_layout,
            compose_module,
            compose_layout,
            multisampled: Vec::new(),
            text,
            bloom_bright,
            bloom_down,
            bloom_up,
            copy,
            source_layout,
            compose_textures_layout,
            linear,
            screen,
            screen_group,
            effects,
            effects_group,
            atlas,
            atlas_group,
            vertices,
            target: None,
        })
    }

    /// The antialiasing sample counts the frame supports, ascending from 1.
    pub fn sample_counts(&self) -> &[u32] {
        &self.sample_counts
    }

    /// The vsync modes the surface supports, in the menu's order; always including `On`.
    pub fn vsyncs(&self) -> &[Vsync] {
        &self.vsyncs
    }

    /// Whether `render` can return screenshots.
    pub fn can_capture(&self) -> bool {
        self.config.usage.contains(wgpu::TextureUsages::COPY_SRC)
    }

    pub fn set_vsync(&mut self, vsync: Vsync) {
        self.config.present_mode = present_mode(if self.vsyncs.contains(&vsync) { vsync } else { Vsync::On });
        self.surface.configure(&self.device, &self.config);
    }

    /// Follows the window's size in pixels.
    pub fn resize(&mut self, (w, h): (u32, u32)) {
        let (w, h) = (w.max(1), h.max(1));
        if (self.config.width, self.config.height) != (w, h) {
            (self.config.width, self.config.height) = (w, h);
            self.surface.configure(&self.device, &self.config);
        }
    }

    fn multisampled(&mut self, samples: u32) {
        if !self.multisampled.iter().any(|m| m.samples == samples) {
            let shapes = vertex_pipeline(&self.device, &self.shapes_layout, &self.draw_module, "fs_flat", samples, None);
            let compose = fullscreen_pipeline(&self.device, &self.compose_layout, &self.compose_module, "fs", samples, FRAME_FORMAT, None);
            self.multisampled.push(Multisampled { samples, shapes, compose });
        }
    }

    fn target(&mut self, size: (u32, u32), samples: u32) {
        if self.target.as_ref().is_some_and(|t| t.size == size && t.samples == samples) {
            return;
        }
        let texture = |(w, h): (u32, u32), samples: u32, format, usage| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let level = |size: (u32, u32), format| {
            let view = texture(size, 1, format, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING).create_view(&Default::default());
            let source = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.source_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.linear) },
                ],
            });
            Level { view, source }
        };
        let msaa = (samples > 1).then(|| texture(size, samples, FRAME_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT).create_view(&Default::default()));
        let scene = level(size, FRAME_FORMAT);
        let frame = level(size, FRAME_FORMAT);
        let mut bloom = Vec::new();
        let mut s = (size.0 / 2, size.1 / 2);
        while bloom.len() < BLOOM_LEVELS && s.0 > 0 && s.1 > 0 {
            bloom.push(level(s, BLOOM_FORMAT));
            s = (s.0 / 2, s.1 / 2);
        }
        if bloom.is_empty() {
            bloom.push(level((1, 1), BLOOM_FORMAT));
        }
        let compose = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compose"),
            layout: &self.compose_textures_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&scene.view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.linear) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&bloom[0].view) },
            ],
        });
        self.target = Some(Target { size, samples, msaa, scene, bloom, frame, compose });
    }

    /// The window's next image to draw into, waiting for one to be free (with vsync, until a
    /// refresh), or none if there isn't one this time.
    pub fn acquire(&mut self) -> Option<wgpu::SurfaceTexture> {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => Some(t),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                None
            }
            _ => None,
        }
    }

    /// Draws a frame into an image from `acquire` and shows it; returns a screenshot of it if
    /// asked for one.
    pub fn render(&mut self, surface: wgpu::SurfaceTexture, f: Frame, capture: bool) -> Option<Image> {
        for g in f.glyphs.drain(..) {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &self.atlas, mip_level: 0, origin: wgpu::Origin3d { x: g.x, y: g.y, z: 0 }, aspect: wgpu::TextureAspect::All },
                &g.coverage,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(g.w), rows_per_image: None },
                wgpu::Extent3d { width: g.w, height: g.h, depth_or_array_layers: 1 },
            );
        }
        // the scene, the player, the interface's shapes and its text, one after another
        let bytes = size_of_val(f.scene) + size_of_val(f.player) + size_of_val(f.gui) + size_of_val(f.text);
        if bytes as u64 > self.vertices.size() {
            self.vertices = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vertices"),
                size: (bytes as u64).next_power_of_two(),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        let mut at = 0;
        let mut ranges = [0..0, 0..0, 0..0, 0..0];
        for (range, verts) in ranges.iter_mut().zip([f.scene, f.player, f.gui, f.text]) {
            self.queue.write_buffer(&self.vertices, (at * size_of::<Vertex>()) as u64, bytemuck::cast_slice(verts));
            *range = at as u32..(at + verts.len()) as u32;
            at += verts.len();
        }
        let [scene_range, player_range, gui_range, text_range] = ranges;
        self.queue.write_buffer(&self.screen, 0, bytemuck::cast_slice(&[f.size.0 as f32, f.size.1 as f32, 0.0, 0.0]));
        self.queue.write_buffer(&self.effects, 0, bytemuck::cast_slice(&[f.aberration, f.bloom, f.backdrop, 0.0]));

        let out = surface.texture.create_view(&Default::default());

        // the highest supported count not above the setting
        let samples = self.sample_counts.iter().copied().filter(|&n| n <= f.samples).max().unwrap_or(1);
        self.multisampled(samples);
        self.target(f.size, samples);
        let ms = self.multisampled.iter().find(|m| m.samples == samples).unwrap();
        let t = self.target.as_ref().unwrap();
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let [r, g, b, a] = f.clear.rgba().map(f64::from);
            let (view, resolve) = attachment(t.msaa.as_ref(), &t.scene);
            let mut pass = begin(&mut enc, view, resolve, wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a }));
            pass.set_pipeline(&ms.shapes);
            pass.set_bind_group(0, &self.screen_group, &[]);
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            pass.draw(scene_range, 0..1);
        }
        if f.bloom > 0.0 {
            // the bright parts down the chain, then back up it, each level adding to the larger
            let clear = wgpu::LoadOp::Clear(wgpu::Color::BLACK);
            {
                let mut pass = begin(&mut enc, &t.bloom[0].view, None, clear);
                pass.set_pipeline(&self.bloom_bright);
                pass.set_bind_group(0, &t.scene.source, &[]);
                pass.set_bind_group(1, &self.effects_group, &[]);
                pass.draw(0..3, 0..1);
            }
            let mut step = |pipeline: &wgpu::RenderPipeline, from: &Level, to: &Level, load| {
                let mut pass = begin(&mut enc, &to.view, None, load);
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &from.source, &[]);
                pass.draw(0..3, 0..1);
            };
            for w in t.bloom.windows(2) {
                step(&self.bloom_down, &w[0], &w[1], clear);
            }
            for w in t.bloom.windows(2).rev() {
                step(&self.bloom_up, &w[1], &w[0], wgpu::LoadOp::Load);
            }
        }
        {
            // the scene with its effects, then the player and the interface's shapes over it
            let (view, resolve) = attachment(t.msaa.as_ref(), &t.frame);
            let mut pass = begin(&mut enc, view, resolve, wgpu::LoadOp::Clear(wgpu::Color::BLACK));
            pass.set_pipeline(&ms.compose);
            pass.set_bind_group(0, &t.compose, &[]);
            pass.set_bind_group(1, &self.effects_group, &[]);
            pass.draw(0..3, 0..1);
            pass.set_pipeline(&ms.shapes);
            pass.set_bind_group(0, &self.screen_group, &[]);
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            pass.draw(player_range, 0..1);
            pass.draw(gui_range, 0..1);
        }
        {
            let mut pass = begin(&mut enc, &t.frame.view, None, wgpu::LoadOp::Load);
            pass.set_pipeline(&self.text);
            pass.set_bind_group(0, &self.screen_group, &[]);
            pass.set_bind_group(1, &self.atlas_group, &[]);
            pass.set_vertex_buffer(0, self.vertices.slice(..));
            pass.draw(text_range, 0..1);
        }
        {
            let mut pass = begin(&mut enc, &out, None, wgpu::LoadOp::Clear(wgpu::Color::BLACK));
            pass.set_pipeline(&self.copy);
            pass.set_bind_group(0, &t.frame.source, &[]);
            pass.set_viewport(f.at.0 as f32, f.at.1 as f32, f.size.0 as f32, f.size.1 as f32, 0.0, 1.0);
            pass.draw(0..3, 0..1);
        }
        let readback = (capture && self.can_capture()).then(|| {
            let (w, h) = (self.config.width, self.config.height);
            let row = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("screenshot"),
                size: (row * h) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            enc.copy_texture_to_buffer(
                surface.texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None } },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            (buf, row, w, h)
        });
        self.queue.submit([enc.finish()]);
        let image = readback.and_then(|(buf, row, w, h)| {
            buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
            let data = buf.slice(..).get_mapped_range().ok()?;
            let bgra = self.config.format == wgpu::TextureFormat::Bgra8UnormSrgb;
            let mut rgba = Vec::with_capacity((w * h * 4) as usize);
            for r in data.chunks(row as usize) {
                for p in r[..(w * 4) as usize].chunks(4) {
                    rgba.extend(if bgra { [p[2], p[1], p[0], 255] } else { [p[0], p[1], p[2], 255] });
                }
            }
            Some(Image { width: w, height: h, rgba })
        });
        self.queue.present(surface);
        image
    }
}
