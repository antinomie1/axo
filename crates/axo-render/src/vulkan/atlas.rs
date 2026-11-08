//! The glyph atlases shared by every terminal surface: a coverage page
//! (`R8_UNORM`) and a color page (`R8G8B8A8_UNORM`), each shelf-packed, filled
//! on demand and uploaded through a [`Transfer`].
//!
//! Coverage glyphs are by far the common case, so keeping them in an `R8` page
//! costs a quarter of the bytes an `RGBA8` atlas spent on the same pixels, and
//! halves the upload traffic for the first paint of a character.

use std::{collections::HashMap, sync::Arc};

use aegle::app::ash::vk;
use axo_term::Pixels;

use super::{
    gpu::{Gpu, Image, Transfer, Upload, barrier, region, to_shader},
    textures::Textures,
};
use crate::{
    Result,
    glyphs::{Bitmap, GlyphKey, Glyphs},
    quads::{AtlasGlyph, GlyphSource},
    shelf::pack,
};

/// One page image with its shelf allocator and the entries queued for upload.
struct Page {
    image: Image,
    side: u32,
    shelf: (u32, u32, u32),
    /// Pixels queued since the last submission, and where they go.
    bytes: Vec<u8>,
    regions: Vec<vk::BufferImageCopy>,
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
        })
    }

    /// Queues `bitmap` at `(x, y)`; the caller keeps the region inside the page.
    fn queue(&mut self, bitmap: &Bitmap, x: u32, y: u32) {
        let offset = self.bytes.len() as u64;
        self.regions
            .push(region(offset, x, y, bitmap.width, bitmap.height));
        self.bytes.extend_from_slice(&bitmap.data);
    }

    /// This page's queued pixels, as an upload.
    fn upload(&self) -> Upload<'_> {
        Upload {
            image: self.image.image,
            fresh: false,
            bytes: &self.bytes,
            regions: &self.regions,
        }
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
    transfer: Transfer,
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
        let transfer = Transfer::new(gpu, pool)?;
        transfer.begin(gpu)?;
        let command = transfer.command;
        // SAFETY: recording between `begin` and `end`; the clear finishes before
        // `new` returns.
        unsafe {
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
                to_shader(device, command, page.image.image);
            }
        }
        transfer.end(gpu)?;
        // Waiting without a reset leaves the fence signaled, as `flush` expects when idle.
        // SAFETY: the fence belongs to this device.
        unsafe { device.wait_for_fences(&[transfer.fence], true, u64::MAX)? };
        Ok(Self {
            mask,
            color,
            map: HashMap::new(),
            transfer,
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

    /// Uploads glyphs added since the last flush.
    pub fn flush(&mut self, gpu: &Gpu) -> Result {
        self.transfer
            .upload(gpu, &[self.mask.upload(), self.color.upload()])?;
        for page in [&mut self.mask, &mut self.color] {
            page.bytes.clear();
            page.regions.clear();
        }
        Ok(())
    }

    /// # Safety
    /// The device must be idle.
    pub unsafe fn destroy(&mut self, gpu: &Gpu, pool: vk::CommandPool) {
        // SAFETY: forwarded to the caller.
        unsafe {
            self.transfer.destroy(gpu, pool);
            for page in [&self.mask, &self.color] {
                page.image.destroy(&gpu.device);
            }
        }
    }
}

/// The atlases with the rasterizer that fills them, as a quad layout glyph source.
pub struct Source<'a> {
    pub atlas: &'a mut Atlas,
    pub glyphs: &'a mut Glyphs,
    pub images: &'a mut Textures,
    pub gpu: &'a Gpu,
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

    fn image(&mut self, pixels: &Arc<Pixels>) -> Option<u64> {
        self.images.get(self.gpu, pixels)
    }
}
