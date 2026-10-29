//! The glyph atlases shared by every terminal surface: a coverage page
//! (`R8_UNORM`) and a color page (`R8G8B8A8_UNORM`), each shelf-packed, filled
//! on demand and uploaded in its own submission ahead of the surfaces that
//! sample it.
//!
//! Coverage glyphs are by far the common case, so keeping them in an `R8` page
//! costs a quarter of the bytes an `RGBA8` atlas spent on the same pixels, and
//! halves the upload traffic for the first paint of a character.

use std::collections::HashMap;

use aegle::app::ash::vk;

use super::gpu::{Buffer, Gpu, Image, barrier};
use crate::{
    Result,
    glyphs::{Bitmap, GlyphKey, Glyphs},
    quads::{AtlasGlyph, GlyphSource},
};

/// Transparent texels between glyphs.
const GAP: u32 = 1;

/// Places `width × height` on a shelf-packed page of `side`² and returns its
/// origin. `shelf` is the next free x, the shelf's top and its height.
fn pack(shelf: &mut (u32, u32, u32), side: u32, width: u32, height: u32) -> Option<(u32, u32)> {
    let (w, h) = (width.checked_add(GAP)?, height.checked_add(GAP)?);
    if w > side || h > side {
        return None;
    }
    let (mut x, mut y, mut height_of_shelf) = *shelf;
    if x + w > side {
        (x, y, height_of_shelf) = (0, y + height_of_shelf, 0);
    }
    if y + h > side {
        return None;
    }
    *shelf = (x + w, y, height_of_shelf.max(h));
    Some((x, y))
}

/// One page image with its shelf allocator and the entries queued for upload.
struct Page {
    image: Image,
    side: u32,
    shelf: (u32, u32, u32),
    /// Pixels queued since the last submission, and where they go.
    bytes: Vec<u8>,
    regions: Vec<vk::BufferImageCopy>,
    staging: Option<Buffer>,
}

impl Page {
    fn new(gpu: &Gpu, side: u32, format: vk::Format) -> Result<Self> {
        let image = gpu.image(
            format,
            [side, side],
            vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
        )?;
        Ok(Self {
            image,
            side,
            shelf: (0, 0, 0),
            bytes: Vec::new(),
            regions: Vec::new(),
            staging: None,
        })
    }

    /// Queues `bitmap` at `(x, y)`; the caller keeps the region inside the page.
    fn queue(&mut self, bitmap: &Bitmap, x: u32, y: u32) {
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
        self.bytes.extend_from_slice(&bitmap.data);
    }

    /// Keeps the staging buffer at the queued size and writes this page's pixels.
    fn prepare(&mut self, gpu: &Gpu) -> Result {
        let needed = self.bytes.len() as u64;
        if self
            .staging
            .as_ref()
            .is_none_or(|staging| staging.size < needed)
        {
            if let Some(old) = self.staging.take() {
                // SAFETY: the atlas fence was waited, retiring the last upload.
                unsafe { old.destroy(&gpu.device) };
            }
            self.staging = Some(gpu.buffer(
                needed.next_power_of_two().max(1 << 16),
                vk::BufferUsageFlags::TRANSFER_SRC,
            )?);
        }
        let staging = self.staging.as_ref().unwrap();
        staging.write(0, &self.bytes);
        self.bytes.clear();
        Ok(())
    }

    /// Forgets every glyph; their texels are overwritten by later uploads.
    fn reset(&mut self) {
        self.shelf = (0, 0, 0);
        self.bytes.clear();
        self.regions.clear();
    }
}

/// The renderer's glyph cache: one coverage page, one color page and the keys
/// already uploaded.
pub struct Atlas {
    mask: Page,
    color: Page,
    map: HashMap<GlyphKey, Option<AtlasGlyph>>,
    command: vk::CommandBuffer,
    fence: vk::Fence,
    /// A glyph did not fit; the caller resets and lays out again.
    pub full: bool,
}

impl Atlas {
    /// Creates a cleared coverage page of `side`² and a color page of a quarter
    /// of that extent, ready for sampling.
    pub fn new(gpu: &Gpu, pool: vk::CommandPool, side: u32) -> Result<Self> {
        let mask = Page::new(gpu, side, vk::Format::R8_UNORM)?;
        // Color glyphs are rare and large; a quarter page holds hundreds.
        let color_side = (side / 2).max(512).min(side);
        let color = Page::new(gpu, color_side, vk::Format::R8G8B8A8_UNORM)?;
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
            for page in [&mask, &color] {
                let to_transfer = barrier(
                    page.image.image,
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
                    page.image.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &vk::ClearColorValue::default(),
                    &[super::gpu::COLOR_RANGE],
                );
                Self::to_shader(device, command, page.image.image);
            }
            device.end_command_buffer(command)?;
        }
        gpu.submit(command, fence)?;
        // Waiting without a reset leaves the fence signaled, as `flush` expects when idle.
        // SAFETY: the fence belongs to this device.
        unsafe { gpu.device.wait_for_fences(&[fence], true, u64::MAX)? };
        Ok(Self {
            mask,
            color,
            map: HashMap::new(),
            command,
            fence,
            full: false,
        })
    }

    /// The coverage page sampled for mask glyphs (`R8_UNORM`).
    pub fn mask_view(&self) -> vk::ImageView {
        self.mask.image.view
    }

    /// The color page sampled for color glyphs (`R8G8B8A8_UNORM`).
    pub fn color_view(&self) -> vk::ImageView {
        self.color.image.view
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
        self.mask.reset();
        self.color.reset();
        self.full = false;
    }

    fn insert(&mut self, glyphs: &mut Glyphs, key: GlyphKey) -> Option<AtlasGlyph> {
        let bitmap = glyphs.rasterize(key)?;
        let page = if bitmap.color {
            &mut self.color
        } else {
            &mut self.mask
        };
        let Some((x, y)) = pack(&mut page.shelf, page.side, bitmap.width, bitmap.height) else {
            self.full = true;
            return None;
        };
        page.queue(&bitmap, x, y);
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
        if self.mask.regions.is_empty() && self.color.regions.is_empty() {
            return Ok(());
        }
        let device = &gpu.device;
        gpu.wait(self.fence)?;
        self.mask.prepare(gpu)?;
        self.color.prepare(gpu)?;
        // SAFETY: the command buffer is idle (fence waited) and records on this thread.
        unsafe {
            device.begin_command_buffer(
                self.command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            for page in [&self.mask, &self.color] {
                if page.regions.is_empty() {
                    continue;
                }
                // Earlier surface renders sampling the page finish before it is written.
                let to_transfer = barrier(
                    page.image.image,
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
                    page.staging.as_ref().unwrap().buffer,
                    page.image.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &page.regions,
                );
                Self::to_shader(device, self.command, page.image.image);
            }
            device.end_command_buffer(self.command)?;
        }
        gpu.submit(self.command, self.fence)?;
        self.mask.regions.clear();
        self.color.regions.clear();
        Ok(())
    }

    /// # Safety
    /// The device must be idle.
    pub unsafe fn destroy(&mut self, gpu: &Gpu, pool: vk::CommandPool) {
        // SAFETY: forwarded to the caller.
        unsafe {
            for page in [&mut self.mask, &mut self.color] {
                if let Some(staging) = page.staging.take() {
                    staging.destroy(&gpu.device);
                }
                page.image.destroy(&gpu.device);
            }
            gpu.device.destroy_fence(self.fence, None);
            gpu.device.free_command_buffers(pool, &[self.command]);
        }
    }
}

/// The atlases with the rasterizer that fills them, as a quad layout glyph source.
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

#[cfg(test)]
mod tests {
    use super::pack;

    #[test]
    fn shelves_fill_a_page_and_report_overflow() {
        let (mut shelf, side) = ((0, 0, 0), 16);
        // Two 7×7 glyphs plus a texel gap each fit side by side on one shelf.
        assert_eq!(pack(&mut shelf, side, 7, 7), Some((0, 0)));
        assert_eq!(pack(&mut shelf, side, 7, 7), Some((8, 0)));
        assert_eq!(shelf, (16, 0, 8));
        // The next glyph starts a shelf below the tallest one.
        assert_eq!(pack(&mut shelf, side, 7, 7), Some((0, 8)));
        // A glyph taller than the page never fits, and one wider than the page
        // is rejected before a shelf is opened.
        assert_eq!(pack(&mut shelf, side, 7, 20), None);
        assert_eq!(pack(&mut shelf, side, 20, 7), None);
        // Filling the page leaves no room.
        let (mut shelf, side) = ((0, 0, 0), 8);
        assert_eq!(pack(&mut shelf, side, 3, 3), Some((0, 0)));
        assert_eq!(pack(&mut shelf, side, 3, 3), Some((4, 0)));
        assert_eq!(pack(&mut shelf, side, 3, 3), Some((0, 4)));
        assert_eq!(pack(&mut shelf, side, 3, 3), Some((4, 4)));
        assert_eq!(pack(&mut shelf, side, 3, 3), None);
    }
}
