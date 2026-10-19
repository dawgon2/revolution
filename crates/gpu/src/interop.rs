//! Hardware-decoded pictures sampled where the decoder wrote them (Linux, Vulkan; ADR 0002).
//!
//! [`create_device`] makes the wgpu device on a Vulkan adapter with the extensions that import
//! Linux DMA-BUFs (`VK_KHR_external_memory_fd`, `VK_EXT_external_memory_dma_buf`,
//! `VK_EXT_image_drm_format_modifier`). [`Importer`] turns a decoded picture's DMA-BUF layers
//! ([`filmcraft_frame::DmaBuf`]: luma and interleaved chroma) into two wgpu textures over the
//! same memory, which the compositor samples with its biplanar path, with no copy. Imports are
//! cached per decoder surface (decoders reuse a pool of surfaces). [`Importer::import_target`]
//! imports a video encoder's input surface the same way, as two render targets the export writes
//! its Y'CbCr into (fork Phase 5.2c: no read-back, no upload).
//!
//! This is the only module of `filmcraft-gpu` with `unsafe` (Vulkan through `ash` and wgpu-hal),
//! under the rules of ADR 0001: a `// SAFETY:` comment on every block, a safe `Option` / `Result`
//! API, no panics. Without the extensions (other drivers or backends), callers fall back to
//! reading the picture back to memory.

use std::collections::HashMap;
use std::ffi::CStr;
use std::os::fd::IntoRawFd;

use ash::vk;
use filmcraft_frame::{DmaBuf, GpuFormat, GpuSurface};
use wgpu::hal::api::Vulkan;

const EXTENSIONS: [&CStr; 3] = [ash::khr::external_memory_fd::NAME, ash::ext::external_memory_dma_buf::NAME, ash::ext::image_drm_format_modifier::NAME];

/// Imported pictures kept (each holds a reference on its DMA-BUF, which keeps the decoder's
/// memory alive even after the decoder released the surface).
const MAX_IMPORTS: usize = 64;
/// Imports not drawn for this many imports are dropped.
const STALE_AFTER: u64 = 300;

/// A wgpu device on `adapter` with DMA-BUF import enabled, when the adapter is Vulkan and has the
/// extensions (None: it is not, use an ordinary device). `desc` as for `Adapter::request_device`.
pub fn create_device(adapter: &wgpu::Adapter, desc: &wgpu::DeviceDescriptor<'_>) -> Option<Result<(wgpu::Device, wgpu::Queue), String>> {
    // SAFETY: the hal adapter is only used within this function, while `adapter` is alive, and we
    // do not destroy anything it owns.
    let hal = unsafe { adapter.as_hal::<Vulkan>() }?;
    // the import's format queries are Vulkan 1.1 functions (instance and physical device)
    if hal.shared_instance().instance_api_version() < vk::API_VERSION_1_1 || hal.physical_device_capabilities().properties().api_version < vk::API_VERSION_1_1 {
        return None;
    }
    if !EXTENSIONS.iter().all(|e| hal.physical_device_capabilities().supports_extension(e)) {
        return None;
    }
    let cb = Box::new(|args: wgpu::hal::vulkan::CreateDeviceCallbackArgs<'_, '_, '_>| {
        for e in EXTENSIONS {
            if !args.extensions.contains(&e) {
                args.extensions.push(e);
            }
        }
    });
    // SAFETY: the callback only adds extensions the physical device supports (checked above); it
    // removes no extension or feature. Features and limits are the ones wgpu would request.
    let open = unsafe { hal.open_with_callback(desc.required_features, &desc.required_limits, &desc.memory_hints, Some(cb)) };
    let open = match open {
        Ok(o) => o,
        Err(e) => return Some(Err(format!("Vulkan device with DMA-BUF import: {e}"))),
    };
    drop(hal);
    // SAFETY: `open` was created from this adapter's hal adapter, with exactly `desc`'s features.
    Some(unsafe { adapter.create_device_from_hal(open, desc) }.map_err(|e| e.to_string()))
}

/// Whether `device` was made by [`create_device`] (has the import extensions).
pub fn can_import(device: &wgpu::Device) -> bool {
    // SAFETY: only reads the enabled-extension list of the live hal device.
    let Some(hal) = (unsafe { device.as_hal::<Vulkan>() }) else { return false };
    EXTENSIONS.iter().all(|e| hal.enabled_device_extensions().contains(e))
}

/// Largest DMA-BUF [`alloc_dmabuf`] makes (a 16K 4:2:2 16-bit picture is 1 GiB).
const MAX_ALLOC: u64 = 1 << 30;

/// New memory of at least `size` bytes on `device` (made by [`create_device`]), exported as a
/// DMA-BUF another device can import (fork 6.1b: Intel's media engine copies decoded 4:2:2
/// pictures into it, untiled, for this GPU to read). System memory only: host-visible and
/// host-coherent, never a discrete GPU's device-local memory (no resizable-BAR VRAM, which the
/// other device could not reach coherently; an integrated GPU's device-local memory is system
/// memory); cached when the device has such a type. Coherent host memory is snooped by
/// this GPU, so writes the other device has finished (synced on its side) are seen. Refused
/// (`Err`, the caller falls back) when the driver cannot export buffers as DMA-BUFs. The
/// descriptor owns the memory: the Vulkan objects are freed at once, the buffer object lives
/// while the descriptor (or an import of it) does.
pub fn alloc_dmabuf(device: &wgpu::Device, size: u64) -> Result<std::os::fd::OwnedFd, String> {
    if size == 0 || size > MAX_ALLOC {
        return Err(format!("a DMA-BUF of {size} bytes"));
    }
    if !can_import(device) {
        return Err("the device has no DMA-BUF extensions".into());
    }
    // SAFETY: the hal device is used only within this call, while `device` is alive.
    let hal = unsafe { device.as_hal::<Vulkan>() }.ok_or("not a Vulkan device")?;
    let dev = hal.raw_device().clone();
    let instance = hal.shared_instance().raw_instance().clone();
    let pd = hal.raw_physical_device();
    let dma_buf = vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT;
    let usage = vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST;
    // can buffers be exported as DMA-BUFs at all, and must their memory be dedicated?
    let query = vk::PhysicalDeviceExternalBufferInfo::default().usage(usage).handle_type(dma_buf);
    let mut ext_props = vk::ExternalBufferProperties::default();
    // SAFETY: a live instance (Vulkan >= 1.1, checked by `create_device`) and physical device;
    // valid in / out structs.
    unsafe { instance.get_physical_device_external_buffer_properties(pd, &query, &mut ext_props) };
    let features = ext_props.external_memory_properties.external_memory_features;
    if !features.contains(vk::ExternalMemoryFeatureFlags::EXPORTABLE) {
        return Err("the driver cannot export buffers as DMA-BUFs".into());
    }
    let dedicated = features.contains(vk::ExternalMemoryFeatureFlags::DEDICATED_ONLY);
    let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(dma_buf);
    let info = vk::BufferCreateInfo::default().size(size).usage(usage).sharing_mode(vk::SharingMode::EXCLUSIVE).push_next(&mut external);
    // SAFETY: a valid create info on a live device that enabled the external-memory extensions.
    let buffer = unsafe { dev.create_buffer(&info, None) }.map_err(|e| format!("vkCreateBuffer: {e}"))?;
    let destroy_buffer = || {
        // SAFETY: the buffer was created above, never bound to memory or used; destroyed once.
        unsafe { dev.destroy_buffer(buffer, None) }
    };
    // SAFETY: a valid buffer.
    let req = unsafe { dev.get_buffer_memory_requirements(buffer) };
    // SAFETY: a live instance and physical device.
    let props = unsafe { instance.get_physical_device_memory_properties(pd) };
    let types = props.memory_types.get(..(props.memory_type_count as usize).min(32)).unwrap_or(&[]);
    let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    let cached = host | vk::MemoryPropertyFlags::HOST_CACHED;
    // DEVICE_LOCAL is VRAM (resizable BAR) on a discrete GPU, never taken; an integrated GPU's
    // memory is all system memory, marked device-local
    // SAFETY: a live instance and physical device.
    let integrated = unsafe { instance.get_physical_device_properties(pd) }.device_type == vk::PhysicalDeviceType::INTEGRATED_GPU;
    let fits = |i: usize, want: vk::MemoryPropertyFlags| {
        req.memory_type_bits & (1 << i) != 0
            && types
                .get(i)
                .is_some_and(|t| t.property_flags.contains(want) && (integrated || !t.property_flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)))
    };
    let Some(type_index) = (0..types.len()).find(|&i| fits(i, cached)).or_else(|| (0..types.len()).find(|&i| fits(i, host))) else {
        destroy_buffer();
        return Err("no host-coherent system memory type can be exported".into());
    };
    let mut export = vk::ExportMemoryAllocateInfo::default().handle_types(dma_buf);
    let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default().buffer(buffer);
    let mut alloc = vk::MemoryAllocateInfo::default().allocation_size(req.size.max(size)).memory_type_index(type_index as u32).push_next(&mut export);
    if dedicated {
        alloc = alloc.push_next(&mut dedicated_info);
    }
    // SAFETY: a valid allocation of an exportable memory type on a live device (dedicated to
    // `buffer`, alive until after the memory is freed, when the driver requires it).
    let memory = match unsafe { dev.allocate_memory(&alloc, None) } {
        Ok(m) => m,
        Err(e) => {
            destroy_buffer();
            return Err(format!("vkAllocateMemory (exportable): {e}"));
        }
    };
    let fd_api = ash::khr::external_memory_fd::Device::new(&instance, &dev);
    let get = vk::MemoryGetFdInfoKHR::default().memory(memory).handle_type(dma_buf);
    // SAFETY: the memory was allocated above for export as a DMA-BUF; the new fd is ours.
    let fd = unsafe { fd_api.get_memory_fd(&get) };
    // SAFETY: frees the allocation made above, bound to nothing; an exported DMA-BUF keeps the
    // buffer object (its payload) alive on its own.
    unsafe { dev.free_memory(memory, None) };
    destroy_buffer();
    let fd = fd.map_err(|e| format!("vkGetMemoryFdKHR: {e}"))?;
    if fd < 0 {
        return Err("vkGetMemoryFdKHR returned no descriptor".into());
    }
    // SAFETY: a new descriptor returned to us by vkGetMemoryFdKHR, owned from here on.
    Ok(unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) })
}

/// One imported picture: the luma and chroma planes as textures over the decoder's memory.
pub struct Imported {
    /// Dead once the decoder freed the memory: the import is dropped then.
    alive: std::sync::Weak<()>,
    pub luma: wgpu::TextureView,
    pub chroma: wgpu::TextureView,
    last_used: u64,
    _textures: [wgpu::Texture; 2],
}

/// What an imported plane is used for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlaneUse {
    /// Sampled (decoded pictures).
    Sampled,
    /// Rendered into (encoder input). UNORM like the sampled planes: drivers list fewer tiling
    /// modifiers for UINT render targets (ANV has no Y-tiled `R16_UINT`), and a code stored as
    /// `code / 65535` converts back to exactly that code.
    Target,
}

impl PlaneUse {
    fn vk_usage(self) -> vk::ImageUsageFlags {
        match self {
            PlaneUse::Sampled => vk::ImageUsageFlags::SAMPLED,
            PlaneUse::Target => vk::ImageUsageFlags::COLOR_ATTACHMENT,
        }
    }
    fn feature(self) -> vk::FormatFeatureFlags {
        match self {
            PlaneUse::Sampled => vk::FormatFeatureFlags::SAMPLED_IMAGE,
            PlaneUse::Target => vk::FormatFeatureFlags::COLOR_ATTACHMENT,
        }
    }
}

/// Imports decoded pictures into a device made by [`create_device`].
pub struct Importer {
    device: wgpu::Device,
    cache: HashMap<u64, Imported>,
    /// Encoder input surfaces imported as render targets ([`Importer::import_target`]).
    targets: HashMap<u64, Imported>,
    clock: u64,
    /// Imports that failed (they fall back to reading back).
    pub failures: u64,
}

impl Importer {
    /// None when `device` cannot import DMA-BUFs.
    pub fn new(device: &wgpu::Device) -> Option<Importer> {
        can_import(device).then(|| Importer { device: device.clone(), cache: HashMap::new(), targets: HashMap::new(), clock: 0, failures: 0 })
    }

    /// The textures of `surface` (imported on first use, then cached by its memory).
    pub fn import(&mut self, surface: &dyn GpuSurface) -> Option<(&wgpu::TextureView, &wgpu::TextureView)> {
        let made = |this: &Self| {
            surface
                .dmabuf()
                .ok_or_else(|| "the decoder did not export the picture".to_string())
                .and_then(|d| this.import_dmabuf(&d, surface.format(), PlaneUse::Sampled))
        };
        self.cached(false, surface.memory_id(), surface.liveness(), made, "importing a decoded picture into Vulkan failed (reading it back instead)")
    }

    /// A video encoder's input surface (`d`, memory `id`, dead with `alive`) as two render
    /// targets: luma (`R16Unorm` / `R8Unorm`) and interleaved chroma (`Rg16Unorm` / `Rg8Unorm`) of the
    /// surface's full size, which the export writes P010 / NV12 samples into. Cached by `id`.
    /// None: the driver cannot render into this memory (the caller uploads instead). `writable`:
    /// the memory was exported for writing (a read-only export is never rendered into).
    pub fn import_target(
        &mut self,
        id: u64,
        alive: std::sync::Weak<()>,
        d: &DmaBuf,
        format: GpuFormat,
        writable: bool,
    ) -> Option<(&wgpu::TextureView, &wgpu::TextureView)> {
        if !writable {
            log::warn!("an encoder surface exported read-only is not rendered into");
            return None;
        }
        self.cached(
            true,
            id,
            alive,
            |this| this.import_dmabuf(d, format, PlaneUse::Target),
            "importing an encoder surface into Vulkan failed (uploading frames instead)",
        )
    }

    fn cached(
        &mut self,
        target: bool,
        key: u64,
        alive: std::sync::Weak<()>,
        make: impl FnOnce(&Self) -> Result<Imported, String>,
        what: &str,
    ) -> Option<(&wgpu::TextureView, &wgpu::TextureView)> {
        self.clock += 1;
        let clock = self.clock;
        // imports of memory that is gone go at once (they pin it: an export's 64 target
        // surfaces would otherwise stay allocated until the next import)
        for cache in [&mut self.cache, &mut self.targets] {
            cache.retain(|_, v| v.alive.strong_count() > 0);
        }
        let present = if target { self.targets.contains_key(&key) } else { self.cache.contains_key(&key) };
        if !present {
            match make(self) {
                Ok(mut i) => {
                    i.alive = alive;
                    let cache = if target { &mut self.targets } else { &mut self.cache };
                    cache.retain(|_, v| v.last_used + STALE_AFTER > clock && v.alive.strong_count() > 0);
                    if cache.len() >= MAX_IMPORTS
                        && let Some(old) = cache.iter().min_by_key(|(_, v)| v.last_used).map(|(k, _)| *k)
                    {
                        cache.remove(&old);
                    }
                    cache.insert(key, i);
                }
                Err(e) => {
                    self.failures += 1;
                    if self.failures <= 3 {
                        log::warn!("{what}: {e}");
                    }
                    return None;
                }
            }
        }
        let cache = if target { &mut self.targets } else { &mut self.cache };
        let i = cache.get_mut(&key)?;
        i.last_used = clock;
        Some((&i.luma, &i.chroma))
    }

    fn import_dmabuf(&self, d: &DmaBuf, format: GpuFormat, usage: PlaneUse) -> Result<Imported, String> {
        let (fy, fc, wy, wc) = match format {
            GpuFormat::P010 => (vk::Format::R16_UNORM, vk::Format::R16G16_UNORM, wgpu::TextureFormat::R16Unorm, wgpu::TextureFormat::Rg16Unorm),
            GpuFormat::Nv12 => (vk::Format::R8_UNORM, vk::Format::R8G8_UNORM, wgpu::TextureFormat::R8Unorm, wgpu::TextureFormat::Rg8Unorm),
            GpuFormat::Y210 => return self.import_packed(d, usage),
        };
        let (y_fourcc, c_fourcc) = layer_fourccs(format);
        if d.layer_formats != [y_fourcc, c_fourcc] {
            return Err(format!("layer formats {:#x} / {:#x} do not match {format:?}", d.layer_formats[0], d.layer_formats[1]));
        }
        let (w, h) = (d.width.max(1), d.height.max(1));
        let labels = match usage {
            PlaneUse::Sampled => ["decoded luma", "decoded chroma"],
            PlaneUse::Target => ["encoder luma", "encoder chroma"],
        };
        let luma = self.import_plane(d, 0, fy, wy, (w, h), usage, labels[0])?;
        let chroma = self.import_plane(d, 1, fc, wc, (w.div_ceil(2), h.div_ceil(2)), usage, labels[1])?;
        Ok(Imported {
            alive: std::sync::Weak::new(),
            luma: luma.create_view(&Default::default()),
            chroma: chroma.create_view(&Default::default()),
            last_used: 0,
            _textures: [luma, chroma],
        })
    }

    /// Packed 4:2:2 (`Y210`, one layer): the memory as one `Rgba16Unorm` texture of one texel per
    /// two pixels (`Y0 Cb Y1 Cr` → R G B A), sampled only (fork 6.1b: Intel's linear copy, which
    /// [`crate::unpack422`] splits into VRAM planes). Both views of the result are that texture.
    fn import_packed(&self, d: &DmaBuf, usage: PlaneUse) -> Result<Imported, String> {
        if usage != PlaneUse::Sampled {
            return Err("packed 4:2:2 memory is only sampled".into());
        }
        let y210 = u32::from_le_bytes(*b"Y210");
        if d.layer_formats != [y210, 0] || !d.layers[1].is_empty() {
            return Err(format!("layer formats {:#x} / {:#x} are not packed Y210", d.layer_formats[0], d.layer_formats[1]));
        }
        let (w, h) = (d.width.max(1), d.height.max(1));
        // NVIDIA's driver lists no DMA-BUF modifiers for 64-bit texel formats (XPS 16, RTX 4050):
        // the same bytes then go in as `Rg16Unorm` (Y0 Cb) (Y1 Cr) texel pairs, which
        // `unpack422` reads by the view's format
        let packed = self
            .import_plane(d, 0, vk::Format::R16G16B16A16_UNORM, wgpu::TextureFormat::Rgba16Unorm, (w.div_ceil(2), h), usage, "decoded 4:2:2")
            .or_else(|wide| {
                let pairs = w.div_ceil(2).saturating_mul(2);
                self.import_plane(d, 0, vk::Format::R16G16_UNORM, wgpu::TextureFormat::Rg16Unorm, (pairs, h), usage, "decoded 4:2:2")
                    .map_err(|narrow| format!("{wide}; as texel pairs: {narrow}"))
            })?;
        let view = packed.create_view(&Default::default());
        Ok(Imported { alive: std::sync::Weak::new(), luma: view.clone(), chroma: view, last_used: 0, _textures: [packed.clone(), packed] })
    }

    fn import_plane(
        &self,
        d: &DmaBuf,
        layer: usize,
        vk_format: vk::Format,
        format: wgpu::TextureFormat,
        (w, h): (u32, u32),
        usage: PlaneUse,
        label: &'static str,
    ) -> Result<wgpu::Texture, String> {
        let (planes, modifier) = (d.layers.get(layer).ok_or("layer index")?, d.modifier);
        // One memory plane only: a modifier with metadata planes (compression) would need the
        // metadata kept across the layout transition below, which only a foreign-queue acquire
        // guarantees; such memory is read back instead.
        let [first] = planes.as_slice() else {
            return Err(format!("{} memory planes in a layer (only single-plane modifiers are imported)", planes.len()));
        };
        // single-plane modifiers can still be compressed (metadata outside the plane)
        if compressed_modifier(modifier) {
            return Err(format!("format modifier {modifier:#x} is compressed (not imported)"));
        }
        let object = d.objects.get(first.object).ok_or("plane of a missing buffer object")?;
        // the rows must hold the texels, and the plane lie in its buffer (checked against the
        // buffer's real size below): a description claiming otherwise would make the GPU read
        // outside the memory. Untiled (LINEAR) memory is addressed exactly so; tiled layouts are
        // at least as large.
        let texel = u64::from(format.block_copy_size(None).ok_or("a format without a texel size")?);
        let (pitch, offset) = (u64::from(first.pitch), u64::from(first.offset));
        if pitch < u64::from(w).saturating_mul(texel) {
            return Err(format!("row pitch {pitch} is less than {w} texels of {texel} bytes"));
        }
        let plane_end = pitch.checked_mul(u64::from(h)).and_then(|n| n.checked_add(offset)).ok_or("plane size overflows")?;
        // SAFETY: the hal device is used only within this call, while `self.device` is alive.
        let hal = unsafe { self.device.as_hal::<Vulkan>() }.ok_or("not a Vulkan device")?;
        let dev = hal.raw_device().clone();
        // SAFETY: the instance outlives the device; only used for function pointers here.
        let instance = hal.shared_instance().raw_instance().clone();
        if hal.shared_instance().instance_api_version() < vk::API_VERSION_1_1 {
            return Err("Vulkan 1.1 is needed to import".into());
        }
        // creating an image with a modifier the device does not list for the format is invalid usage
        check_modifier(&instance, hal.raw_physical_device(), vk_format, modifier, planes.len(), (w, h), usage)?;
        let fd_api = ash::khr::external_memory_fd::Device::new(&instance, &dev);
        let layouts: Vec<vk::SubresourceLayout> = planes
            .iter()
            .map(|p| vk::SubresourceLayout { offset: p.offset as u64, size: 0, row_pitch: p.pitch as u64, array_pitch: 0, depth_pitch: 0 })
            .collect();
        let mut modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default().drm_format_modifier(modifier).plane_layouts(&layouts);
        let mut external = vk::ExternalMemoryImageCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk_format)
            .extent(vk::Extent3D { width: w, height: h, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(usage.vk_usage())
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut external)
            .push_next(&mut modifier_info);
        // SAFETY: a valid create info (single-plane colour format, DRM modifier tiling with one
        // explicit layout per memory plane) on a live device that enabled the extensions.
        let image = unsafe { dev.create_image(&info, None) }.map_err(|e| format!("vkCreateImage: {e}"))?;
        let destroy_image = |dev: &ash::Device| {
            // SAFETY: the image was created above, is unbound / unused and destroyed once.
            unsafe { dev.destroy_image(image, None) }
        };
        // SAFETY: valid image.
        let req = unsafe { dev.get_image_memory_requirements(image) };
        // our own reference on the buffer (Vulkan takes ownership of the fd only on success), and
        // the buffer's real size from the kernel (DMA-BUFs report it through seeking to the end)
        let fd = match object.try_clone().and_then(|fd| {
            let mut f = std::fs::File::from(fd);
            let size = std::io::Seek::seek(&mut f, std::io::SeekFrom::End(0))?;
            Ok((std::os::fd::OwnedFd::from(f), size))
        }) {
            Ok((fd, size)) if req.size <= size && plane_end <= size => fd,
            Ok((_, size)) => {
                destroy_image(&dev);
                return Err(format!("the image needs {} bytes (plane ends at {plane_end}), the buffer has {size}", req.size));
            }
            Err(e) => {
                destroy_image(&dev);
                return Err(format!("dup: {e}"));
            }
        };
        let mut fd_props = vk::MemoryFdPropertiesKHR::default();
        let raw = std::os::fd::AsRawFd::as_raw_fd(&fd);
        // SAFETY: a valid DMA-BUF fd and out-struct.
        let props = unsafe { fd_api.get_memory_fd_properties(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT, raw, &mut fd_props) };
        if let Err(e) = props {
            destroy_image(&dev);
            return Err(format!("vkGetMemoryFdPropertiesKHR: {e}"));
        }
        let bits = req.memory_type_bits & fd_props.memory_type_bits;
        let Some(type_index) = (0..32).find(|i| bits & (1 << i) != 0) else {
            destroy_image(&dev);
            return Err("no memory type fits the image and the buffer".into());
        };
        let raw_fd = fd.into_raw_fd();
        let mut import = vk::ImportMemoryFdInfoKHR::default().handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT).fd(raw_fd);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let alloc = vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(type_index).push_next(&mut import).push_next(&mut dedicated);
        // SAFETY: imports the DMA-BUF (ownership of `raw_fd` passes to Vulkan on success) as
        // dedicated memory of `image`, with a type both allow.
        let memory = match unsafe { dev.allocate_memory(&alloc, None) } {
            Ok(m) => m,
            Err(e) => {
                // SAFETY: Vulkan did not take the fd; close our duplicate.
                drop(unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(raw_fd) });
                destroy_image(&dev);
                return Err(format!("vkAllocateMemory (DMA-BUF import): {e}"));
            }
        };
        // SAFETY: binds the dedicated memory to its image at offset 0, once.
        if let Err(e) = unsafe { dev.bind_image_memory(image, memory, 0) } {
            // SAFETY: free the memory and image made above, unused.
            unsafe { dev.free_memory(memory, None) };
            destroy_image(&dev);
            return Err(format!("vkBindImageMemory: {e}"));
        }
        let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
        let hal_desc = wgpu::hal::TextureDescriptor {
            label: Some(label),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: match usage {
                PlaneUse::Sampled => wgpu::TextureUses::RESOURCE,
                PlaneUse::Target => wgpu::TextureUses::COLOR_TARGET,
            },
            memory_flags: wgpu::hal::MemoryFlags::empty(),
            view_formats: Vec::new(),
        };
        // SAFETY: `image` was created on this device to match `hal_desc` and has memory bound;
        // with no drop callback and `Dedicated` memory, wgpu-hal destroys both when the texture goes.
        let hal_tex = unsafe { hal.texture_from_raw(image, &hal_desc, None, wgpu::hal::vulkan::TextureMemory::Dedicated(memory)) };
        drop(hal);
        let desc = wgpu::TextureDescriptor {
            label: Some(label),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: match usage {
                PlaneUse::Sampled => wgpu::TextureUsages::TEXTURE_BINDING,
                PlaneUse::Target => wgpu::TextureUsages::RENDER_ATTACHMENT,
            },
            view_formats: &[],
        };
        // SAFETY: the hal texture was made on this device for `desc`. Sampled: its memory already
        // holds the decoded picture (written by the video decoder and synced before export).
        // UNINITIALIZED lets wgpu move it from UNDEFINED to the sampled layout, which keeps the
        // contents of a DMA-BUF image without compression metadata (the decoder's surfaces).
        // Target: every texel is rendered before the encoder reads the memory, and the caller
        // waits for that submission to finish first. That the encoder then sees what was rendered
        // without a release to the foreign queue family is observed driver behaviour (RADV, ANV:
        // uncompressed modifiers, `compressed_modifier` refuses the others, keep no state outside
        // the memory and flush caches at the end of a submission), not a Vulkan guarantee; see
        // ADR 0002 "Encoding" residual risks.
        Ok(unsafe { self.device.create_texture_from_hal::<Vulkan>(hal_tex, &desc, wgpu::TextureUses::UNINITIALIZED) })
    }
}

/// Whether a DRM format modifier means compressed memory (metadata the import cannot keep valid
/// across the layout transition and the hand-over to the video engine): Intel CCS modifiers
/// (`I915_FORMAT_MOD_*_CCS*`, all single-plane from Xe2 on), AMD modifiers with DCC, and AMD GFX12
/// tiling (compression is chosen per allocation there, not shown in the modifier).
fn compressed_modifier(modifier: u64) -> bool {
    let (vendor, code) = (modifier >> 56, modifier & ((1 << 56) - 1));
    match vendor {
        0x01 => matches!(code, 4..=8 | 10..=17),
        0x02 => (code >> 13) & 1 == 1 || code & 0xff == 5,
        _ => false,
    }
}

/// The DRM fourccs of the luma and chroma layers of `format` (`R16` / `GR32`, `R8` / `GR88`;
/// packed `Y210`: its one layer, then 0).
fn layer_fourccs(format: GpuFormat) -> (u32, u32) {
    let fourcc = |c: &[u8; 4]| u32::from_le_bytes(*c);
    match format {
        GpuFormat::P010 => (fourcc(b"R16 "), fourcc(b"GR32")),
        GpuFormat::Nv12 => (fourcc(b"R8  "), fourcc(b"GR88")),
        GpuFormat::Y210 => (fourcc(b"Y210"), 0),
    }
}

/// Whether the device can make a sampled `format` image with DRM `modifier` in `planes` memory
/// planes over imported DMA-BUF memory, `w`×`h` in size (vkCreateImage requires it; the queries
/// are the ones the Vulkan spec prescribes for `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`).
fn check_modifier(
    instance: &ash::Instance,
    pd: vk::PhysicalDevice,
    format: vk::Format,
    modifier: u64,
    planes: usize,
    (w, h): (u32, u32),
    usage: PlaneUse,
) -> Result<(), String> {
    let count = {
        let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
        let mut props = vk::FormatProperties2::default().push_next(&mut list);
        // SAFETY: a live instance (Vulkan ≥ 1.1, checked by the caller) and physical device; the
        // list without an array only receives the count.
        unsafe { instance.get_physical_device_format_properties2(pd, format, &mut props) };
        (list.drm_format_modifier_count as usize).min(1024)
    };
    let mut mods = vec![vk::DrmFormatModifierPropertiesEXT::default(); count];
    let listed = {
        let mut list = vk::DrmFormatModifierPropertiesListEXT::default().drm_format_modifier_properties(&mut mods);
        let mut props = vk::FormatProperties2::default().push_next(&mut list);
        // SAFETY: as above; the list points to `count` writable entries and says so.
        unsafe { instance.get_physical_device_format_properties2(pd, format, &mut props) };
        (list.drm_format_modifier_count as usize).min(count)
    };
    let m = mods
        .get(..listed)
        .and_then(|l| l.iter().find(|m| m.drm_format_modifier == modifier))
        .ok_or_else(|| format!("format modifier {modifier:#x} not supported for {format:?}"))?;
    if m.drm_format_modifier_plane_count as usize != planes {
        return Err(format!("modifier {modifier:#x} has {} memory planes, the export {planes}", m.drm_format_modifier_plane_count));
    }
    if !m.drm_format_modifier_tiling_features.contains(usage.feature()) {
        return Err(format!("modifier {modifier:#x} cannot be used as {format:?} ({usage:?})"));
    }
    let mut drm = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default().drm_format_modifier(modifier).sharing_mode(vk::SharingMode::EXCLUSIVE);
    let mut external = vk::PhysicalDeviceExternalImageFormatInfo::default().handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let info = vk::PhysicalDeviceImageFormatInfo2::default()
        .format(format)
        .ty(vk::ImageType::TYPE_2D)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(usage.vk_usage())
        .push_next(&mut external)
        .push_next(&mut drm);
    let mut external_props = vk::ExternalImageFormatProperties::default();
    let mut out = vk::ImageFormatProperties2::default().push_next(&mut external_props);
    // SAFETY: valid chained in / out structs on a live instance and physical device.
    unsafe { instance.get_physical_device_image_format_properties2(pd, &info, &mut out) }.map_err(|e| format!("modifier {modifier:#x} for {format:?}: {e}"))?;
    let max = out.image_format_properties.max_extent;
    if w > max.width || h > max.height {
        return Err(format!("{w}x{h} exceeds the device's {}x{} for this format", max.width, max.height));
    }
    if !external_props.external_memory_properties.external_memory_features.contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE) {
        return Err("DMA-BUF memory is not importable for this image".into());
    }
    Ok(())
}

// ------------------------------------------------------- memory shared with CUDA (fork 8.6c)

/// Device memory of a GPU shared with CUDA on the same GPU (fork 8.6c / XPS item 18): NVDEC copies
/// each decoded picture into it on the GPU, and the compositor copies it into its plane textures
/// on the GPU, so the picture never crosses to the CPU. (CUDA can't export its own memory as a
/// DMA-BUF on consumer GPUs: the XPS 16's RTX 4050 reports no DMA-BUF support.)
pub struct CudaShared {
    /// The memory as an opaque fd, for CUDA's `cuImportExternalMemory` (which takes it over).
    pub fd: std::os::fd::OwnedFd,
    /// The same memory as a buffer of the device that made it (a copy source and destination).
    pub buffer: wgpu::Buffer,
    pub size: u64,
    /// The memory is a dedicated allocation (CUDA's import must say so).
    pub dedicated: bool,
}

/// [`CudaShared`] of `size` bytes in `device`'s own memory (VRAM on a discrete GPU). Needs the
/// external-memory extension ([`create_device`]) and a driver that exports opaque fds.
pub fn alloc_cuda_shared(device: &wgpu::Device, size: u64) -> Result<CudaShared, String> {
    if size == 0 || size > MAX_ALLOC {
        return Err(format!("CUDA-shared memory of {size} bytes"));
    }
    if !can_import(device) {
        return Err("the device has no external-memory extensions".into());
    }
    // SAFETY: the hal device is used only within this call, while `device` is alive.
    let hal = unsafe { device.as_hal::<Vulkan>() }.ok_or("not a Vulkan device")?;
    let dev = hal.raw_device().clone();
    let instance = hal.shared_instance().raw_instance().clone();
    let pd = hal.raw_physical_device();
    let opaque = vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD;
    let usage = vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST;
    let query = vk::PhysicalDeviceExternalBufferInfo::default().usage(usage).handle_type(opaque);
    let mut ext_props = vk::ExternalBufferProperties::default();
    // SAFETY: a live instance (Vulkan >= 1.1, checked by `create_device`) and physical device;
    // valid in / out structs.
    unsafe { instance.get_physical_device_external_buffer_properties(pd, &query, &mut ext_props) };
    let features = ext_props.external_memory_properties.external_memory_features;
    if !features.contains(vk::ExternalMemoryFeatureFlags::EXPORTABLE) {
        return Err("the driver cannot export buffers as opaque fds".into());
    }
    let dedicated = features.contains(vk::ExternalMemoryFeatureFlags::DEDICATED_ONLY);
    let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(opaque);
    let info = vk::BufferCreateInfo::default().size(size).usage(usage).sharing_mode(vk::SharingMode::EXCLUSIVE).push_next(&mut external);
    // SAFETY: a valid create info on a live device that enabled the external-memory extensions.
    let buffer = unsafe { dev.create_buffer(&info, None) }.map_err(|e| format!("vkCreateBuffer: {e}"))?;
    let destroy_buffer = || {
        // SAFETY: the buffer was created above and is not in use; destroyed once, on a failure path.
        unsafe { dev.destroy_buffer(buffer, None) }
    };
    // SAFETY: a valid buffer.
    let req = unsafe { dev.get_buffer_memory_requirements(buffer) };
    // SAFETY: a live instance and physical device.
    let props = unsafe { instance.get_physical_device_memory_properties(pd) };
    let types = props.memory_types.get(..(props.memory_type_count as usize).min(32)).unwrap_or(&[]);
    let local = vk::MemoryPropertyFlags::DEVICE_LOCAL;
    let Some(type_index) = (0..types.len()).find(|&i| req.memory_type_bits & (1 << i) != 0 && types.get(i).is_some_and(|t| t.property_flags.contains(local)))
    else {
        destroy_buffer();
        return Err("no device-local memory type can be exported".into());
    };
    let mut export = vk::ExportMemoryAllocateInfo::default().handle_types(opaque);
    let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default().buffer(buffer);
    let alloc_size = req.size.max(size);
    let mut alloc = vk::MemoryAllocateInfo::default().allocation_size(alloc_size).memory_type_index(type_index as u32).push_next(&mut export);
    if dedicated {
        alloc = alloc.push_next(&mut dedicated_info);
    }
    // SAFETY: a valid allocation of an exportable memory type on a live device (dedicated to
    // `buffer`, alive until the memory is freed, when the driver requires it).
    let memory = match unsafe { dev.allocate_memory(&alloc, None) } {
        Ok(m) => m,
        Err(e) => {
            destroy_buffer();
            return Err(format!("vkAllocateMemory (exportable): {e}"));
        }
    };
    let free_both = || {
        // SAFETY: failure path: the buffer and memory were made above, unused; each freed once.
        unsafe {
            dev.destroy_buffer(buffer, None);
            dev.free_memory(memory, None);
        }
    };
    // SAFETY: binds the fresh buffer to the fresh allocation, which is large enough (`req`).
    if let Err(e) = unsafe { dev.bind_buffer_memory(buffer, memory, 0) } {
        free_both();
        return Err(format!("vkBindBufferMemory: {e}"));
    }
    let fd_api = ash::khr::external_memory_fd::Device::new(&instance, &dev);
    let get = vk::MemoryGetFdInfoKHR::default().memory(memory).handle_type(opaque);
    // SAFETY: the memory was allocated above for export as an opaque fd; the new fd is ours.
    let fd = match unsafe { fd_api.get_memory_fd(&get) } {
        Ok(fd) if fd >= 0 => fd,
        Ok(_) => {
            free_both();
            return Err("vkGetMemoryFdKHR returned no descriptor".into());
        }
        Err(e) => {
            free_both();
            return Err(format!("vkGetMemoryFdKHR: {e}"));
        }
    };
    // SAFETY: a new descriptor returned to us by vkGetMemoryFdKHR, owned from here on.
    let fd = unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    // SAFETY: `buffer` was made on this device with `usage` (COPY_SRC / COPY_DST in wgpu terms)
    // and bound to `memory` (offset 0, `alloc_size` bytes); wgpu owns and frees both from here on
    // (managed), and never maps them (device-local, not created mappable).
    let hal_buffer = unsafe { wgpu::hal::vulkan::Buffer::from_raw_managed(buffer, memory, 0, alloc_size) };
    let desc = wgpu::BufferDescriptor {
        label: Some("CUDA-shared picture"),
        size,
        usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    };
    // SAFETY: the hal buffer was made from this device's raw device, respecting `desc`
    // (size within the allocation, usage as created), bound and therefore initialised, not empty.
    let buffer = unsafe { device.create_buffer_from_hal::<Vulkan>(hal_buffer, &desc) };
    Ok(CudaShared { fd, buffer, size, dedicated })
}

/// The compositor's device CUDA copies decoded pictures for (fork 8.6c): set by the app when its
/// window draws on an NVIDIA GPU with the external-memory extensions; NVDEC then hands
/// [`crate::shared_picture::SharedPicture`]s over instead of reading pictures back.
static CUDA_TARGET: std::sync::RwLock<Option<wgpu::Device>> = std::sync::RwLock::new(None);

/// Register (Some) or clear (None) the device CUDA copies pictures for.
pub fn set_cuda_target(device: Option<wgpu::Device>) {
    *CUDA_TARGET.write().unwrap_or_else(std::sync::PoisonError::into_inner) = device.filter(can_import);
}

/// The device [`set_cuda_target`] registered.
pub fn cuda_target() -> Option<wgpu::Device> {
    CUDA_TARGET.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// Whether two wgpu devices are the same Vulkan device (by their raw handles).
pub fn same_device(a: &wgpu::Device, b: &wgpu::Device) -> bool {
    // SAFETY: the hal devices are only read for their handles within this call, while both live.
    let raw = |d: &wgpu::Device| unsafe { d.as_hal::<Vulkan>() }.map(|h| ash::vk::Handle::as_raw(h.raw_device().handle()));
    matches!((raw(a), raw(b)), (Some(x), Some(y)) if x == y)
}

/// The memory of a [`CudaShared`] allocation (an opaque fd from the device that made it)
/// imported into another device on the same GPU as a buffer of `size` bytes (fork 8.6c: the
/// export renderer's own device copying NVDEC's pictures made for the window's). `fd` is consumed.
pub fn import_cuda_shared(device: &wgpu::Device, fd: std::os::fd::OwnedFd, size: u64, dedicated: bool) -> Result<wgpu::Buffer, String> {
    if size == 0 || size > MAX_ALLOC {
        return Err(format!("CUDA-shared memory of {size} bytes"));
    }
    if !can_import(device) {
        return Err("the device has no external-memory extensions".into());
    }
    // SAFETY: the hal device is used only within this call, while `device` is alive.
    let hal = unsafe { device.as_hal::<Vulkan>() }.ok_or("not a Vulkan device")?;
    let dev = hal.raw_device().clone();
    let instance = hal.shared_instance().raw_instance().clone();
    let pd = hal.raw_physical_device();
    let opaque = vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD;
    let usage = vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST;
    let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(opaque);
    let info = vk::BufferCreateInfo::default().size(size).usage(usage).sharing_mode(vk::SharingMode::EXCLUSIVE).push_next(&mut external);
    // SAFETY: a valid create info on a live device that enabled the external-memory extensions.
    let buffer = unsafe { dev.create_buffer(&info, None) }.map_err(|e| format!("vkCreateBuffer: {e}"))?;
    // SAFETY: a valid buffer.
    let req = unsafe { dev.get_buffer_memory_requirements(buffer) };
    // SAFETY: a live instance and physical device.
    let props = unsafe { instance.get_physical_device_memory_properties(pd) };
    let types = props.memory_types.get(..(props.memory_type_count as usize).min(32)).unwrap_or(&[]);
    let local = vk::MemoryPropertyFlags::DEVICE_LOCAL;
    let Some(type_index) = (0..types.len()).find(|&i| req.memory_type_bits & (1 << i) != 0 && types.get(i).is_some_and(|t| t.property_flags.contains(local)))
    else {
        // SAFETY: the buffer was created above and never used; destroyed once.
        unsafe { dev.destroy_buffer(buffer, None) };
        return Err("no device-local memory type to import into".into());
    };
    let raw = fd.into_raw_fd();
    let mut import = vk::ImportMemoryFdInfoKHR::default().handle_type(opaque).fd(raw);
    let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default().buffer(buffer);
    let alloc_size = req.size.max(size);
    let mut alloc = vk::MemoryAllocateInfo::default().allocation_size(alloc_size).memory_type_index(type_index as u32).push_next(&mut import);
    if dedicated {
        alloc = alloc.push_next(&mut dedicated_info);
    }
    // SAFETY: imports the opaque fd (ownership passes to the driver on success) as memory for the
    // fresh buffer (dedicated to it when the exporter's was).
    let memory = match unsafe { dev.allocate_memory(&alloc, None) } {
        Ok(m) => m,
        Err(e) => {
            // SAFETY: the import failed, so the fd is still ours: close it once; the buffer was
            // never bound or used: destroyed once.
            unsafe {
                drop(<std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(raw));
                dev.destroy_buffer(buffer, None);
            }
            return Err(format!("vkAllocateMemory (import): {e}"));
        }
    };
    // SAFETY: binds the fresh buffer to the imported allocation, which is large enough (`req`).
    if let Err(e) = unsafe { dev.bind_buffer_memory(buffer, memory, 0) } {
        // SAFETY: failure path: both made above, unused; each freed once.
        unsafe {
            dev.destroy_buffer(buffer, None);
            dev.free_memory(memory, None);
        }
        return Err(format!("vkBindBufferMemory: {e}"));
    }
    // SAFETY: made on this device, bound to `memory`; wgpu owns and frees both from here on.
    let hal_buffer = unsafe { wgpu::hal::vulkan::Buffer::from_raw_managed(buffer, memory, 0, alloc_size) };
    let desc = wgpu::BufferDescriptor {
        label: Some("CUDA-shared picture (imported)"),
        size,
        usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    };
    // SAFETY: the hal buffer was made from this device respecting `desc`, bound, not empty.
    Ok(unsafe { device.create_buffer_from_hal::<Vulkan>(hal_buffer, &desc) })
}

#[cfg(test)]
mod tests {
    /// Review of Phase 5.2c: compressed single-plane modifiers are never imported.
    #[test]
    fn compressed_modifiers_are_recognised() {
        let intel = |c: u64| (0x01u64 << 56) | c;
        let amd = |c: u64| (0x02u64 << 56) | c;
        assert!(!super::compressed_modifier(0)); // linear
        assert!(!super::compressed_modifier(intel(2))); // Y-tiled (the iHD encoder's surfaces)
        assert!(!super::compressed_modifier(intel(9))); // 4-tiled
        for c in [4, 6, 7, 8, 10, 13, 16, 17] {
            assert!(super::compressed_modifier(intel(c)), "intel {c}");
        }
        assert!(super::compressed_modifier(amd((1 << 13) | 3))); // GFX10 with DCC
        assert!(!super::compressed_modifier(amd(3))); // GFX10 without
        assert!(super::compressed_modifier(amd(5))); // GFX12
    }
}
