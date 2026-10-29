//! The glyph atlas shared by every terminal surface: one RGBA8 image packed in
//! shelves, filled on demand and uploaded in its own submission ahead of the
//! surfaces that sample it.

use std::collections::HashMap;

use aegle::app::ash::vk;

use super::gpu::{Buffer, Gpu, Image, barrier};
use crate::{
    Result,
    glyphs::{GlyphKey, Glyphs},
    quads::{AtlasGlyph, GlyphSource},
};

/// Transparent texels between glyphs.
const GAP: u32 = 1;

pub struct Atlas {
    pub image: Image,
    side: u32,
    /// Next free x, the shelf's top and height.
    shelf: (u32, u32, u32),
    map: HashMap<GlyphKey, Option<AtlasGlyph>>,
    bytes: Vec<u8>,
    regions: Vec<vk::BufferImageCopy>,
    staging: Option<Buffer>,
    command: vk::CommandBuffer,
    fence: vk::Fence,
    /// A glyph did not fit; the caller resets and lays out again.
    pub full: bool,
}

impl Atlas {
    /// Creates a cleared `side`² atlas, ready for sampling.
    pub fn new(gpu: &Gpu, pool: vk::CommandPool, side: u32) -> Result<Self> {
        let image = gpu.image(
            vk::Format::R8G8B8A8_UNORM,
            [side, side],
            vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
        )?;
        let device = &gpu.device;
        // SAFETY: one command buffer from the renderer's pool, recorded and
        // submitted on this thread; the clear finishes before `new` returns.
        let command = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0]
        };
        let fence = gpu.fence(false)?;
        unsafe {
            device.begin_command_buffer(
                command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            let to_transfer = barrier(
                image.image,
                (vk::ImageLayout::UNDEFINED, vk::AccessFlags::empty()),
                (
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::AccessFlags::TRANSFER_WRITE,
                ),
            );
            device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_transfer],
            );
            device.cmd_clear_color_image(
                command,
                image.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &vk::ClearColorValue::default(),
                &[super::gpu::COLOR_RANGE],
            );
            Self::to_shader(device, command, image.image);
            device.end_command_buffer(command)?;
        }
        gpu.submit(command, fence)?;
        // Waiting without a reset leaves the fence signaled, as `flush` expects when idle.
        // SAFETY: the fence belongs to this device.
        unsafe { gpu.device.wait_for_fences(&[fence], true, u64::MAX)? };
        Ok(Self {
            image,
            side,
            shelf: (0, 0, 0),
            map: HashMap::new(),
            bytes: Vec::new(),
            regions: Vec::new(),
            staging: None,
            command,
            fence,
            full: false,
        })
    }

    fn to_shader(device: &aegle::app::ash::Device, command: vk::CommandBuffer, image: vk::Image) {
        let to_shader = barrier(
            image,
            (
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE,
            ),
            (
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::AccessFlags::SHADER_READ,
            ),
        );
        // SAFETY: recording into a command buffer in the recording state.
        unsafe {
            device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_shader],
            );
        }
    }

    /// Forgets every glyph; their texels are overwritten by later uploads.
    pub fn reset(&mut self) {
        self.map.clear();
        self.shelf = (0, 0, 0);
        self.bytes.clear();
        self.regions.clear();
        self.full = false;
    }

    fn pack(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let (w, h) = (width + GAP, height + GAP);
        if w > self.side || h > self.side {
            return None;
        }
        let (mut x, mut y, mut shelf) = self.shelf;
        if x + w > self.side {
            (x, y, shelf) = (0, y + shelf, 0);
        }
        if y + h > self.side {
            return None;
        }
        self.shelf = (x + w, y, shelf.max(h));
        Some((x, y))
    }

    fn insert(&mut self, glyphs: &mut Glyphs, key: GlyphKey) -> Option<AtlasGlyph> {
        let bitmap = glyphs.rasterize(key)?;
        let Some((x, y)) = self.pack(bitmap.width, bitmap.height) else {
            self.full = true;
            return None;
        };
        self.regions.push(
            vk::BufferImageCopy::default()
                .buffer_offset(self.bytes.len() as u64)
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_offset(vk::Offset3D {
                    x: x as i32,
                    y: y as i32,
                    z: 0,
                })
                .image_extent(vk::Extent3D {
                    width: bitmap.width,
                    height: bitmap.height,
                    depth: 1,
                }),
        );
        self.bytes.extend_from_slice(&bitmap.rgba);
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

    /// Uploads glyphs added since the last flush, in a submission ordered before
    /// the surfaces that sample them.
    pub fn flush(&mut self, gpu: &Gpu) -> Result {
        if self.regions.is_empty() {
            return Ok(());
        }
        let device = &gpu.device;
        gpu.wait(self.fence)?;
        let needed = self.bytes.len() as u64;
        if self
            .staging
            .as_ref()
            .is_none_or(|staging| staging.size < needed)
        {
            if let Some(old) = self.staging.take() {
                // SAFETY: the fence wait above retired the last upload using it.
                unsafe { old.destroy(device) };
            }
            self.staging = Some(gpu.buffer(
                needed.next_power_of_two().max(1 << 16),
                vk::BufferUsageFlags::TRANSFER_SRC,
            )?);
        }
        let staging = self.staging.as_ref().unwrap();
        staging.write(0, &self.bytes);
        // SAFETY: the command buffer is idle (fence waited) and records on this thread.
        unsafe {
            device.begin_command_buffer(
                self.command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            // Earlier surface renders sampling the atlas finish before it is written.
            let to_transfer = barrier(
                self.image.image,
                (
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::AccessFlags::SHADER_READ,
                ),
                (
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::AccessFlags::TRANSFER_WRITE,
                ),
            );
            device.cmd_pipeline_barrier(
                self.command,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_transfer],
            );
            device.cmd_copy_buffer_to_image(
                self.command,
                staging.buffer,
                self.image.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &self.regions,
            );
            Self::to_shader(device, self.command, self.image.image);
            device.end_command_buffer(self.command)?;
        }
        gpu.submit(self.command, self.fence)?;
        self.bytes.clear();
        self.regions.clear();
        Ok(())
    }

    /// # Safety
    /// The device must be idle.
    pub unsafe fn destroy(&mut self, gpu: &Gpu, pool: vk::CommandPool) {
        // SAFETY: forwarded to the caller.
        unsafe {
            if let Some(staging) = self.staging.take() {
                staging.destroy(&gpu.device);
            }
            self.image.destroy(&gpu.device);
            gpu.device.destroy_fence(self.fence, None);
            gpu.device.free_command_buffers(pool, &[self.command]);
        }
    }
}

/// The atlas with the rasterizer that fills it, as a quad layout glyph source.
pub struct Source<'a> {
    pub atlas: &'a mut Atlas,
    pub glyphs: &'a mut Glyphs,
}

impl GlyphSource for Source<'_> {
    fn glyph(&mut self, key: GlyphKey) -> Option<AtlasGlyph> {
        if let Some(&entry) = self.atlas.map.get(&key) {
            return entry;
        }
        let entry = self.atlas.insert(self.glyphs, key);
        // A failed insert because the atlas is full is retried after the reset.
        if !self.atlas.full {
            self.atlas.map.insert(key, entry);
        }
        entry
    }
}
