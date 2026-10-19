//! A decoded picture in GPU memory shared with CUDA (fork 8.6c / XPS item 18): NVDEC copied it
//! there on the GPU ([`crate::interop::alloc_cuda_shared`]), and the compositor copies its planes
//! into textures on the same GPU ([`crate::GpuCompositor`]), so on an NVIDIA GPU a picture never
//! crosses to the CPU on its way to the screen. The CPU paths (frame cache, exports on another
//! device) read it back through the decoder's own `download`.

use std::sync::Arc;

use filmcraft_frame::{DmaBuf, GpuFormat, GpuSurface, PixelData};

/// The memory behind a picture, for a compositor on another device of the same GPU to import
/// (an export renderer): a duplicate of the opaque fd, the allocation's size, whether it is a
/// dedicated allocation, a key per allocation (pictures reuse it) and its liveness.
#[derive(Clone)]
pub struct SharedMemory {
    pub fd: Arc<std::os::fd::OwnedFd>,
    pub size: u64,
    pub dedicated: bool,
    pub key: u64,
    pub alive: std::sync::Weak<()>,
}

static DOWNLOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Pictures read back to memory so far (the CPU paths; `perf.stats` `playback.sharedReadBacks`).
pub fn downloads() -> u64 {
    DOWNLOADS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Reads the visible `width` × `height` picture back to memory (the decoder's CUDA copy).
pub type Download = Box<dyn Fn(u32, u32) -> Result<PixelData, String> + Send + Sync>;

/// One picture: biplanar NV12 (8-bit) or P010 (10-bit, MSB-aligned) at `pitch` bytes a row, the
/// chroma plane `chroma_offset` bytes in.
pub struct SharedPicture {
    pub buffer: wgpu::Buffer,
    pub device: wgpu::Device,
    pub format: GpuFormat,
    pub coded: (u32, u32),
    pub pitch: u32,
    pub chroma_offset: u64,
    id: u64,
    alive: Arc<()>,
    /// The memory for another device (None: only this one draws it in place).
    pub memory: Option<SharedMemory>,
    /// The decoder's side (its CUDA mapping of the memory), alive as long as the picture.
    _keep: Arc<dyn std::any::Any + Send + Sync>,
    download: Download,
}

impl SharedPicture {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        buffer: wgpu::Buffer,
        device: wgpu::Device,
        format: GpuFormat,
        coded: (u32, u32),
        pitch: u32,
        chroma_offset: u64,
        keep: Arc<dyn std::any::Any + Send + Sync>,
        download: Download,
        memory: Option<SharedMemory>,
    ) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 62);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        SharedPicture { buffer, device, format, coded, pitch, chroma_offset, id, alive: Arc::new(()), memory, _keep: keep, download }
    }

    /// The bytes a row of the plane takes (luma: one sample per pixel, chroma: a Cb Cr pair per
    /// two pixels; 2 bytes a sample at 10 bits).
    pub fn row_bytes(&self) -> u32 {
        self.coded.0.saturating_mul(if self.format == GpuFormat::P010 { 2 } else { 1 })
    }

    /// Whether the planes can be copied into textures as they lie: the pitch a multiple of
    /// wgpu's 256-byte copy alignment, the rows inside it, both planes inside the buffer.
    pub fn copyable(&self) -> bool {
        let (w, h) = self.coded;
        let pitch = u64::from(self.pitch);
        w > 0
            && h > 0
            && h.is_multiple_of(2)
            && matches!(self.format, GpuFormat::Nv12 | GpuFormat::P010)
            && self.pitch.is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            && self.row_bytes() <= self.pitch
            && pitch.checked_mul(u64::from(h)).is_some_and(|luma| luma <= self.chroma_offset)
            && pitch.checked_mul(u64::from(h / 2)).and_then(|c| c.checked_add(self.chroma_offset)).is_some_and(|end| end <= self.buffer.size())
    }
}

impl std::fmt::Debug for SharedPicture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedPicture").field("format", &self.format).field("coded", &self.coded).field("pitch", &self.pitch).finish_non_exhaustive()
    }
}

impl GpuSurface for SharedPicture {
    fn memory_id(&self) -> u64 {
        self.id
    }
    fn liveness(&self) -> std::sync::Weak<()> {
        Arc::downgrade(&self.alive)
    }
    fn format(&self) -> GpuFormat {
        self.format
    }
    fn coded_size(&self) -> (u32, u32) {
        self.coded
    }
    fn crop_origin(&self) -> (u32, u32) {
        (0, 0)
    }
    fn dmabuf(&self) -> Option<DmaBuf> {
        None
    }
    fn download(&self, width: u32, height: u32) -> Result<PixelData, String> {
        DOWNLOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        (self.download)(width, height)
    }
    fn gpu_bytes(&self) -> usize {
        usize::try_from(self.buffer.size()).unwrap_or(usize::MAX)
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
}
