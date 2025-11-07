/// External memory sharing module for Vulkan interop
///
/// This module provides helpers for creating and sharing textures between
/// separate WGPU instances using Vulkan external memory extensions.

use anyhow::{Context, Result};
use ash::vk;
use std::os::fd::RawFd;
use std::sync::Arc;
use wgpu::hal::api::Vulkan as VkApi;
use wgpu::hal::vulkan as hal_vulkan;
use wgpu_types::TextureUses;


/// External memory handle representing a shared texture
#[allow(dead_code)]
pub struct ExternalMemoryHandle {
    pub memory_fd: RawFd,
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub format: vk::Format,
    pub extent: vk::Extent3D,
}

/// Bridge texture on the exporting (Smelter) side
pub struct BridgeTextureExport {
    pub wgpu_texture: wgpu::Texture,
    pub external_handle: ExternalMemoryHandle,
    // Keep ash device alive for cleanup
    _device_holder: Arc<ash::Device>,
}

/// Bridge texture on the importing (Window) side
pub struct BridgeTextureImport {
    pub wgpu_texture: wgpu::Texture,
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    // Keep ash device alive for cleanup
    _device_holder: Arc<ash::Device>,
}

/// Check if required external memory extensions are supported
#[allow(dead_code)]
pub fn check_external_memory_support(device: &wgpu::Device) -> Result<()> {
    unsafe {
        device.as_hal::<VkApi, _, _>(|hal_device| {
            let Some(_hal_device) = hal_device else {
                anyhow::bail!("Failed to get Vulkan HAL device");
            };

            // TODO: Check for VK_KHR_external_memory_fd extension
            // This would require accessing the extension list from the physical device
            // For now, we'll assume the extension is available and let it fail at runtime if not

            Ok(())
        })
    }
}

/// Create a bridge texture with external memory support on the exporting device
pub fn create_bridge_texture_export(device: &wgpu::Device, resolution: (u32, u32)) -> Result<BridgeTextureExport> {
    unsafe { device.as_hal::<VkApi, _, _>(|hal_device| {
        let Some(hal_device) = hal_device else {
            anyhow::bail!("Failed to get Vulkan HAL device");
        };

        // Get raw Vulkan device handle
        let vk_device = hal_device.raw_device().clone();
        let vk_device_arc = Arc::new(vk_device.clone());

        // Create image with external memory export capability
        let mut external_memory_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);

        let extent = vk::Extent3D {
            width: resolution.0,
            height: resolution.1,
            depth: 1,
        };

        let image_create_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_SRGB)
            .extent(extent)
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::SAMPLED,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut external_memory_info);

        let image = vk_device
            .create_image(&image_create_info, None)
            .context("Failed to create VkImage with external memory")?;

        // Get memory requirements
        let mem_requirements = vk_device.get_image_memory_requirements(image);

        // Find suitable memory type
        // We need device-local memory with export capability
        let memory_type_index = find_memory_type_index(&mem_requirements, hal_device)
            .context("Failed to find suitable memory type")?;

        // Allocate memory with export capability
        let mut export_memory_info = vk::ExportMemoryAllocateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);

        let allocate_info = vk::MemoryAllocateInfo::default()
            .allocation_size(mem_requirements.size)
            .memory_type_index(memory_type_index)
            .push_next(&mut export_memory_info);

        let memory = vk_device
            .allocate_memory(&allocate_info, None)
            .context("Failed to allocate device memory with export capability")?;

        // Bind memory to image
        vk_device
            .bind_image_memory(image, memory, 0)
            .context("Failed to bind image memory")?;

        // Export file descriptor
        let external_memory_fd = ash::khr::external_memory_fd::Device::new(
            &hal_device.shared_instance().raw_instance(),
            &vk_device,
        );

        let get_fd_info = vk::MemoryGetFdInfoKHR::default()
            .memory(memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);

        let memory_fd = external_memory_fd
            .get_memory_fd(&get_fd_info)
            .context("Failed to export memory file descriptor")?;

        tracing::info!(
            "Exported bridge texture with FD: {}, size: {} MB",
            memory_fd,
            mem_requirements.size / (1024 * 1024)
        );

        // Wrap in WGPU texture
        // Note: We use texture_from_raw with no drop guard since we manage memory externally
        let hal_texture = hal_vulkan::Device::texture_from_raw(
                image,
                &wgpu_hal::TextureDescriptor {
                    label: Some("Bridge Texture Export"),
                    size: wgpu::Extent3d {
                        width: resolution.0,
                        height: resolution.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: TextureUses::COPY_DST
                        | TextureUses::COPY_SRC
                        | TextureUses::RESOURCE,
                    memory_flags: wgpu_hal::MemoryFlags::empty(),
                    view_formats: vec![wgpu::TextureFormat::Rgba8UnormSrgb],
                },
                None, // No drop guard - we manage cleanup
            );

        // Convert HAL texture to wgpu::Texture
        let wgpu_texture = device.create_texture_from_hal::<VkApi>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("Bridge Texture Export"),
                    size: wgpu::Extent3d {
                        width: resolution.0,
                        height: resolution.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
            );

        Ok(BridgeTextureExport {
            wgpu_texture,
            external_handle: ExternalMemoryHandle {
                memory_fd,
                image,
                memory,
                format: vk::Format::R8G8B8A8_SRGB,
                extent,
            },
            _device_holder: vk_device_arc,
        })
    }) }
}

/// Import a bridge texture on the importing device using a file descriptor
pub fn import_bridge_texture(
    device: &wgpu::Device,
    memory_fd: RawFd,
    resolution: (u32, u32),
) -> Result<BridgeTextureImport> {
    unsafe { device.as_hal::<VkApi, _, _>(|hal_device| {
        let Some(hal_device) = hal_device else {
            anyhow::bail!("Failed to get Vulkan HAL device");
        };

        // Get raw Vulkan device handle
        let vk_device = hal_device.raw_device().clone();
        let vk_device_arc = Arc::new(vk_device.clone());

        // Create image with external memory import capability
        let mut external_memory_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);

        let extent = vk::Extent3D {
            width: resolution.0,
            height: resolution.1,
            depth: 1,
        };

        let image_create_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_SRGB)
            .extent(extent)
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut external_memory_info);

        let image = vk_device
            .create_image(&image_create_info, None)
            .context("Failed to create VkImage for import")?;

        // Get memory requirements
        let mem_requirements = vk_device.get_image_memory_requirements(image);

        // Find suitable memory type
        let memory_type_index = find_memory_type_index(&mem_requirements, hal_device)
            .context("Failed to find suitable memory type for import")?;

        // Import memory from file descriptor
        let mut import_fd_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
            .fd(memory_fd);

        let allocate_info = vk::MemoryAllocateInfo::default()
            .allocation_size(mem_requirements.size)
            .memory_type_index(memory_type_index)
            .push_next(&mut import_fd_info);

        let memory = vk_device
            .allocate_memory(&allocate_info, None)
            .context("Failed to import device memory from file descriptor")?;

        tracing::info!(
            "Imported bridge texture from FD: {}, size: {} MB",
            memory_fd,
            mem_requirements.size / (1024 * 1024)
        );

        // Bind memory to image
        vk_device
            .bind_image_memory(image, memory, 0)
            .context("Failed to bind imported image memory")?;

        // Wrap in WGPU texture
        let hal_texture = hal_vulkan::Device::texture_from_raw(
                image,
                &wgpu_hal::TextureDescriptor {
                    label: Some("Bridge Texture Import"),
                    size: wgpu::Extent3d {
                        width: resolution.0,
                        height: resolution.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: TextureUses::RESOURCE
                        | TextureUses::COPY_DST,
                    memory_flags: wgpu_hal::MemoryFlags::empty(),
                    view_formats: vec![wgpu::TextureFormat::Rgba8UnormSrgb],
                },
                None, // No drop guard - we manage cleanup
            );

        // Convert HAL texture to wgpu::Texture
        let wgpu_texture = device.create_texture_from_hal::<VkApi>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("Bridge Texture Import"),
                    size: wgpu::Extent3d {
                        width: resolution.0,
                        height: resolution.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                },
            );

        Ok(BridgeTextureImport {
            wgpu_texture,
            image,
            memory,
            _device_holder: vk_device_arc,
        })
    }) }
}

/// Find a suitable memory type index for device-local memory
fn find_memory_type_index(
    mem_requirements: &vk::MemoryRequirements,
    hal_device: &hal_vulkan::Device,
) -> Result<u32> {
    // Access physical device memory properties
    // Note: This requires accessing wgpu-hal internals
    // We'll try to find DEVICE_LOCAL memory type

    let memory_properties = unsafe {
        hal_device
            .shared_instance()
            .raw_instance()
            .get_physical_device_memory_properties(hal_device.raw_physical_device())
    };

    for i in 0..memory_properties.memory_type_count {
        let memory_type = memory_properties.memory_types[i as usize];

        // Check if this memory type is allowed by requirements
        if (mem_requirements.memory_type_bits & (1 << i)) == 0 {
            continue;
        }

        // Check if it's device-local
        if memory_type
            .property_flags
            .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
        {
            return Ok(i);
        }
    }

    anyhow::bail!("No suitable memory type found");
}

impl Drop for BridgeTextureExport {
    fn drop(&mut self) {
        tracing::info!("Cleaning up bridge texture export");
        unsafe {
            self._device_holder
                .destroy_image(self.external_handle.image, None);
            self._device_holder
                .free_memory(self.external_handle.memory, None);
            // Note: File descriptor is duplicated on export, so we don't close it here
        }
    }
}

impl Drop for BridgeTextureImport {
    fn drop(&mut self) {
        tracing::info!("Cleaning up bridge texture import");
        unsafe {
            self._device_holder.destroy_image(self.image, None);
            self._device_holder.free_memory(self.memory, None);
        }
    }
}
