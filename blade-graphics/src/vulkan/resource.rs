use ash::vk;
use gpu_alloc_ash::AshMemoryDevice;
use std::ptr;

struct Allocation {
    memory: vk::DeviceMemory,
    offset: u64,
    data: *mut u8,
    handle: usize,
}

impl super::Context {
    fn allocate_memory(
        &self,
        requirements: vk::MemoryRequirements,
        memory: crate::Memory,
    ) -> Allocation {
        let mut manager = self.memory.lock().unwrap();
        let alloc_usage = match memory {
            crate::Memory::Device => gpu_alloc::UsageFlags::FAST_DEVICE_ACCESS,
            crate::Memory::Shared => {
                gpu_alloc::UsageFlags::HOST_ACCESS
                    | gpu_alloc::UsageFlags::DOWNLOAD
                    | gpu_alloc::UsageFlags::UPLOAD
                    | gpu_alloc::UsageFlags::FAST_DEVICE_ACCESS
            }
            crate::Memory::Upload => {
                gpu_alloc::UsageFlags::HOST_ACCESS | gpu_alloc::UsageFlags::UPLOAD
            }
        };
        let memory_types = requirements.memory_type_bits & manager.valid_ash_memory_types;
        let mut block = unsafe {
            manager
                .allocator
                .alloc(
                    AshMemoryDevice::wrap(&self.device),
                    gpu_alloc::Request {
                        size: requirements.size,
                        align_mask: requirements.alignment - 1,
                        usage: alloc_usage,
                        memory_types,
                    },
                )
                .unwrap()
        };

        let data = match memory {
            crate::Memory::Device => ptr::null_mut(),
            crate::Memory::Shared | crate::Memory::Upload => unsafe {
                block
                    .map(
                        AshMemoryDevice::wrap(&self.device),
                        0,
                        requirements.size as usize,
                    )
                    .unwrap()
                    .as_ptr()
            },
        };
        Allocation {
            memory: *block.memory(),
            offset: block.offset(),
            data,
            handle: manager.slab.insert(block),
        }
    }

    fn free_memory(&self, handle: usize) {
        let mut manager = self.memory.lock().unwrap();
        let block = manager.slab.remove(handle);
        unsafe {
            manager
                .allocator
                .dealloc(AshMemoryDevice::wrap(&self.device), block);
        }
    }

    pub fn create_buffer(&self, desc: crate::BufferDesc) -> super::Buffer {
        use vk::BufferUsageFlags as Buf;
        let vk_info = vk::BufferCreateInfo::builder()
            .size(desc.size)
            .usage(
                Buf::TRANSFER_SRC
                    | Buf::TRANSFER_DST
                    | Buf::STORAGE_BUFFER
                    | Buf::INDEX_BUFFER
                    | Buf::VERTEX_BUFFER
                    | Buf::INDIRECT_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        let raw = unsafe { self.device.create_buffer(&vk_info, None).unwrap() };
        let requirements = unsafe { self.device.get_buffer_memory_requirements(raw) };
        let allocation = self.allocate_memory(requirements, desc.memory);

        unsafe {
            self.device
                .bind_buffer_memory(raw, allocation.memory, allocation.offset)
                .unwrap()
        };
        if !desc.name.is_empty() {
            self.set_object_name(vk::ObjectType::BUFFER, raw, desc.name);
        }

        super::Buffer {
            raw,
            memory_handle: allocation.handle,
            mapped_data: allocation.data,
        }
    }

    pub fn destroy_buffer(&self, buffer: super::Buffer) {
        unsafe { self.device.destroy_buffer(buffer.raw, None) };
        self.free_memory(buffer.memory_handle);
    }

    pub fn create_texture(&self, desc: crate::TextureDesc) -> super::Texture {
        let mut create_flags = vk::ImageCreateFlags::empty();
        if desc.dimension == crate::TextureDimension::D2
            && desc.size.depth % 6 == 0
            //&& desc.sample_count == 1
            && desc.size.width == desc.size.height
        {
            create_flags |= vk::ImageCreateFlags::CUBE_COMPATIBLE;
        }

        let format_info = super::describe_format(desc.format);
        let vk_info = vk::ImageCreateInfo::builder()
            .flags(create_flags)
            .image_type(map_texture_dimension(desc.dimension))
            .format(format_info.raw)
            .extent(super::map_extent_3d(&desc.size))
            .mip_levels(desc.mip_level_count)
            .array_layers(desc.array_layer_count)
            .samples(vk::SampleCountFlags::from_raw(1)) // desc.sample_count
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(map_texture_usage(desc.usage, format_info.aspects))
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        let raw = unsafe { self.device.create_image(&vk_info, None).unwrap() };
        let requirements = unsafe { self.device.get_image_memory_requirements(raw) };
        let allocation = self.allocate_memory(requirements, crate::Memory::Device);

        unsafe {
            self.device
                .bind_image_memory(raw, allocation.memory, allocation.offset)
                .unwrap()
        };
        if !desc.name.is_empty() {
            self.set_object_name(vk::ObjectType::IMAGE, raw, desc.name);
        }

        super::Texture {
            raw,
            memory_handle: allocation.handle,
            format: desc.format,
        }
    }

    pub fn destroy_texture(&self, texture: super::Texture) {
        unsafe { self.device.destroy_image(texture.raw, None) };
        self.free_memory(texture.memory_handle);
    }

    pub fn create_texture_view(&self, desc: crate::TextureViewDesc) -> super::TextureView {
        let format_info = super::describe_format(desc.format);
        let subresource_range =
            super::map_subresource_range(desc.subresources, format_info.aspects);
        let vk_info = vk::ImageViewCreateInfo::builder()
            .image(desc.texture.raw)
            .view_type(map_view_dimension(desc.dimension))
            .format(format_info.raw)
            .subresource_range(subresource_range);

        let raw = unsafe { self.device.create_image_view(&vk_info, None).unwrap() };
        if !desc.name.is_empty() {
            self.set_object_name(vk::ObjectType::IMAGE_VIEW, raw, desc.name);
        }

        super::TextureView { raw }
    }

    pub fn destroy_texture_view(&self, view: super::TextureView) {
        unsafe { self.device.destroy_image_view(view.raw, None) };
    }

    pub fn create_sampler(&self, desc: crate::SamplerDesc) -> super::Sampler {
        let mut vk_info = vk::SamplerCreateInfo::builder()
            .mag_filter(map_filter_mode(desc.mag_filter))
            .min_filter(map_filter_mode(desc.min_filter))
            .mipmap_mode(map_mip_filter_mode(desc.mipmap_filter))
            .address_mode_u(map_address_mode(desc.address_modes[0]))
            .address_mode_v(map_address_mode(desc.address_modes[1]))
            .address_mode_w(map_address_mode(desc.address_modes[2]))
            .min_lod(desc.lod_min_clamp);

        if let Some(lod) = desc.lod_max_clamp {
            vk_info = vk_info.max_lod(lod);
        }
        if let Some(fun) = desc.compare {
            vk_info = vk_info
                .compare_enable(true)
                .compare_op(super::map_comparison(fun));
        }
        if desc.anisotropy_clamp > 1 {
            vk_info = vk_info
                .anisotropy_enable(true)
                .max_anisotropy(desc.anisotropy_clamp as f32);
        }
        if let Some(color) = desc.border_color {
            vk_info = vk_info.border_color(map_border_color(color));
        }

        let raw = unsafe { self.device.create_sampler(&vk_info, None).unwrap() };
        if !desc.name.is_empty() {
            self.set_object_name(vk::ObjectType::SAMPLER, raw, desc.name);
        }

        super::Sampler { raw }
    }

    pub fn destroy_sampler(&self, sampler: super::Sampler) {
        unsafe { self.device.destroy_sampler(sampler.raw, None) };
    }
}

fn map_texture_dimension(dimension: crate::TextureDimension) -> vk::ImageType {
    match dimension {
        crate::TextureDimension::D1 => vk::ImageType::TYPE_1D,
        crate::TextureDimension::D2 => vk::ImageType::TYPE_2D,
        crate::TextureDimension::D3 => vk::ImageType::TYPE_3D,
    }
}

fn map_view_dimension(dimension: crate::TextureViewDimension) -> vk::ImageViewType {
    use crate::TextureViewDimension as Tvd;
    match dimension {
        Tvd::D1 => vk::ImageViewType::TYPE_1D,
        Tvd::D2 => vk::ImageViewType::TYPE_2D,
        Tvd::D2Array => vk::ImageViewType::TYPE_2D_ARRAY,
        Tvd::Cube => vk::ImageViewType::CUBE,
        Tvd::CubeArray => vk::ImageViewType::CUBE_ARRAY,
        Tvd::D3 => vk::ImageViewType::TYPE_3D,
    }
}

fn map_texture_usage(
    usage: crate::TextureUsage,
    aspects: super::FormatAspects,
) -> vk::ImageUsageFlags {
    use vk::ImageUsageFlags as Iuf;

    let mut flags = Iuf::empty();
    if usage.contains(crate::TextureUsage::COPY) {
        flags |= Iuf::TRANSFER_SRC | Iuf::TRANSFER_DST;
    }
    if usage.contains(crate::TextureUsage::RESOURCE) {
        flags |= vk::ImageUsageFlags::SAMPLED;
    }
    if usage.contains(crate::TextureUsage::TARGET) {
        flags |= if aspects.contains(super::FormatAspects::COLOR) {
            vk::ImageUsageFlags::COLOR_ATTACHMENT
        } else {
            vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
        };
    }
    if usage.intersects(crate::TextureUsage::STORAGE) {
        flags |= vk::ImageUsageFlags::STORAGE;
    }
    flags
}

fn map_filter_mode(mode: crate::FilterMode) -> vk::Filter {
    match mode {
        crate::FilterMode::Nearest => vk::Filter::NEAREST,
        crate::FilterMode::Linear => vk::Filter::LINEAR,
    }
}

fn map_mip_filter_mode(mode: crate::FilterMode) -> vk::SamplerMipmapMode {
    match mode {
        crate::FilterMode::Nearest => vk::SamplerMipmapMode::NEAREST,
        crate::FilterMode::Linear => vk::SamplerMipmapMode::LINEAR,
    }
}

fn map_address_mode(mode: crate::AddressMode) -> vk::SamplerAddressMode {
    match mode {
        crate::AddressMode::ClampToEdge => vk::SamplerAddressMode::CLAMP_TO_EDGE,
        crate::AddressMode::Repeat => vk::SamplerAddressMode::REPEAT,
        crate::AddressMode::MirrorRepeat => vk::SamplerAddressMode::MIRRORED_REPEAT,
        crate::AddressMode::ClampToBorder => vk::SamplerAddressMode::CLAMP_TO_BORDER,
        // wgt::AddressMode::MirrorClamp => vk::SamplerAddressMode::MIRROR_CLAMP_TO_EDGE,
    }
}

fn map_border_color(border_color: crate::TextureColor) -> vk::BorderColor {
    match border_color {
        crate::TextureColor::TransparentBlack => vk::BorderColor::FLOAT_TRANSPARENT_BLACK,
        crate::TextureColor::OpaqueBlack => vk::BorderColor::FLOAT_OPAQUE_BLACK,
        crate::TextureColor::White => vk::BorderColor::FLOAT_OPAQUE_WHITE,
    }
}