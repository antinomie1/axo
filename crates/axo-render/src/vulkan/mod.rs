//! The Vulkan renderer: terminals drawn with ash on aegle's shared device.
//!
//! Each surface is an sRGB image registered as an aegle texture. A render
//! submits one command buffer on aegle's queue before aegle's own frame, so the
//! frame that samples the texture sees the finished image. Render pass
//! dependencies order the write after earlier frames' reads and make it visible
//! to later fragment shaders; the atlas upload has its own earlier submission.
#![allow(unsafe_code)]

mod atlas;
mod gpu;

use aegle::{
    app::{SharedDevice, ash::vk},
    ui::scene::{Rect, SceneBuilder, TextureId},
};

use self::{
    atlas::{Atlas, Source},
    gpu::{Buffer, Gpu, Image},
};
use crate::{
    CellSize, FontSpec, Frame, Renderer, Result, SurfaceId,
    glyphs::Glyphs,
    quads::{self, Quad, Quads},
};

const FORMAT: vk::Format = vk::Format::R8G8B8A8_SRGB;

struct Target {
    image: Image,
    framebuffer: vk::Framebuffer,
    texture: TextureId,
}

struct Surface {
    target: Option<Target>,
    command: vk::CommandBuffer,
    fence: vk::Fence,
    instances: Option<Buffer>,
}

pub struct VulkanRenderer {
    shared: SharedDevice,
    gpu: Gpu,
    pool: vk::CommandPool,
    render_pass: vk::RenderPass,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    descriptors: vk::DescriptorPool,
    set: vk::DescriptorSet,
    atlas: Atlas,
    glyphs: Glyphs,
    cell: CellSize,
    surfaces: Vec<Option<Surface>>,
    quads: Quads,
}

fn spirv(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&b| u32::from_le_bytes(b))
        .collect()
}

impl VulkanRenderer {
    /// Creates the pipeline and atlas on aegle's device (`App::vulkan`).
    pub fn new(shared: SharedDevice) -> Result<Self> {
        let raw = shared.raw_device();
        // SAFETY: the physical device belongs to the instance.
        let memory = unsafe {
            raw.instance
                .get_physical_device_memory_properties(raw.physical)
        };
        let gpu = Gpu {
            device: raw.device.clone(),
            queue: raw.queue,
            memory,
        };
        let device = &gpu.device;
        // SAFETY: plain object creation on a live device; everything is destroyed in Drop.
        unsafe {
            let pool = device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                    .queue_family_index(raw.family),
                None,
            )?;
            let attachment = [vk::AttachmentDescription::default()
                .format(FORMAT)
                .samples(vk::SampleCountFlags::TYPE_1)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .final_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            let color = [vk::AttachmentReference::default()
                .attachment(0)
                .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
            let subpass = [vk::SubpassDescription::default()
                .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
                .color_attachments(&color)];
            let dependencies = [
                // Earlier frames sampling the image finish before it is cleared.
                vk::SubpassDependency::default()
                    .src_subpass(vk::SUBPASS_EXTERNAL)
                    .dst_subpass(0)
                    .src_stage_mask(vk::PipelineStageFlags::FRAGMENT_SHADER)
                    .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                    .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE),
                // Later fragment shaders (aegle's frame) see the written image.
                vk::SubpassDependency::default()
                    .src_subpass(0)
                    .dst_subpass(vk::SUBPASS_EXTERNAL)
                    .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                    .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                    .dst_stage_mask(vk::PipelineStageFlags::FRAGMENT_SHADER)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ),
            ];
            let render_pass = device.create_render_pass(
                &vk::RenderPassCreateInfo::default()
                    .attachments(&attachment)
                    .subpasses(&subpass)
                    .dependencies(&dependencies),
                None,
            )?;

            // Binding 0 is the coverage page (R8), binding 1 the color page.
            let bindings = [
                vk::DescriptorSetLayoutBinding::default()
                    .binding(0)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(1)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
            ];
            let set_layout = device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                None,
            )?;
            let set_layouts = [set_layout];
            let layout = device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts),
                None,
            )?;
            let pipeline = Self::pipeline(device, render_pass, layout)?;

            let sizes = [vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLED_IMAGE,
                descriptor_count: 2,
            }];
            let descriptors = device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(1)
                    .pool_sizes(&sizes),
                None,
            )?;
            let set = device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(descriptors)
                    .set_layouts(&set_layouts),
            )?[0];
            let atlas = Atlas::new(&gpu, pool, 1024)?;
            // Shape and rasterize through the device's cache: aegle's own UI
            // text keeps its glyph pixels there too.
            let mut glyphs = Glyphs::new();
            glyphs.share(shared.glyph_cache());
            let renderer = Self {
                shared,
                pool,
                render_pass,
                set_layout,
                layout,
                pipeline,
                descriptors,
                set,
                atlas,
                glyphs,
                cell: CellSize::default(),
                surfaces: Vec::new(),
                quads: Quads::default(),
                gpu,
            };
            renderer.bind_atlas();
            Ok(renderer)
        }
    }

    unsafe fn pipeline(
        device: &aegle::app::ash::Device,
        pass: vk::RenderPass,
        layout: vk::PipelineLayout,
    ) -> Result<vk::Pipeline> {
        let vertex = spirv(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/terminal.vert.spv"
        )));
        let fragment = spirv(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/terminal.frag.spv"
        )));
        // SAFETY: valid SPIR-V from the build script; modules are destroyed after creation.
        unsafe {
            let vs = device
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&vertex), None)?;
            let fs = device.create_shader_module(
                &vk::ShaderModuleCreateInfo::default().code(&fragment),
                None,
            )?;
            let stages = [
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::VERTEX)
                    .module(vs)
                    .name(c"vs_main"),
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::FRAGMENT)
                    .module(fs)
                    .name(c"fs_main"),
            ];
            let bindings = [vk::VertexInputBindingDescription {
                binding: 0,
                stride: size_of::<Quad>() as u32,
                input_rate: vk::VertexInputRate::INSTANCE,
            }];
            let attribute = |location, format, offset| vk::VertexInputAttributeDescription {
                location,
                binding: 0,
                format,
                offset,
            };
            let attributes = [
                attribute(0, vk::Format::R32G32B32A32_SFLOAT, 0),
                attribute(1, vk::Format::R16G16B16A16_UINT, 16),
                attribute(2, vk::Format::R8G8B8A8_UNORM, 24),
                attribute(3, vk::Format::R32_UINT, 28),
            ];
            let input = vk::PipelineVertexInputStateCreateInfo::default()
                .vertex_binding_descriptions(&bindings)
                .vertex_attribute_descriptions(&attributes);
            let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
                .topology(vk::PrimitiveTopology::TRIANGLE_STRIP);
            let viewport = vk::PipelineViewportStateCreateInfo::default()
                .viewport_count(1)
                .scissor_count(1);
            let raster = vk::PipelineRasterizationStateCreateInfo::default()
                .polygon_mode(vk::PolygonMode::FILL)
                .cull_mode(vk::CullModeFlags::NONE)
                .line_width(1.0);
            let multisample = vk::PipelineMultisampleStateCreateInfo::default()
                .rasterization_samples(vk::SampleCountFlags::TYPE_1);
            // Premultiplied source-over.
            let blend = [vk::PipelineColorBlendAttachmentState::default()
                .blend_enable(true)
                .src_color_blend_factor(vk::BlendFactor::ONE)
                .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ONE)
                .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .alpha_blend_op(vk::BlendOp::ADD)
                .color_write_mask(vk::ColorComponentFlags::RGBA)];
            let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend);
            let dynamic = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
            let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic);
            let info = vk::GraphicsPipelineCreateInfo::default()
                .stages(&stages)
                .vertex_input_state(&input)
                .input_assembly_state(&assembly)
                .viewport_state(&viewport)
                .rasterization_state(&raster)
                .multisample_state(&multisample)
                .color_blend_state(&blend)
                .dynamic_state(&dynamic)
                .layout(layout)
                .render_pass(pass)
                .subpass(0);
            let pipeline = device
                .create_graphics_pipelines(vk::PipelineCache::null(), &[info], None)
                .map_err(|(_, error)| error)?[0];
            device.destroy_shader_module(vs, None);
            device.destroy_shader_module(fs, None);
            Ok(pipeline)
        }
    }

    fn bind_atlas(&self) {
        let mask = [vk::DescriptorImageInfo::default()
            .image_view(self.atlas.mask_view())
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let color = [vk::DescriptorImageInfo::default()
            .image_view(self.atlas.color_view())
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(self.set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&mask),
            vk::WriteDescriptorSet::default()
                .dst_set(self.set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&color),
        ];
        // SAFETY: the set is not in use: callers waited for the device or never submitted.
        unsafe { self.gpu.device.update_descriptor_sets(&writes, &[]) };
    }

    fn wait_idle(&self) {
        // SAFETY: the device is live; aegle uses it on this same thread.
        let _ = unsafe { self.gpu.device.device_wait_idle() };
    }

    /// Replaces a surface's image with one of `extent`; the device must be idle.
    fn retarget(&mut self, index: usize, extent: [u32; 2]) -> Result {
        let surface = self.surfaces[index].as_mut().unwrap();
        if let Some(old) = surface.target.take() {
            self.shared.unregister_texture(old.texture);
            // SAFETY: the caller waited for the device.
            unsafe {
                self.gpu.device.destroy_framebuffer(old.framebuffer, None);
                old.image.destroy(&self.gpu.device);
            }
        }
        let image = self.gpu.image(
            FORMAT,
            extent,
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
        )?;
        let views = [image.view];
        // SAFETY: the view is a single-sample 2D sRGB color view of this device with
        // SAMPLED usage; every render leaves it in SHADER_READ_ONLY_OPTIMAL with a
        // dependency to fragment reads, and it is unregistered before destruction
        // only after the device is idle.
        let (framebuffer, texture) = unsafe {
            let framebuffer = self.gpu.device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(self.render_pass)
                    .attachments(&views)
                    .width(extent[0])
                    .height(extent[1])
                    .layers(1),
                None,
            )?;
            (
                framebuffer,
                self.shared.register_texture(image.view, extent)?,
            )
        };
        surface.target = Some(Target {
            image,
            framebuffer,
            texture,
        });
        Ok(())
    }

    fn destroy_surface_now(&mut self, surface: Surface) {
        let device = &self.gpu.device;
        // SAFETY: the caller waited for the device.
        unsafe {
            if let Some(target) = surface.target {
                self.shared.unregister_texture(target.texture);
                device.destroy_framebuffer(target.framebuffer, None);
                target.image.destroy(device);
            }
            if let Some(buffer) = surface.instances {
                buffer.destroy(device);
            }
            device.destroy_fence(surface.fence, None);
            device.free_command_buffers(self.pool, &[surface.command]);
        }
    }
}

impl Renderer for VulkanRenderer {
    fn set_font(&mut self, font: &FontSpec, scale: f32) -> Result<CellSize> {
        self.cell = self.glyphs.set_font(font, scale)?;
        let area = (self.cell.width * self.cell.height * 1024) as f64;
        let side = (area.sqrt() as u32).next_power_of_two().clamp(1024, 4096);
        self.wait_idle();
        // SAFETY: the device is idle.
        unsafe { self.atlas.destroy(&self.gpu, self.pool) };
        self.atlas = Atlas::new(&self.gpu, self.pool, side)?;
        self.bind_atlas();
        Ok(self.cell)
    }

    fn cell_size(&self) -> CellSize {
        self.cell
    }

    fn create_surface(&mut self) -> Result<SurfaceId> {
        // SAFETY: allocation from this renderer's pool.
        let command = unsafe {
            self.gpu.device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(self.pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0]
        };
        let surface = Surface {
            target: None,
            command,
            fence: self.gpu.fence(true)?,
            instances: None,
        };
        let index = match self.surfaces.iter().position(Option::is_none) {
            Some(index) => {
                self.surfaces[index] = Some(surface);
                index
            }
            None => {
                self.surfaces.push(Some(surface));
                self.surfaces.len() - 1
            }
        };
        Ok(SurfaceId(index as u32))
    }

    fn destroy_surface(&mut self, surface: SurfaceId) {
        if let Some(surface) = self
            .surfaces
            .get_mut(surface.0 as usize)
            .and_then(Option::take)
        {
            self.wait_idle();
            self.destroy_surface_now(surface);
        }
    }

    fn render(&mut self, id: SurfaceId, size: [u32; 2], frame: &Frame<'_>) -> Result {
        let index = id.0 as usize;
        if size[0] == 0 || size[1] == 0 || self.cell.width == 0 {
            return Ok(());
        }
        let extent = self.surfaces[index]
            .as_ref()
            .ok_or("unknown surface")?
            .target
            .as_ref()
            .map(|t| t.image.extent);
        if extent != Some(size) {
            // aegle's in-flight frame may still sample the old image.
            self.wait_idle();
            self.retarget(index, size)?;
        }

        let cell = self.cell;
        let mut source = Source {
            atlas: &mut self.atlas,
            glyphs: &mut self.glyphs,
        };
        quads::build(frame, cell, &mut source, &mut self.quads);
        if self.atlas.full {
            self.atlas.reset();
            let mut source = Source {
                atlas: &mut self.atlas,
                glyphs: &mut self.glyphs,
            };
            quads::build(frame, cell, &mut source, &mut self.quads);
        }
        self.atlas.flush(&self.gpu)?;

        let surface = self.surfaces[index].as_mut().unwrap();
        self.gpu.wait(surface.fence)?;
        let count = self.quads.len();
        let bytes = (count.max(1) * size_of::<Quad>()) as u64;
        if surface
            .instances
            .as_ref()
            .is_none_or(|buffer| buffer.size < bytes)
        {
            if let Some(old) = surface.instances.take() {
                // SAFETY: the surface fence was waited: no submission uses it.
                unsafe { old.destroy(&self.gpu.device) };
            }
            surface.instances = Some(self.gpu.buffer(
                bytes.next_power_of_two().max(64 * 1024),
                vk::BufferUsageFlags::VERTEX_BUFFER,
            )?);
        }
        let instances = surface.instances.as_ref().unwrap();
        let (sx, sy) = (2.0 / size[0] as f32, 2.0 / size[1] as f32);
        let dst = instances.ptr.cast::<Quad>();
        for (i, quad) in self.quads.iter().enumerate() {
            let [x, y, w, h] = quad.rect;
            // SAFETY: the mapping holds at least `count` quads and is aligned for them.
            unsafe {
                dst.add(i).write(Quad {
                    rect: [x * sx - 1.0, y * sy - 1.0, w * sx, h * sy],
                    ..*quad
                });
            }
        }

        let target = surface.target.as_ref().unwrap();
        let device = &self.gpu.device;
        let command = surface.command;
        // SAFETY: the command buffer is idle (fence waited); all handles are live.
        unsafe {
            device.begin_command_buffer(
                command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            let clear = [vk::ClearValue {
                color: vk::ClearColorValue { float32: [0.0; 4] },
            }];
            let area = vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent: vk::Extent2D {
                    width: size[0],
                    height: size[1],
                },
            };
            device.cmd_begin_render_pass(
                command,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(self.render_pass)
                    .framebuffer(target.framebuffer)
                    .render_area(area)
                    .clear_values(&clear),
                vk::SubpassContents::INLINE,
            );
            if count > 0 {
                device.cmd_set_viewport(
                    command,
                    0,
                    &[vk::Viewport {
                        x: 0.0,
                        y: 0.0,
                        width: size[0] as f32,
                        height: size[1] as f32,
                        min_depth: 0.0,
                        max_depth: 1.0,
                    }],
                );
                device.cmd_set_scissor(command, 0, &[area]);
                device.cmd_bind_pipeline(command, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
                device.cmd_bind_descriptor_sets(
                    command,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.layout,
                    0,
                    &[self.set],
                    &[],
                );
                device.cmd_bind_vertex_buffers(command, 0, &[instances.buffer], &[0]);
                device.cmd_draw(command, 4, count as u32, 0, 0);
            }
            device.cmd_end_render_pass(command);
            device.end_command_buffer(command)?;
        }
        self.gpu.submit(command, surface.fence)
    }

    fn paint(&self, surface: SurfaceId, builder: &mut SceneBuilder, rect: Rect) {
        let target = self
            .surfaces
            .get(surface.0 as usize)
            .and_then(Option::as_ref)
            .and_then(|s| s.target.as_ref());
        if let Some(target) = target {
            builder.texture(target.texture, rect);
        }
    }
}

impl Drop for VulkanRenderer {
    fn drop(&mut self) {
        self.wait_idle();
        for surface in std::mem::take(&mut self.surfaces).into_iter().flatten() {
            self.destroy_surface_now(surface);
        }
        let device = &self.gpu.device;
        // SAFETY: the device is idle and nothing else references these objects.
        unsafe {
            self.atlas.destroy(&self.gpu, self.pool);
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.layout, None);
            device.destroy_descriptor_pool(self.descriptors, None);
            device.destroy_descriptor_set_layout(self.set_layout, None);
            device.destroy_render_pass(self.render_pass, None);
            device.destroy_command_pool(self.pool, None);
        }
    }
}
