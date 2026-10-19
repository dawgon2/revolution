//! NVDEC pictures kept on the GPU (fork 8.6c / XPS item 18): each decoded picture is copied, on
//! the GPU, into memory the compositor's Vulkan device allocated and exported
//! ([`filmcraft_gpu::interop::alloc_cuda_shared`]) and CUDA imported (`cuImportExternalMemory`),
//! and handed over as a [`SharedPicture`] the compositor copies into its plane textures on the
//! same GPU. Before, every NVDEC picture was read back to the CPU and uploaded again (the XPS 16's
//! RTX 4050 dropped 99.7 % of 6.2K pictures that way). CUDA can't export its own memory as a
//! DMA-BUF on consumer GPUs, so the memory comes from Vulkan.
//!
//! Anything that fails (no shared target, an import or copy refused) leaves the decoder reading
//! pictures back as before; nothing here panics. The CUDA driver API is loaded from
//! `libcuda.so.1` (signatures from NVIDIA's public Driver API reference; no toolkit).

use std::ffi::c_void;
use std::os::fd::IntoRawFd;
use std::sync::Arc;

use filmcraft_frame::{GpuFormat, PixelData};
use filmcraft_gpu::shared_picture::SharedPicture;

/// `CUDA_EXTERNAL_MEMORY_HANDLE_DESC` (cuda.h): type, the handle union (an fd in its first four
/// bytes on Linux), size, flags, reserved.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct ExternalMemoryHandleDesc {
    pub kind: u32,
    pub handle: [u64; 2],
    pub size: u64,
    pub flags: u32,
    pub reserved: [u32; 16],
}

/// `CUDA_EXTERNAL_MEMORY_BUFFER_DESC` (cuda.h).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct ExternalMemoryBufferDesc {
    pub offset: u64,
    pub size: u64,
    pub flags: u32,
    pub reserved: [u32; 16],
}

/// `CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD`.
const HANDLE_OPAQUE_FD: u32 = 1;
/// `CUDA_EXTERNAL_MEMORY_DEDICATED`.
const DEDICATED: u32 = 1;
/// GPU memory one decoder's slots may take (pictures in flight at once: decoded, queued, cached,
/// being drawn), and the bounds on their number; past it, pictures are read back.
const SLOTS_BYTES: u64 = 1 << 30;
/// All decoders' slots together (playback, an export and a proxy job decode at once; the XPS 16's
/// RTX 4050 has 6 GB, and 1.5 GB a decoder with several decoders ran it out of memory at 6.2K,
/// which turned GPU exports off for the session). 1 GB a decoder (13 pictures at 6.2K: XPS 18 B3,
/// 6 were too few for playback) and 1.5 GB in all.
const ALL_SLOTS_BYTES: u64 = 3 << 29;
static ALLOCATED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const MIN_SLOTS: u64 = 4;
const MAX_SLOTS: u64 = 48;

type Init = unsafe extern "C" fn(u32) -> i32;
type DeviceGet = unsafe extern "C" fn(*mut i32, i32) -> i32;
type Retain = unsafe extern "C" fn(*mut *mut c_void, i32) -> i32;
type Release = unsafe extern "C" fn(i32) -> i32;
type Push = unsafe extern "C" fn(*mut c_void) -> i32;
type Pop = unsafe extern "C" fn(*mut *mut c_void) -> i32;
type Import = unsafe extern "C" fn(*mut *mut c_void, *const ExternalMemoryHandleDesc) -> i32;
type MapBuffer = unsafe extern "C" fn(*mut u64, *mut c_void, *const ExternalMemoryBufferDesc) -> i32;
type DestroyExternal = unsafe extern "C" fn(*mut c_void) -> i32;
type MemFree = unsafe extern "C" fn(u64) -> i32;
type CopyDtoD = unsafe extern "C" fn(u64, u64, usize) -> i32;
type CopyDtoH = unsafe extern "C" fn(*mut c_void, u64, usize) -> i32;
type Synchronize = unsafe extern "C" fn() -> i32;

/// The CUDA driver and the first device's retained primary context (NVDEC's: device pointers
/// from its mappings are valid here).
struct Cuda {
    context: *mut c_void,
    ordinal: i32,
    release: Release,
    push: Push,
    pop: Pop,
    import: Import,
    map_buffer: MapBuffer,
    destroy_external: DestroyExternal,
    mem_free: MemFree,
    copy_dtod: CopyDtoD,
    copy_dtoh: CopyDtoH,
    synchronize: Synchronize,
    _library: libloading::Library,
}

// SAFETY: the CUDA driver API may be called from any thread; every call below pushes the retained
// primary context on the calling thread first and pops it after, so no thread-affine state is
// shared. The context handle and function pointers are immutable after `load`.
unsafe impl Send for Cuda {}
// SAFETY: as for Send: `&Cuda` only reads immutable handles and calls thread-safe driver entry points.
unsafe impl Sync for Cuda {}

impl Cuda {
    fn load() -> Result<Cuda, String> {
        // SAFETY: symbols from NVIDIA's CUDA driver (signatures per the public Driver API
        // reference); the library is kept in the returned value for as long as any pointer is used.
        unsafe {
            let library = libloading::Library::new("libcuda.so.1").map_err(|e| format!("no NVIDIA CUDA driver: {e}"))?;
            let sym = |name: &[u8]| -> Result<*const c_void, String> { library.get::<*const c_void>(name).map(|s| *s).map_err(|e| e.to_string()) };
            let init: Init = std::mem::transmute::<*const c_void, Init>(sym(b"cuInit\0")?);
            let get: DeviceGet = std::mem::transmute::<*const c_void, DeviceGet>(sym(b"cuDeviceGet\0")?);
            let retain: Retain = std::mem::transmute::<*const c_void, Retain>(sym(b"cuDevicePrimaryCtxRetain\0")?);
            let cuda =
                |release, push, pop, import, map_buffer, destroy_external, mem_free, copy_dtod, copy_dtoh, synchronize, context, ordinal, library| Cuda {
                    context,
                    ordinal,
                    release,
                    push,
                    pop,
                    import,
                    map_buffer,
                    destroy_external,
                    mem_free,
                    copy_dtod,
                    copy_dtoh,
                    synchronize,
                    _library: library,
                };
            let release: Release = std::mem::transmute::<*const c_void, Release>(sym(b"cuDevicePrimaryCtxRelease_v2\0")?);
            let push: Push = std::mem::transmute::<*const c_void, Push>(sym(b"cuCtxPushCurrent_v2\0")?);
            let pop: Pop = std::mem::transmute::<*const c_void, Pop>(sym(b"cuCtxPopCurrent_v2\0")?);
            let import: Import = std::mem::transmute::<*const c_void, Import>(sym(b"cuImportExternalMemory\0")?);
            let map_buffer: MapBuffer = std::mem::transmute::<*const c_void, MapBuffer>(sym(b"cuExternalMemoryGetMappedBuffer\0")?);
            let destroy_external: DestroyExternal = std::mem::transmute::<*const c_void, DestroyExternal>(sym(b"cuDestroyExternalMemory\0")?);
            let mem_free: MemFree = std::mem::transmute::<*const c_void, MemFree>(sym(b"cuMemFree_v2\0")?);
            let copy_dtod: CopyDtoD = std::mem::transmute::<*const c_void, CopyDtoD>(sym(b"cuMemcpyDtoD_v2\0")?);
            let copy_dtoh: CopyDtoH = std::mem::transmute::<*const c_void, CopyDtoH>(sym(b"cuMemcpyDtoH_v2\0")?);
            let synchronize: Synchronize = std::mem::transmute::<*const c_void, Synchronize>(sym(b"cuCtxSynchronize\0")?);
            if init(0) != 0 {
                return Err("cuInit failed".into());
            }
            let mut ordinal = 0;
            if get(&mut ordinal, 0) != 0 {
                return Err("no CUDA device".into());
            }
            let mut context = std::ptr::null_mut();
            if retain(&mut context, ordinal) != 0 || context.is_null() {
                return Err("cuDevicePrimaryCtxRetain failed".into());
            }
            Ok(cuda(release, push, pop, import, map_buffer, destroy_external, mem_free, copy_dtod, copy_dtoh, synchronize, context, ordinal, library))
        }
    }

    /// Run `f` with the context current on this thread.
    fn with<T>(&self, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        // SAFETY: pushes the retained primary context (alive while `self` is); popped below on
        // this same thread whatever `f` returns.
        if unsafe { (self.push)(self.context) } != 0 {
            return Err("cuCtxPushCurrent failed".into());
        }
        let r = f();
        let mut popped = std::ptr::null_mut();
        // SAFETY: balances the push above on the same thread; the out-pointer is a local.
        unsafe { (self.pop)(&mut popped) };
        r
    }
}

impl Drop for Cuda {
    fn drop(&mut self) {
        // SAFETY: releases the retain made in `load`, once; every slot (holding an Arc of this)
        // has already freed its CUDA objects.
        unsafe { (self.release)(self.ordinal) };
    }
}

/// One picture's worth of memory: allocated by the compositor's device, imported into CUDA.
struct Slot {
    cuda: Arc<Cuda>,
    external: *mut c_void,
    pointer: u64,
    size: u64,
    /// The allocation for another device of the same GPU (the export renderer): a duplicate fd,
    /// whether it is dedicated, a key, and the slot's liveness for that device's import cache.
    fd: Arc<std::os::fd::OwnedFd>,
    dedicated: bool,
    key: u64,
    alive: Arc<()>,
    /// Bytes reserved against [`ALL_SLOTS_BYTES`] (given back on drop).
    reserved: u64,
    /// Dropped after the CUDA objects are freed (`Drop` runs before the fields drop).
    buffer: wgpu::Buffer,
}

// SAFETY: the external-memory handle and device pointer are plain driver handles used only
// through `Cuda::with` (thread-safe driver calls with the context pushed); wgpu::Buffer is Send.
unsafe impl Send for Slot {}
// SAFETY: as for Send: shared access only copies out of / reads the handles.
unsafe impl Sync for Slot {}

impl Drop for Slot {
    fn drop(&mut self) {
        ALLOCATED.fetch_sub(self.reserved, std::sync::atomic::Ordering::Relaxed);
        let (pointer, external) = (self.pointer, self.external);
        let cuda = Arc::clone(&self.cuda);
        let _ = cuda.with(|| {
            // SAFETY: the mapped pointer and external memory were made by this slot, freed once,
            // pointer first as the Driver API requires; no copy into them is running (copies are
            // synchronous and a slot drops only when no picture holds it).
            unsafe {
                (cuda.mem_free)(pointer);
                (cuda.destroy_external)(external);
            }
            Ok(())
        });
    }
}

impl Slot {
    fn new(cuda: &Arc<Cuda>, device: &wgpu::Device, size: u64) -> Result<Slot, String> {
        use std::sync::atomic::Ordering;
        // reserve against the process-wide budget first; given back on every failure and on drop
        let reserved = ALLOCATED.try_update(Ordering::Relaxed, Ordering::Relaxed, |a| a.checked_add(size).filter(|n| *n <= ALL_SLOTS_BYTES));
        if reserved.is_err() {
            return Err("NVDEC's shared GPU memory is at its budget".into());
        }
        let made = Self::make(cuda, device, size);
        if made.is_err() {
            ALLOCATED.fetch_sub(size, Ordering::Relaxed);
        }
        made
    }

    fn make(cuda: &Arc<Cuda>, device: &wgpu::Device, size: u64) -> Result<Slot, String> {
        let shared = filmcraft_gpu::interop::alloc_cuda_shared(device, size)?;
        let keep_fd = Arc::new(shared.fd.try_clone().map_err(|e| format!("duplicating the shared memory's fd: {e}"))?);
        let fd = shared.fd.into_raw_fd();
        let desc = ExternalMemoryHandleDesc {
            kind: HANDLE_OPAQUE_FD,
            handle: [u64::from(fd as u32), 0],
            size: shared.size,
            flags: if shared.dedicated { DEDICATED } else { 0 },
            reserved: [0; 16],
        };
        cuda.with(|| {
            let mut external = std::ptr::null_mut();
            // SAFETY: a valid descriptor naming an opaque fd we own; on success CUDA owns the fd.
            let st = unsafe { (cuda.import)(&mut external, &desc) };
            if st != 0 || external.is_null() {
                // SAFETY: the import failed, so the fd is still ours: close it once.
                drop(unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) });
                return Err(format!("cuImportExternalMemory failed ({st})"));
            }
            let buf = ExternalMemoryBufferDesc { offset: 0, size: shared.size, flags: 0, reserved: [0; 16] };
            let mut pointer = 0u64;
            // SAFETY: maps the whole imported range (offset 0, its size) into a local out-pointer.
            let st = unsafe { (cuda.map_buffer)(&mut pointer, external, &buf) };
            if st != 0 || pointer == 0 {
                // SAFETY: destroys the import made above, once.
                unsafe { (cuda.destroy_external)(external) };
                return Err(format!("cuExternalMemoryGetMappedBuffer failed ({st})"));
            }
            static KEYS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let key = KEYS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Slot {
                cuda: Arc::clone(cuda),
                external,
                pointer,
                size: shared.size,
                fd: Arc::clone(&keep_fd),
                dedicated: shared.dedicated,
                key,
                alive: Arc::new(()),
                reserved: size,
                buffer: shared.buffer.clone(),
            })
        })
    }

    /// Copy `bytes` from NVDEC's mapped picture at `source` into this slot, finished on return.
    fn fill(&self, source: u64, bytes: usize) -> Result<(), String> {
        if u64::try_from(bytes).map_or(true, |b| b > self.size) {
            return Err("an NVDEC picture larger than its shared slot".into());
        }
        source.checked_add(u64::try_from(bytes).map_err(|_| "copy size overflows")?).ok_or("NVDEC address overflows")?;
        self.cuda.with(|| {
            // SAFETY: both ranges are device memory of this context: the slot's mapping (at least
            // `bytes`, checked above) and NVDEC's mapped picture (`bytes` = its mapped layout).
            let st = unsafe { (self.cuda.copy_dtod)(self.pointer, source, bytes) };
            if st != 0 {
                return Err(format!("cuMemcpyDtoD failed ({st})"));
            }
            // SAFETY: waits for the copy, so the compositor never reads a half-written picture.
            let st = unsafe { (self.cuda.synchronize)() };
            if st != 0 {
                return Err(format!("cuCtxSynchronize failed ({st})"));
            }
            Ok(())
        })
    }

    /// The slot's first `out.len()` bytes back to memory (the CPU paths).
    fn read(&self, out: &mut [u8]) -> Result<(), String> {
        if u64::try_from(out.len()).map_or(true, |n| n > self.size) {
            return Err("a read past the shared slot".into());
        }
        self.cuda.with(|| {
            // SAFETY: writes `out.len()` bytes (within the slot, checked above) into a live slice.
            let st = unsafe { (self.cuda.copy_dtoh)(out.as_mut_ptr().cast(), self.pointer, out.len()) };
            if st == 0 { Ok(()) } else { Err(format!("cuMemcpyDtoH failed ({st})")) }
        })
    }
}

/// GPU memory NVENC's input pictures may take (fork 8.6d D2), apart from NVDEC's slots.
const ENCODE_BYTES: u64 = 512 << 20;
static ENCODE_ALLOCATED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Fork 8.6d D2: memory an export renderer's device allocated and CUDA imported, for NVENC to
/// encode in place (`nvEncRegisterResource` with the CUDA pointer): the renderer copies the
/// converted planes into [`EncodeBuffer::buffer`] on the GPU, so a frame never crosses to the CPU.
pub(crate) struct EncodeBuffer {
    slot: Slot,
    reserved: u64,
}

impl EncodeBuffer {
    /// `size` bytes on `device` (an NVIDIA device of NVENC's CUDA context), or why not.
    pub(crate) fn new(device: &wgpu::Device, size: u64) -> Result<EncodeBuffer, String> {
        use std::sync::atomic::Ordering;
        static CUDA: std::sync::OnceLock<Result<Arc<Cuda>, String>> = std::sync::OnceLock::new();
        let cuda = CUDA.get_or_init(|| Cuda::load().map(Arc::new)).clone()?;
        let reserved = ENCODE_ALLOCATED.try_update(Ordering::Relaxed, Ordering::Relaxed, |a| a.checked_add(size).filter(|n| *n <= ENCODE_BYTES));
        if reserved.is_err() {
            return Err("NVENC's shared GPU memory is at its budget".into());
        }
        match Slot::make(&cuda, device, size) {
            // (the slot's own NVDEC reservation is zero: this budget is given back on drop)
            Ok(mut slot) => {
                slot.reserved = 0;
                Ok(EncodeBuffer { slot, reserved: size })
            }
            Err(e) => {
                ENCODE_ALLOCATED.fetch_sub(size, Ordering::Relaxed);
                Err(e)
            }
        }
    }

    /// The CUDA device pointer of the memory (NVENC's context: the primary one).
    pub(crate) fn pointer(&self) -> u64 {
        self.slot.pointer
    }

    /// The memory as a buffer of the device that made it.
    pub(crate) fn buffer(&self) -> &wgpu::Buffer {
        &self.slot.buffer
    }

    /// A key per allocation.
    pub(crate) fn key(&self) -> u64 {
        self.slot.key
    }

    /// The first `out.len()` bytes back to memory (the CPU paths).
    pub(crate) fn read(&self, out: &mut [u8]) -> Result<(), String> {
        self.slot.read(out)
    }
}

impl Drop for EncodeBuffer {
    fn drop(&mut self) {
        ENCODE_ALLOCATED.fetch_sub(self.reserved, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The slots of one decoder, reused once no picture holds them.
pub(crate) struct Pool {
    cuda: Arc<Cuda>,
    device: wgpu::Device,
    slots: Vec<Arc<Slot>>,
    size: u64,
}

impl Pool {
    /// A pool for pictures of `size` bytes on the registered compositor device, or why not.
    pub(crate) fn new(size: usize) -> Result<Pool, String> {
        // troubleshooting switch, like FILMCRAFT_NO_GPU_IMPORT: read every picture back
        if std::env::var_os("FILMCRAFT_NO_NVDEC_SHARE").is_some_and(|v| !v.is_empty() && v != "0") {
            return Err("FILMCRAFT_NO_NVDEC_SHARE is set".into());
        }
        let device = filmcraft_gpu::interop::cuda_target().ok_or("no compositor device shares memory with CUDA")?;
        let size = u64::try_from(size).map_err(|_| "picture size overflows")?;
        Ok(Pool { cuda: Arc::new(Cuda::load()?), device, slots: Vec::new(), size })
    }

    /// NVDEC's mapped picture at `source` (`pitch` bytes a row, `bytes` in all, luma then chroma
    /// from `chroma_offset`) as a picture on the GPU; `geometry` describes it. Err: read it back.
    pub(crate) fn picture(
        &mut self,
        source: u64,
        pitch: u32,
        bytes: usize,
        chroma_offset: usize,
        coded: (u32, u32),
        geometry: crate::biplanar::Geometry,
    ) -> Result<filmcraft_frame::VideoFrame, String> {
        if u64::try_from(bytes).map_or(true, |b| b > self.size) {
            return Err("a picture larger than the pool's".into());
        }
        let slot = match self.slots.iter().find(|s| Arc::strong_count(s) == 1) {
            Some(s) => Arc::clone(s),
            None if (self.slots.len() as u64) < (SLOTS_BYTES / self.size.max(1)).clamp(MIN_SLOTS, MAX_SLOTS) => {
                let s = Arc::new(Slot::new(&self.cuda, &self.device, self.size)?);
                self.slots.push(Arc::clone(&s));
                s
            }
            None => return Err("every shared slot is in use".into()),
        };
        slot.fill(source, bytes)?;
        let format = if geometry.bits > 8 { GpuFormat::P010 } else { GpuFormat::Nv12 };
        let read_slot = Arc::clone(&slot);
        let rows = coded.1 as usize;
        let download: filmcraft_gpu::shared_picture::Download = Box::new(move |_w, _h| {
            let mut host = Vec::new();
            host.try_reserve_exact(bytes).map_err(|e| format!("NVDEC read-back allocation failed: {e}"))?;
            host.resize(bytes, 0);
            read_slot.read(&mut host)?;
            let luma = host.get(..chroma_offset).ok_or("NVDEC luma range is invalid")?;
            let chroma = host.get(chroma_offset..).ok_or("NVDEC chroma range is invalid")?;
            crate::biplanar::to_frame(&crate::biplanar::Biplanar { luma, chroma, stride: pitch as usize, rows }, &geometry).map(|f| f.data)
        });
        let keep: Arc<dyn std::any::Any + Send + Sync> = slot.clone();
        let memory = filmcraft_gpu::shared_picture::SharedMemory {
            fd: Arc::clone(&slot.fd),
            size: slot.size,
            dedicated: slot.dedicated,
            key: slot.key,
            alive: Arc::downgrade(&slot.alive),
        };
        let surface = SharedPicture::new(slot.buffer.clone(), self.device.clone(), format, coded, pitch, chroma_offset as u64, keep, download, Some(memory));
        let (_, _, w, h) = geometry.crop;
        Ok(filmcraft_frame::VideoFrame {
            width: w,
            height: h,
            data: PixelData::Gpu(Arc::new(surface)),
            color: geometry.color,
            par: geometry.par,
            pts: filmcraft_time::Tick::ZERO,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// XPS 18 (B3): a decoder of 6.2K 10-bit pictures (78 MB each) had 6 slots and playback holds
    /// more pictures than that in flight, so most were read back (32–77 % dropped on the RTX 4050);
    /// it now has room for 13.
    #[test]
    fn a_decoder_keeps_enough_large_pictures_on_the_gpu() {
        let picture = 12480u64 * 4160 * 3 / 2;
        let slots = (SLOTS_BYTES / picture).clamp(MIN_SLOTS, MAX_SLOTS);
        assert!(slots >= 12, "{slots} slots of 6.2K pictures");
        // one decoder fits the process budget; decode + encode stay within a third of a 6 GB card
        const { assert!(SLOTS_BYTES <= ALL_SLOTS_BYTES) };
        const { assert!(ALL_SLOTS_BYTES + ENCODE_BYTES <= 2 << 30) };
    }

    /// The descriptors as cuda.h lays them out on 64-bit Linux.
    #[test]
    fn descriptors_match_cuda_h() {
        assert_eq!(std::mem::size_of::<ExternalMemoryHandleDesc>(), 104);
        assert_eq!(std::mem::offset_of!(ExternalMemoryHandleDesc, handle), 8);
        assert_eq!(std::mem::offset_of!(ExternalMemoryHandleDesc, size), 24);
        assert_eq!(std::mem::offset_of!(ExternalMemoryHandleDesc, flags), 32);
        assert_eq!(std::mem::size_of::<ExternalMemoryBufferDesc>(), 88);
        assert_eq!(std::mem::offset_of!(ExternalMemoryBufferDesc, flags), 16);
    }
}
