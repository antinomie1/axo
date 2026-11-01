//! Small ash helpers: memory, buffers and color images.

use aegle::app::ash::{self, vk};

use crate::Result;

/// The device handles every helper needs.
pub struct Gpu {
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub memory: vk::PhysicalDeviceMemoryProperties,
}

impl Gpu {
    fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> Result<u32> {
        (0..self.memory.memory_type_count)
            .find(|&i| {
                bits & (1 << i) != 0
                    && self.memory.memory_types[i as usize]
                        .property_flags
                        .contains(flags)
            })
            .ok_or_else(|| "no compatible Vulkan memory type".into())
    }

    /// Allocates and binds memory for `requirements`.
    fn allocate(
        &self,
        requirements: vk::MemoryRequirements,
        flags: vk::MemoryPropertyFlags,
    ) -> Result<vk::DeviceMemory> {
        let info = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(self.memory_type(requirements.memory_type_bits, flags)?);
        // SAFETY: valid device and allocation info.
        Ok(unsafe { self.device.allocate_memory(&info, None)? })
    }

    /// A host-visible, coherent, persistently mapped buffer.
    pub fn buffer(&self, size: u64, usage: vk::BufferUsageFlags) -> Result<Buffer> {
        let device = &self.device;
        // SAFETY: handles are created on this device and destroyed by `Buffer::destroy`.
        unsafe {
            let buffer = device.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )?;
            let memory = self.allocate(
                device.get_buffer_memory_requirements(buffer),
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )?;
            device.bind_buffer_memory(buffer, memory, 0)?;
            let ptr = device.map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())?;
            Ok(Buffer {
                buffer,
                memory,
                size,
                ptr: ptr.cast(),
            })
        }
    }

    /// A device-local 2D color image with a view.
    pub fn image(
        &self,
        format: vk::Format,
        extent: [u32; 2],
        usage: vk::ImageUsageFlags,
    ) -> Result<Image> {
        let device = &self.device;
        // SAFETY: as for `buffer`; destroyed by `Image::destroy`.
        unsafe {
            let image = device.create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(format)
                    .extent(vk::Extent3D {
                        width: extent[0],
                        height: extent[1],
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None,
            )?;
            let memory = self.allocate(
                device.get_image_memory_requirements(image),
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )?;
            device.bind_image_memory(image, memory, 0)?;
            let view = device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(COLOR_RANGE),
                None,
            )?;
            Ok(Image {
                image,
                memory,
                view,
                extent,
            })
        }
    }

    pub fn fence(&self, signaled: bool) -> Result<vk::Fence> {
        let flags = match signaled {
            true => vk::FenceCreateFlags::SIGNALED,
            false => vk::FenceCreateFlags::empty(),
        };
        // SAFETY: valid device.
        Ok(unsafe {
            self.device
                .create_fence(&vk::FenceCreateInfo::default().flags(flags), None)?
        })
    }

    /// Waits for and resets `fence`.
    pub fn wait(&self, fence: vk::Fence) -> Result {
        // SAFETY: the fence belongs to this device.
        unsafe {
            self.device.wait_for_fences(&[fence], true, u64::MAX)?;
            self.device.reset_fences(&[fence])?;
        }
        Ok(())
    }

    /// Submits one command buffer signaling `fence`.
    pub fn submit(&self, command: vk::CommandBuffer, fence: vk::Fence) -> Result {
        let commands = [command];
        let submit = vk::SubmitInfo::default().command_buffers(&commands);
        // SAFETY: the command buffer is recorded and the queue is used on this thread only.
        unsafe { self.device.queue_submit(self.queue, &[submit], fence)? };
        Ok(())
    }
}

pub const COLOR_RANGE: vk::ImageSubresourceRange = vk::ImageSubresourceRange {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    base_mip_level: 0,
    level_count: 1,
    base_array_layer: 0,
    layer_count: 1,
};

pub struct Buffer {
    pub buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    pub size: u64,
    pub ptr: *mut u8,
}

impl Buffer {
    /// # Safety
    /// No submitted work may still use the buffer.
    pub unsafe fn destroy(&self, device: &ash::Device) {
        // SAFETY: forwarded to the caller.
        unsafe {
            device.destroy_buffer(self.buffer, None);
            device.free_memory(self.memory, None);
        }
    }

    /// Copies `bytes` to `offset`; the caller keeps them within `size`.
    pub fn write(&self, offset: usize, bytes: &[u8]) {
        assert!(offset + bytes.len() <= self.size as usize);
        // SAFETY: the mapping covers `size` bytes and the GPU is not reading it.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr.add(offset), bytes.len()) }
    }
}

pub struct Image {
    pub image: vk::Image,
    memory: vk::DeviceMemory,
    pub view: vk::ImageView,
    pub extent: [u32; 2],
}

impl Image {
    /// # Safety
    /// No submitted work may still use the image.
    pub unsafe fn destroy(&self, device: &ash::Device) {
        // SAFETY: forwarded to the caller.
        unsafe {
            device.destroy_image_view(self.view, None);
            device.destroy_image(self.image, None);
            device.free_memory(self.memory, None);
        }
    }
}

/// One image's part of a [`Transfer`].
pub struct Upload<'a> {
    pub image: vk::Image,
    /// Nothing has sampled the image yet, so its contents can be discarded.
    pub fresh: bool,
    /// The pixels, which `regions` address from their start.
    pub bytes: &'a [u8],
    pub regions: &'a [vk::BufferImageCopy],
}

/// A command buffer and staging memory for copying pixels to images, in a
/// submission of its own ahead of the surfaces that sample them.
pub struct Transfer {
    pub command: vk::CommandBuffer,
    pub fence: vk::Fence,
    staging: Option<Buffer>,
}

impl Transfer {
    pub fn new(gpu: &Gpu, pool: vk::CommandPool) -> Result<Self> {
        // SAFETY: one command buffer from the renderer's pool, used on this thread.
        let command = unsafe {
            gpu.device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0]
        };
        Ok(Self {
            command,
            fence: gpu.fence(true)?,
            staging: None,
        })
    }

    /// Waits for the previous submission and opens the command buffer.
    pub fn begin(&self, gpu: &Gpu) -> Result {
        gpu.wait(self.fence)?;
        // SAFETY: the fence was waited, so the command buffer is idle.
        unsafe {
            gpu.device.begin_command_buffer(
                self.command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
        }
        Ok(())
    }

    /// Closes the command buffer and submits it.
    pub fn end(&self, gpu: &Gpu) -> Result {
        // SAFETY: `begin` opened it on this thread.
        unsafe { gpu.device.end_command_buffer(self.command)? };
        gpu.submit(self.command, self.fence)
    }

    /// Whether the last submission finished.
    pub fn is_idle(&self, gpu: &Gpu) -> bool {
        // SAFETY: the fence belongs to this device.
        unsafe { gpu.device.get_fence_status(self.fence) }.unwrap_or(false)
    }

    /// Copies `uploads` to their images and leaves them ready for sampling.
    pub fn upload(&mut self, gpu: &Gpu, uploads: &[Upload<'_>]) -> Result {
        let total: usize = uploads.iter().map(|upload| upload.bytes.len()).sum();
        if total == 0 {
            return Ok(());
        }
        self.begin(gpu)?;
        // The wait in `begin` retired the last upload, and with it the buffer.
        self.reserve(gpu, total as u64)?;
        let staging = self.staging.as_ref().unwrap();
        let device = &gpu.device;
        let mut offset = 0;
        for upload in uploads.iter().filter(|upload| !upload.bytes.is_empty()) {
            staging.write(offset, upload.bytes);
            let (from, stage) = match upload.fresh {
                true => (
                    (vk::ImageLayout::UNDEFINED, vk::AccessFlags::empty()),
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                ),
                // Earlier surface renders finish sampling before it is written.
                false => (
                    (
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                        vk::AccessFlags::SHADER_READ,
                    ),
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                ),
            };
            let to_transfer = barrier(
                upload.image,
                from,
                (
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::AccessFlags::TRANSFER_WRITE,
                ),
            );
            let regions: Vec<_> = upload
                .regions
                .iter()
                .map(|region| vk::BufferImageCopy {
                    buffer_offset: region.buffer_offset + offset as u64,
                    ..*region
                })
                .collect();
            // SAFETY: recording between `begin` and `end`; the regions lie inside
            // the image and the buffer holds the bytes they address.
            unsafe {
                device.cmd_pipeline_barrier(
                    self.command,
                    stage,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[to_transfer],
                );
                device.cmd_copy_buffer_to_image(
                    self.command,
                    staging.buffer,
                    upload.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &regions,
                );
            }
            to_shader(device, self.command, upload.image);
            offset += upload.bytes.len();
        }
        self.end(gpu)
    }

    fn reserve(&mut self, gpu: &Gpu, size: u64) -> Result {
        if self
            .staging
            .as_ref()
            .is_none_or(|staging| staging.size < size)
        {
            self.release(gpu);
            self.staging = Some(gpu.buffer(
                size.next_power_of_two().max(1 << 16),
                vk::BufferUsageFlags::TRANSFER_SRC,
            )?);
        }
        Ok(())
    }

    /// Frees the staging memory; no submission may still read it.
    pub fn release(&mut self, gpu: &Gpu) {
        if let Some(staging) = self.staging.take() {
            // SAFETY: as the caller promises.
            unsafe { staging.destroy(&gpu.device) };
        }
    }

    /// # Safety
    /// The device must be idle.
    pub unsafe fn destroy(&mut self, gpu: &Gpu, pool: vk::CommandPool) {
        self.release(gpu);
        // SAFETY: forwarded to the caller.
        unsafe {
            gpu.device.destroy_fence(self.fence, None);
            gpu.device.free_command_buffers(pool, &[self.command]);
        }
    }
}

/// A copy of `width` × `height` pixels to `(x, y)` from `offset`.
pub fn region(offset: u64, x: u32, y: u32, width: u32, height: u32) -> vk::BufferImageCopy {
    vk::BufferImageCopy::default()
        .buffer_offset(offset)
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
            width,
            height,
            depth: 1,
        })
}

/// Moves an image from transfer writes to fragment shader reads.
pub fn to_shader(device: &ash::Device, command: vk::CommandBuffer, image: vk::Image) {
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

/// A layout transition with its access masks, for one color image.
pub fn barrier(
    image: vk::Image,
    from: (vk::ImageLayout, vk::AccessFlags),
    to: (vk::ImageLayout, vk::AccessFlags),
) -> vk::ImageMemoryBarrier<'static> {
    vk::ImageMemoryBarrier::default()
        .old_layout(from.0)
        .src_access_mask(from.1)
        .new_layout(to.0)
        .dst_access_mask(to.1)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(COLOR_RANGE)
}
