//! Packed 4:2:2 pictures split into the compositor's biplanar planes on the GPU (fork 6.1b,
//! owner §1a.30).
//!
//! Intel's media engine decodes HEVC 4:2:2 and hands each picture over as a linear DMA-BUF of
//! packed `Y210` (per two pixels `Y0 Cb Y1 Cr`, 16-bit MSB-aligned samples), which the importer
//! ([`crate::interop`]) shows as one `Rgba16Unorm` texture of half the width, or, on a GPU that
//! takes no 64-bit texel format from a DMA-BUF (NVIDIA's driver lists no modifiers for them: the
//! XPS 16's RTX 4050), as one `Rg16Unorm` texture of the full (even) width, the same bytes as
//! (`Y0 Cb`) (`Y1 Cr`) texel pairs. A compute pass reads
//! it once and writes the planes every other 10-bit picture uses into VRAM: luma (`R16Unorm`,
//! full size) and interleaved Cb/Cr (`Rg16Unorm`, half width, full height), through two staging
//! buffers copied into the textures (no render targets: 16-bit UNORM rendering needs adapter-
//! specific format features the app's device does not request). The compositor then samples the
//! picture with its biplanar path, exactly as one read back and uploaded (`Yuv16Bi` 4:2:2). UNORM
//! loads of 16-bit codes are exact (`code / 65535`, rounded back to `code`).

const SHADER: &str = r"
struct U { cw: u32, h: u32, luma_words: u32, chroma_words: u32, pairs: u32, p0: u32, p1: u32, p2: u32 }
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var<storage, read_write> luma: array<u32>;
@group(0) @binding(3) var<storage, read_write> chroma: array<u32>;

fn code(v: f32) -> u32 {
    return u32(clamp(round(v * 65535.0), 0.0, 65535.0));
}

// one texel (a pixel pair) per invocation: its two luma samples are one word of the luma row,
// its Cb and Cr one word of the chroma row (little-endian 16-bit samples)
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= u.cw || id.y >= u.h) {
        return;
    }
    var t: vec4<f32>;
    if (u.pairs != 0u) {
        // `Rg16Unorm` view: the pair's (Y0 Cb) and (Y1 Cr) are two texels
        let a = textureLoad(src, vec2<i32>(i32(id.x * 2u), i32(id.y)), 0);
        let b = textureLoad(src, vec2<i32>(i32(id.x * 2u + 1u), i32(id.y)), 0);
        t = vec4<f32>(a.r, a.g, b.r, b.g);
    } else {
        t = textureLoad(src, vec2<i32>(i32(id.x), i32(id.y)), 0);
    }
    luma[id.y * u.luma_words + id.x] = code(t.r) | (code(t.b) << 16u);
    chroma[id.y * u.chroma_words + id.x] = code(t.g) | (code(t.a) << 16u);
}
";

/// Rows of buffer → texture copies are a multiple of this many bytes.
const ROW_ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

/// The unpacking pipeline and its staging buffers (kept for the next picture of the size).
pub struct Unpack422 {
    bgl: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
    uniforms: wgpu::Buffer,
    /// (luma, chroma, their byte sizes)
    staging: Option<(wgpu::Buffer, wgpu::Buffer, (u64, u64))>,
}

/// A picture unpacked into VRAM: its planes and the submission that writes them.
pub struct Unpacked {
    pub luma: wgpu::TextureView,
    pub chroma: wgpu::TextureView,
    pub submission: wgpu::SubmissionIndex,
}

impl Unpack422 {
    /// None when the device has no 16-bit normalized textures (`TEXTURE_FORMAT_16BIT_NORM`):
    /// such pictures are read back instead.
    pub fn new(device: &wgpu::Device) -> Option<Unpack422> {
        if !device.features().contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM) {
            return None;
        }
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("unpack 4:2:2"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("unpack 4:2:2"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                storage(2),
                storage(3),
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("unpack 4:2:2"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("unpack 4:2:2"),
            layout: Some(&layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("unpack 4:2:2"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Some(Unpack422 { bgl, pipeline, uniforms, staging: None })
    }

    /// Unpack the `w`×`h` picture at the top-left of `packed` (an `Rgba16Unorm` view at least
    /// ⌈w/2⌉ × h texels, or an `Rg16Unorm` view at least 2⌈w/2⌉ × h) into new VRAM planes, submitted on `queue`. None for an empty picture,
    /// one beyond the device's limits (texture size, storage buffer size), or a smaller source.
    pub fn run(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, packed: &wgpu::TextureView, (w, h): (u32, u32)) -> Option<Unpacked> {
        let limits = device.limits();
        let src = packed.texture().size();
        let cw = w.div_ceil(2);
        let pairs = match packed.texture().format() {
            wgpu::TextureFormat::Rgba16Unorm => false,
            wgpu::TextureFormat::Rg16Unorm => true,
            _ => return None,
        };
        let src_pairs = if pairs { src.width / 2 } else { src.width };
        if w == 0 || h == 0 || w > limits.max_texture_dimension_2d || h > limits.max_texture_dimension_2d || src_pairs < cw || src.height < h {
            return None;
        }
        let row = |bytes: u32| bytes.checked_next_multiple_of(ROW_ALIGN);
        let (luma_row, chroma_row) = (row(w.checked_mul(2)?)?, row(cw.checked_mul(4)?)?);
        let sizes = (u64::from(luma_row) * u64::from(h), u64::from(chroma_row) * u64::from(h));
        let max = limits.max_storage_buffer_binding_size.min(limits.max_buffer_size);
        if sizes.0 > max || sizes.1 > max {
            return None;
        }
        if self.staging.as_ref().is_none_or(|s| s.2 != sizes) {
            let buffer = |size| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("unpack 4:2:2"),
                    size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                })
            };
            self.staging = Some((buffer(sizes.0), buffer(sizes.1), sizes));
        }
        let (luma_buf, chroma_buf, _) = self.staging.as_ref()?;
        let words: [u32; 8] = [cw, h, luma_row / 4, chroma_row / 4, u32::from(pairs), 0, 0, 0];
        let bytes: Vec<u8> = words.iter().flat_map(|v| v.to_le_bytes()).collect();
        queue.write_buffer(&self.uniforms, 0, &bytes);
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("unpack 4:2:2"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.uniforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(packed) },
                wgpu::BindGroupEntry { binding: 2, resource: luma_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: chroma_buf.as_entire_binding() },
            ],
        });
        let plane = |label: &'static str, width: u32, format: wgpu::TextureFormat| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let (luma, chroma) = (plane("4:2:2 luma", w, wgpu::TextureFormat::R16Unorm), plane("4:2:2 chroma", cw, wgpu::TextureFormat::Rg16Unorm));
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("unpack 4:2:2") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("unpack 4:2:2"), timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(cw.div_ceil(16), h.div_ceil(16), 1);
        }
        for (buffer, texture, bytes_per_row) in [(luma_buf, &luma, luma_row), (chroma_buf, &chroma, chroma_row)] {
            enc.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer,
                    layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: Some(h) },
                },
                texture.as_image_copy(),
                texture.size(),
            );
        }
        let submission = queue.submit([enc.finish()]);
        Some(Unpacked { luma: luma.create_view(&Default::default()), chroma: chroma.create_view(&Default::default()), submission })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
        let desc = wgpu::DeviceDescriptor { required_features: adapter.features() & wgpu::Features::TEXTURE_FORMAT_16BIT_NORM, ..Default::default() };
        pollster::block_on(adapter.request_device(&desc)).ok()
    }

    /// The texels of `view`'s texture (`bpp` bytes each) as 16-bit samples, row by row.
    fn read(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView, bpp: u32) -> Vec<u16> {
        let t = view.texture();
        let (w, h) = (t.width(), t.height());
        let row = (w * bpp).div_ceil(256) * 256;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(row * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            t.as_image_copy(),
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
            t.size(),
        );
        queue.submit([enc.finish()]);
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let data = buf.slice(..).get_mapped_range().unwrap();
        let mut out = Vec::new();
        for r in 0..h {
            let line = &data[(r * row) as usize..(r * row + w * bpp) as usize];
            out.extend(line.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)));
        }
        out
    }

    /// Fork 6.1b: a packed 4:2:2 picture (odd width, a source wider than the picture as a linear
    /// copy is, MSB-aligned 10-bit codes and stray low bits) unpacks into exactly the luma and
    /// Cb/Cr samples the CPU read-back takes from the same memory.
    #[test]
    fn unpacks_exactly_as_the_cpu_read_back() {
        unpacks_exactly(wgpu::TextureFormat::Rgba16Unorm);
    }

    /// XPS 10: the same memory viewed as `Rg16Unorm` texel pairs (how NVIDIA's driver takes
    /// Intel's linear copy: it lists no DMA-BUF modifiers for 64-bit formats) unpacks the same.
    #[test]
    fn unpacks_texel_pairs_exactly_as_the_cpu_read_back() {
        unpacks_exactly(wgpu::TextureFormat::Rg16Unorm);
    }

    fn unpacks_exactly(format: wgpu::TextureFormat) {
        let Some((device, queue)) = device() else {
            eprintln!("SKIPPED: no GPU");
            return;
        };
        let Some(mut unpack) = Unpack422::new(&device) else {
            eprintln!("SKIPPED: no 16-bit normalized textures");
            return;
        };
        let (w, h) = (37u32, 5u32);
        let (pw, cw) = (24u32, w.div_ceil(2));
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut packed = vec![0u16; (pw * h * 4) as usize];
        for v in &mut packed {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *v = (seed & 0xffc0) as u16 | if seed & 0x10000 != 0 { 0x3f } else { 0 };
        }
        let src = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: if format == wgpu::TextureFormat::Rg16Unorm { pw * 2 } else { pw }, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let bytes: Vec<u8> = packed.iter().flat_map(|v| v.to_le_bytes()).collect();
        queue.write_texture(
            src.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(pw * 8), rows_per_image: Some(h) },
            src.size(),
        );
        let view = src.create_view(&Default::default());
        let out = unpack.run(&device, &queue, &view, (w, h)).expect("unpacked");
        let (y, uv) = (read(&device, &queue, &out.luma, 2), read(&device, &queue, &out.chroma, 4));
        for r in 0..h as usize {
            for x in 0..w as usize {
                let texel = (r * pw as usize + x / 2) * 4;
                let want = packed[texel + if x % 2 == 0 { 0 } else { 2 }];
                assert_eq!(y[r * w as usize + x], want, "luma ({x}, {r})");
            }
            for x in 0..cw as usize {
                let texel = (r * pw as usize + x) * 4;
                let got = &uv[(r * cw as usize + x) * 2..][..2];
                assert_eq!(got, [packed[texel + 1], packed[texel + 3]], "chroma ({x}, {r})");
            }
        }
        // refused: empty, wider or taller than the source
        assert!(unpack.run(&device, &queue, &view, (0, h)).is_none());
        assert!(unpack.run(&device, &queue, &view, (pw * 2 + 1, h)).is_none());
        assert!(unpack.run(&device, &queue, &view, (w, h + 1)).is_none());
    }

    /// A view of any other format is refused (read back instead), never misread.
    #[test]
    fn other_formats_are_refused() {
        let Some((device, queue)) = device() else {
            eprintln!("SKIPPED: no GPU");
            return;
        };
        let Some(mut unpack) = Unpack422::new(&device) else {
            eprintln!("SKIPPED: no 16-bit normalized textures");
            return;
        };
        let src = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: 8, height: 2, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        assert!(unpack.run(&device, &queue, &src.create_view(&Default::default()), (4, 2)).is_none());
    }
}
