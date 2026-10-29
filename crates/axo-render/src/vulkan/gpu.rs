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
