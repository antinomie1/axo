//! The GPU copies of terminal images, shared by every surface.
//!
//! A texture is a cache entry for [`Pixels`] the terminal owns: made the first
//! time they are visible, and gone soon after the terminal lets go of them,
//! which each render notices through a [`Weak`] reference. So a client that
//! sends a new image per frame leaves nothing behind, and images out of view
//! only cost GPU memory while they fit the budget. A retired texture may still
//! be sampled by a submission in flight, so it waits until every surface is
//! idle, which never stalls a render.

use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

use aegle::app::ash::vk::{self, Handle};
use axo_term::Pixels;

use super::gpu::{Gpu, Image, Transfer, Upload, region};
use crate::Result;

/// The format images are stored in: sampling decodes sRGB to linear.
const FORMAT: vk::Format = vk::Format::R8G8B8A8_SRGB;
/// Textures alive at once, and so the images one frame can show.
const MAX_TEXTURES: u32 = 1024;
/// GPU memory images out of view may keep.
const BUDGET: u64 = 256 << 20;

struct Texture {
    source: Weak<Pixels>,
    image: Image,
    set: vk::DescriptorSet,
    /// The render that last drew it.
    used: u64,
    bytes: u64,
}

pub struct Textures {
    layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    /// By the address of the pixels, which the `Weak` keeps from being reused.
    textures: HashMap<usize, Texture>,
    retired: Vec<Texture>,
    /// Textures waiting for their first upload.
    queued: Vec<(Arc<Pixels>, vk::Image)>,
    transfer: Transfer,
    serial: u64,
    bytes: u64,
}

impl Textures {
    pub fn new(gpu: &Gpu, command_pool: vk::CommandPool) -> Result<Self> {
        let device = &gpu.device;
        let binding = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
        let sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::SAMPLED_IMAGE,
            descriptor_count: MAX_TEXTURES,
        }];
        // SAFETY: plain object creation; `destroy` releases them.
        let (layout, pool) = unsafe {
            (
                device.create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&binding),
                    None,
                )?,
                device.create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
                        .max_sets(MAX_TEXTURES)
                        .pool_sizes(&sizes),
                    None,
                )?,
            )
        };
        Ok(Self {
            layout,
            pool,
            textures: HashMap::new(),
            retired: Vec::new(),
            queued: Vec::new(),
            transfer: Transfer::new(gpu, command_pool)?,
            serial: 0,
            bytes: 0,
        })
    }

    /// The layout of the set [`get`](Self::get) hands out, set 1 of the pipeline.
    pub fn layout(&self) -> vk::DescriptorSetLayout {
        self.layout
    }

    /// Starts a render: retires what the terminals dropped.
    pub fn begin(&mut self) {
        self.serial += 1;
        if self.textures.is_empty() {
            return;
        }
        let dead: Vec<usize> = self
            .textures
            .iter()
            .filter(|(_, texture)| texture.source.strong_count() == 0)
            .map(|(key, _)| *key)
            .collect();
        for key in dead {
            self.retire(key);
        }
    }

    /// The descriptor set of `pixels` as a raw handle, queueing the upload the
    /// first time; `None` when the device cannot hold the image.
    pub fn get(&mut self, gpu: &Gpu, pixels: &Arc<Pixels>) -> Option<u64> {
        let key = Arc::as_ptr(pixels) as usize;
        if let Some(texture) = self.textures.get_mut(&key) {
            texture.used = self.serial;
            return Some(texture.set.as_raw());
        }
        match self.create(gpu, pixels) {
            Ok(texture) => {
                let set = texture.set.as_raw();
                self.queued.push((pixels.clone(), texture.image.image));
                self.bytes += texture.bytes;
                self.textures.insert(key, texture);
                Some(set)
            }
            Err(error) => {
                eprintln!("axo: image texture failed: {error}");
                None
            }
        }
    }

    fn create(&self, gpu: &Gpu, pixels: &Arc<Pixels>) -> Result<Texture> {
        if self.textures.len() + self.retired.len() >= MAX_TEXTURES as usize {
            return Err("too many images".into());
        }
        let device = &gpu.device;
        let image = gpu.image(
            FORMAT,
            [pixels.width, pixels.height],
            vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
        )?;
        let layouts = [self.layout];
        // SAFETY: the pool holds one set per texture, counted above.
        let set = unsafe {
            device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(self.pool)
                    .set_layouts(&layouts),
            )
        };
        let set = match set {
            Ok(sets) => sets[0],
            Err(error) => {
                // SAFETY: the image was never used.
                unsafe { image.destroy(device) };
                return Err(error.into());
            }
        };
        let info = [vk::DescriptorImageInfo::default()
            .image_view(image.view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
            .image_info(&info);
        // SAFETY: a new set no command buffer references yet.
        unsafe { device.update_descriptor_sets(&[write], &[]) };
        Ok(Texture {
            source: Arc::downgrade(pixels),
            image,
            set,
            used: self.serial,
            bytes: pixels.rgba.len() as u64,
        })
    }

    /// Uploads new textures, and evicts the longest unused ones over budget.
    pub fn flush(&mut self, gpu: &Gpu) -> Result {
        let regions: Vec<_> = self
            .queued
            .iter()
            .map(|(pixels, _)| [region(0, 0, 0, pixels.width, pixels.height)])
            .collect();
        let uploads: Vec<_> = self
            .queued
            .iter()
            .zip(&regions)
            .map(|((pixels, image), regions)| Upload {
                image: *image,
                fresh: true,
                bytes: &pixels.rgba,
                regions,
            })
            .collect();
        self.transfer.upload(gpu, &uploads)?;
        if self.queued.is_empty() && self.transfer.is_idle(gpu) {
            // A stream keeps its staging memory; one image does not.
            self.transfer.release(gpu);
        }
        self.queued.clear();
        while self.bytes > BUDGET {
            let oldest = self
                .textures
                .iter()
                .filter(|(_, texture)| texture.used != self.serial)
                .min_by_key(|(_, texture)| texture.used)
                .map(|(key, _)| *key);
            match oldest {
                Some(key) => self.retire(key),
                None => break,
            }
        }
        Ok(())
    }

    fn retire(&mut self, key: usize) {
        if let Some(texture) = self.textures.remove(&key) {
            self.bytes -= texture.bytes;
            self.retired.push(texture);
        }
    }

    /// Destroys retired textures once `idle` says no surface can be sampling
    /// them. Asking is left to the caller, which knows the surfaces.
    pub fn reap(&mut self, gpu: &Gpu, idle: impl FnOnce() -> bool) {
        if self.retired.is_empty() || !self.transfer.is_idle(gpu) || !idle() {
            return;
        }
        for texture in std::mem::take(&mut self.retired) {
            // SAFETY: nothing in flight uses it, as just established.
            unsafe { self.free(gpu, texture) };
        }
    }

    /// # Safety
    /// No submitted work may still use the texture.
    unsafe fn free(&self, gpu: &Gpu, texture: Texture) {
        // SAFETY: forwarded to the caller; the set came from this pool, which
        // allows freeing.
        unsafe {
            let _ = gpu.device.free_descriptor_sets(self.pool, &[texture.set]);
            texture.image.destroy(&gpu.device);
        }
    }

    /// # Safety
    /// The device must be idle.
    pub unsafe fn destroy(&mut self, gpu: &Gpu, command_pool: vk::CommandPool) {
        let textures = std::mem::take(&mut self.textures);
        let retired = std::mem::take(&mut self.retired);
        // SAFETY: forwarded to the caller.
        unsafe {
            for texture in textures.into_values().chain(retired) {
                self.free(gpu, texture);
            }
            self.transfer.destroy(gpu, command_pool);
            gpu.device.destroy_descriptor_pool(self.pool, None);
            gpu.device.destroy_descriptor_set_layout(self.layout, None);
        }
    }
}
