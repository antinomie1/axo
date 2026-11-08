//! The wgpu renderer: terminals drawn on aegle's shared wgpu device, which is
//! Metal on macOS.
//!
//! Each surface is an sRGB texture registered with aegle. A render submits one
//! command buffer on aegle's queue before aegle's own frame, so the frame that
//! samples the texture sees the finished image. wgpu tracks every hazard and
//! keeps resources alive while submissions use them, so unlike the Vulkan
//! backend nothing here waits for the device, fences surfaces or retires
//! textures. Uploads (`write_texture`, `write_buffer_with`) run at the start
//! of the next submission, which also lets all surfaces share one instance
//! buffer: each render's write lands right before its own draws.

use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

use aegle::{
    app::{SharedGpu, wgpu},
    ui::scene::{Rect, SceneBuilder, TextureId},
};
use axo_term::Pixels;

use crate::{
    CellSize, FontSpec, Frame, Renderer, Result, SurfaceId,
    glyphs::{GlyphKey, Glyphs},
    quads::{self, AtlasGlyph, GlyphSource, Quad, Quads},
    shelf::pack,
};

/// Terminal surfaces and images: sampling decodes sRGB to linear.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// GPU memory images out of view may keep.
const BUDGET: u64 = 256 << 20;
const QUAD: u64 = size_of::<Quad>() as u64;

/// The instance layout of [`Quad`]: 32 bytes.
const ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
    0 => Float32x4,
    1 => Uint16x4,
    2 => Unorm8x4,
    3 => Uint32,
];

/// One quad in the vertex layout, its rectangle moved into clip space.
/// WebGPU clip space points y up, so rows grow downwards from `+1`.
fn instance(quad: &Quad, sx: f32, sy: f32) -> [u8; 32] {
    let [x, y, w, h] = quad.rect;
    let mut out = [0; 32];
    for (i, v) in [x * sx - 1.0, 1.0 - y * sy, w * sx, -h * sy]
        .into_iter()
        .enumerate()
    {
        out[i * 4..][..4].copy_from_slice(&v.to_ne_bytes());
    }
    for (i, v) in quad.uv.into_iter().enumerate() {
        out[16 + i * 2..][..2].copy_from_slice(&v.to_ne_bytes());
    }
    out[24..28].copy_from_slice(&quad.color);
    out[28..].copy_from_slice(&quad.kind.to_ne_bytes());
    out
}

fn extent([width, height]: [u32; 2]) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}

fn texture(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    size: [u32; 2],
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("axo"),
        size: extent(size),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

/// Queues `bytes`, tightly packed rows of `bpp` bytes per texel, for `at`.
fn upload(
    queue: &wgpu::Queue,
    target: &wgpu::Texture,
    at: [u32; 2],
    size: [u32; 2],
    bpp: u32,
    bytes: &[u8],
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: target,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: at[0],
                y: at[1],
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size[0] * bpp),
            rows_per_image: None,
        },
        extent(size),
    );
}

/// One atlas page: a texture and its shelf allocator.
struct Page {
    texture: wgpu::Texture,
    side: u32,
    shelf: (u32, u32, u32),
}

/// The glyph cache shared by every surface: a coverage page (`R8Unorm`) and a
/// color page (`Rgba8Unorm`, straight sRGB), as the Vulkan backend keeps them.
/// New textures start cleared, and glyphs upload as they are rasterized.
struct Atlas {
    mask: Page,
    color: Page,
    map: HashMap<GlyphKey, Option<AtlasGlyph>>,
    /// Group 0 of the pipelines: both pages and the image sampler.
    group: wgpu::BindGroup,
    /// A glyph did not fit; the caller resets and lays out again.
    full: bool,
}

impl Atlas {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        side: u32,
    ) -> Self {
        let page = |format, side| Page {
            texture: texture(
                device,
                format,
                [side, side],
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            ),
            side,
            shelf: (0, 0, 0),
        };
        let mask = page(wgpu::TextureFormat::R8Unorm, side);
        // Color glyphs are rare and large; a quarter page holds hundreds.
        let color = page(
            wgpu::TextureFormat::Rgba8Unorm,
            (side / 2).max(512).min(side),
        );
        let views = [&mask, &color].map(|page| page.texture.create_view(&Default::default()));
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("axo atlas"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&views[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&views[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        Self {
            mask,
            color,
            map: HashMap::new(),
            group,
            full: false,
        }
    }

    /// Forgets every glyph; their texels are overwritten by later uploads,
    /// which the queue orders after the renders that sampled them.
    fn reset(&mut self) {
        self.map.clear();
        self.mask.shelf = (0, 0, 0);
        self.color.shelf = (0, 0, 0);
        self.full = false;
    }

    fn insert(
        &mut self,
        queue: &wgpu::Queue,
        glyphs: &mut Glyphs,
        key: GlyphKey,
    ) -> Option<AtlasGlyph> {
        let bitmap = glyphs.rasterize(key)?;
        let page = match bitmap.color {
            true => &mut self.color,
            false => &mut self.mask,
        };
        let Some((x, y)) = pack(&mut page.shelf, page.side, bitmap.width, bitmap.height) else {
            self.full = true;
            return None;
        };
        upload(
            queue,
            &page.texture,
            [x, y],
            [bitmap.width, bitmap.height],
            bitmap.stride() as u32,
            &bitmap.data,
        );
        Some(AtlasGlyph {
            uv: [
                x as u16,
                y as u16,
                bitmap.width as u16,
                bitmap.height as u16,
            ],
            left: bitmap.left as i16,
            top: bitmap.top as i16,
            color: bitmap.color,
        })
    }
}

/// The GPU copy of terminal [`Pixels`]: made the first time they are visible,
/// dropped once the terminal lets go of them or, out of view, over budget.
struct Image {
    source: Weak<Pixels>,
    group: wgpu::BindGroup,
    /// The render that last drew it.
    used: u64,
    bytes: u64,
}

/// Images by the address of their pixels, which the `Weak` keeps from reuse.
#[derive(Default)]
struct Images {
    map: HashMap<usize, Image>,
    serial: u64,
    bytes: u64,
}

impl Images {
    /// Starts a render: forgets what the terminals dropped.
    fn begin(&mut self) {
        self.serial += 1;
        let bytes = &mut self.bytes;
        self.map.retain(|_, image| {
            let alive = image.source.strong_count() > 0;
            if !alive {
                *bytes -= image.bytes;
            }
            alive
        });
    }

    /// Evicts the longest unused images while over budget.
    fn trim(&mut self) {
        while self.bytes > BUDGET {
            let oldest = self
                .map
                .iter()
                .filter(|(_, image)| image.used != self.serial)
                .min_by_key(|(_, image)| image.used)
                .map(|(&key, _)| key);
            let Some(key) = oldest else { break };
            self.bytes -= self.map.remove(&key).map_or(0, |image| image.bytes);
        }
    }
}

/// Everything a quad layout looks glyphs and images up in.
struct Source<'a> {
    atlas: &'a mut Atlas,
    glyphs: &'a mut Glyphs,
    images: &'a mut Images,
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    image_layout: &'a wgpu::BindGroupLayout,
}

impl GlyphSource for Source<'_> {
    fn glyph(&mut self, key: GlyphKey) -> Option<AtlasGlyph> {
        if let Some(&entry) = self.atlas.map.get(&key) {
            return entry;
        }
        let entry = self.atlas.insert(self.queue, self.glyphs, key);
        // A failed insert because the atlas is full is retried after the reset.
        if !self.atlas.full {
            self.atlas.map.insert(key, entry);
        }
        entry
    }

    fn image(&mut self, pixels: &Arc<Pixels>) -> Option<u64> {
        let key = Arc::as_ptr(pixels) as usize;
        let serial = self.images.serial;
        if let Some(image) = self.images.map.get_mut(&key) {
            image.used = serial;
            return Some(key as u64);
        }
        let size = [pixels.width, pixels.height];
        let max = self.device.limits().max_texture_dimension_2d;
        if size.into_iter().any(|side| side > max) {
            return None;
        }
        let texture = texture(
            self.device,
            FORMAT,
            size,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        upload(self.queue, &texture, [0, 0], size, 4, &pixels.rgba);
        let view = texture.create_view(&Default::default());
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("axo image"),
            layout: self.image_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
        });
        let bytes = pixels.rgba.len() as u64;
        self.images.bytes += bytes;
        self.images.map.insert(
            key,
            Image {
                source: Arc::downgrade(pixels),
                group,
                used: serial,
                bytes,
            },
        );
        Some(key as u64)
    }
}

struct Target {
    view: wgpu::TextureView,
    size: [u32; 2],
    texture: TextureId,
}

pub struct WgpuRenderer {
    gpu: SharedGpu,
    device: wgpu::Device,
    queue: wgpu::Queue,
    atlas_layout: wgpu::BindGroupLayout,
    image_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipeline: wgpu::RenderPipeline,
    /// The same quads, sampling an image (group 1) instead of the atlas pages.
    image_pipeline: wgpu::RenderPipeline,
    atlas: Atlas,
    images: Images,
    glyphs: Glyphs,
    cell: CellSize,
    /// Each surface's texture, created by its first render.
    surfaces: Vec<Option<Option<Target>>>,
    /// Instances of whichever surface renders, grown to the largest frame.
    instances: Option<wgpu::Buffer>,
    quads: Quads,
}

impl WgpuRenderer {
    /// Creates the pipelines and atlas on aegle's device (`App::wgpu`).
    pub fn new(gpu: SharedGpu) -> Self {
        let device = gpu.device().clone();
        let queue = gpu.queue().clone();
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty,
            count: None,
        };
        let page = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        // Binding 0 is the coverage page, 1 the color page, 2 the sampler
        // images use.
        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("axo atlas"),
            entries: &[
                entry(0, page),
                entry(1, page),
                entry(
                    2,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                ),
            ],
        });
        let image_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("axo image"),
            entries: &[entry(0, page)],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("axo"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("axo terminal"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/terminal.wgsl").into()),
        });
        // WebGPU requires every group of a layout bound, so the glyph pipeline
        // leaves out the image group it never samples.
        let pipeline = |layouts: &[Option<&wgpu::BindGroupLayout>], entry| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("axo"),
                bind_group_layouts: layouts,
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("axo terminal"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: QUAD,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &ATTRIBUTES,
                    })],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: FORMAT,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let glyph_pipeline = pipeline(&[Some(&atlas_layout)], "fs_main");
        let image_pipeline = pipeline(&[Some(&atlas_layout), Some(&image_layout)], "fs_image");
        let atlas = Atlas::new(&device, &atlas_layout, &sampler, 1024);
        Self {
            gpu,
            device,
            queue,
            atlas_layout,
            image_layout,
            sampler,
            pipeline: glyph_pipeline,
            image_pipeline,
            atlas,
            images: Images::default(),
            glyphs: Glyphs::new(),
            cell: CellSize::default(),
            surfaces: Vec::new(),
            instances: None,
            quads: Quads::default(),
        }
    }

    fn layout(&mut self, frame: &Frame<'_>) {
        let mut source = Source {
            atlas: &mut self.atlas,
            glyphs: &mut self.glyphs,
            images: &mut self.images,
            device: &self.device,
            queue: &self.queue,
            image_layout: &self.image_layout,
        };
        quads::build(frame, self.cell, &mut source, &mut self.quads);
    }

    /// Writes this frame's instances, growing the buffer to a power of two;
    /// `None` when there are none.
    fn write_instances(&mut self, size: [u32; 2]) -> Option<()> {
        let bytes = self.quads.len() as u64 * QUAD;
        let bytes = wgpu::BufferSize::new(bytes)?;
        if self
            .instances
            .as_ref()
            .is_none_or(|buffer| buffer.size() < bytes.get())
        {
            self.instances = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("axo instances"),
                size: bytes.get().next_power_of_two().max(64 * 1024),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let buffer = self.instances.as_ref()?;
        let mut view = self.queue.write_buffer_with(buffer, 0, bytes)?;
        let (sx, sy) = (2.0 / size[0] as f32, 2.0 / size[1] as f32);
        // Straight into wgpu's staging memory: one pass, no copy of our own.
        let (slots, _) = view.slice(..).into_chunks::<32>();
        slots.write_iter(self.quads.iter().map(|quad| instance(quad, sx, sy)));
        Some(())
    }
}

impl Renderer for WgpuRenderer {
    fn set_font(&mut self, font: &FontSpec, scale: f32) -> Result<CellSize> {
        self.cell = self.glyphs.set_font(font, scale)?;
        let area = (self.cell.width * self.cell.height * 1024) as f64;
        let side = (area.sqrt() as u32).next_power_of_two().clamp(1024, 4096);
        self.atlas = Atlas::new(&self.device, &self.atlas_layout, &self.sampler, side);
        Ok(self.cell)
    }

    fn cell_size(&self) -> CellSize {
        self.cell
    }

    fn create_surface(&mut self) -> Result<SurfaceId> {
        let index = match self.surfaces.iter().position(Option::is_none) {
            Some(index) => index,
            None => {
                self.surfaces.push(None);
                self.surfaces.len() - 1
            }
        };
        self.surfaces[index] = Some(None);
        Ok(SurfaceId(index as u32))
    }

    fn destroy_surface(&mut self, surface: SurfaceId) {
        if let Some(Some(target)) = self
            .surfaces
            .get_mut(surface.0 as usize)
            .and_then(Option::take)
        {
            self.gpu.unregister_texture(target.texture);
        }
    }

    fn render(&mut self, id: SurfaceId, size: [u32; 2], frame: &Frame<'_>) -> Result {
        let index = id.0 as usize;
        if size[0] == 0 || size[1] == 0 || self.cell.width == 0 {
            return Ok(());
        }
        let target = self
            .surfaces
            .get_mut(index)
            .and_then(Option::as_mut)
            .ok_or("unknown surface")?;
        if target.as_ref().map(|t| t.size) != Some(size) {
            let max = self.device.limits().max_texture_dimension_2d;
            if size[0] > max || size[1] > max {
                return Err(format!("terminal of {size:?} pixels exceeds the GPU's {max}").into());
            }
            // Frames already recorded keep the old texture alive.
            if let Some(old) = target.take() {
                self.gpu.unregister_texture(old.texture);
            }
            let texture = texture(
                &self.device,
                FORMAT,
                size,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            );
            *target = Some(Target {
                view: texture.create_view(&Default::default()),
                size,
                texture: self.gpu.register_texture(&texture)?,
            });
        }

        self.images.begin();
        self.layout(frame);
        if self.atlas.full {
            self.atlas.reset();
            self.layout(frame);
        }
        self.images.trim();

        let cell = self.cell;
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let drawn = self.write_instances(size).is_some();
        let target = self.surfaces[index].as_ref().unwrap().as_ref().unwrap();
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("axo terminal"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            if let Some(instances) = self.instances.as_ref().filter(|_| drawn) {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.atlas.group, &[]);
                pass.set_vertex_buffer(0, instances.slice(..));
                // Instances are in drawing order: each layer of images, then
                // the quads above it. Images stay inside the cell area, which
                // the scissor clamps to the target as WebGPU requires.
                let [below, under, over] = self.quads.layers();
                let x = frame.padding.min(size[0]);
                let y = frame.padding.min(size[1]);
                let cells = [
                    x,
                    y,
                    (frame.term.cols() as u32 * cell.width).min(size[0] - x),
                    (frame.term.rows() as u32 * cell.height).min(size[1] - y),
                ];
                let mut first = 0;
                for (images, quads) in [
                    (below, self.quads.background.len()),
                    (under, self.quads.foreground.len()),
                    (over, 0),
                ] {
                    if !images.is_empty() {
                        pass.set_scissor_rect(cells[0], cells[1], cells[2], cells[3]);
                        pass.set_pipeline(&self.image_pipeline);
                        for image in images {
                            let group = &self.images.map[&(image.texture as usize)].group;
                            pass.set_bind_group(1, group, &[]);
                            pass.draw(0..4, first..first + 1);
                            first += 1;
                        }
                        pass.set_scissor_rect(0, 0, size[0], size[1]);
                        pass.set_pipeline(&self.pipeline);
                    }
                    if quads != 0 {
                        pass.draw(0..4, first..first + quads as u32);
                        first += quads as u32;
                    }
                }
            }
        }
        self.queue.submit([encoder.finish()]);
        Ok(())
    }

    fn paint(&self, surface: SurfaceId, builder: &mut SceneBuilder, rect: Rect) {
        if let Some(Some(Some(target))) = self.surfaces.get(surface.0 as usize) {
            builder.texture(target.texture, rect);
        }
    }
}

impl Drop for WgpuRenderer {
    fn drop(&mut self) {
        for target in self.surfaces.drain(..).flatten().flatten() {
            self.gpu.unregister_texture(target.texture);
        }
    }
}
