//! Loading the DeepFilterNet3 checkpoint (`checkpoints/model_120.ckpt.best` from the official
//! `DeepFilterNet3.zip`): a PyTorch zip whose pickle is the bare state dictionary. It is read with
//! candle's restricted pickle reader (rebuilds tensors only, never runs code from the file) under
//! [`filmcraft_ml::job::guarded`], then every tensor the network uses is checked against
//! [`manifest`] (name and exact shape) and for finite values. A missing, truncated, foreign or
//! corrupt file is an error, never a panic.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use candle_core::{DType, Tensor};

use crate::ModelError;

/// The released checkpoint is 8.7 MB; anything far larger isn't it.
pub const MAX_FILE_BYTES: u64 = 64 << 20;
/// PyTorch's BatchNorm default.
const BN_EPS: f32 = 1e-5;

fn err(e: impl std::fmt::Display) -> ModelError {
    ModelError(e.to_string())
}

/// Every tensor the network reads, with its shape. BatchNorm layers list their four tensors.
pub fn manifest() -> Vec<(String, Vec<usize>)> {
    let mut m: Vec<(String, Vec<usize>)> = Vec::new();
    let mut add = |name: String, dims: &[usize]| m.push((name, dims.to_vec()));
    let bn = |add: &mut dyn FnMut(String, &[usize]), p: &str, c: usize| {
        for t in ["weight", "bias", "running_mean", "running_var"] {
            add(format!("{p}.{t}"), &[c]);
        }
    };
    let gru = |add: &mut dyn FnMut(String, &[usize]), p: &str, layers: usize| {
        for l in 0..layers {
            add(format!("{p}.weight_ih_l{l}"), &[768, 256]);
            add(format!("{p}.weight_hh_l{l}"), &[768, 256]);
            add(format!("{p}.bias_ih_l{l}"), &[768]);
            add(format!("{p}.bias_hh_l{l}"), &[768]);
        }
    };
    add("erb_fb".into(), &[481, 32]);
    add("mask.erb_inv_fb".into(), &[32, 481]);
    // Encoder.
    add("enc.erb_conv0.1.weight".into(), &[64, 1, 3, 3]);
    bn(&mut add, "enc.erb_conv0.2", 64);
    for i in 1..=3 {
        add(format!("enc.erb_conv{i}.0.weight"), &[64, 1, 1, 3]);
        add(format!("enc.erb_conv{i}.1.weight"), &[64, 64, 1, 1]);
        bn(&mut add, &format!("enc.erb_conv{i}.2"), 64);
    }
    add("enc.df_conv0.1.weight".into(), &[64, 1, 3, 3]);
    add("enc.df_conv0.2.weight".into(), &[64, 64, 1, 1]);
    bn(&mut add, "enc.df_conv0.3", 64);
    add("enc.df_conv1.0.weight".into(), &[64, 1, 1, 3]);
    add("enc.df_conv1.1.weight".into(), &[64, 64, 1, 1]);
    bn(&mut add, "enc.df_conv1.2", 64);
    add("enc.df_fc_emb.0.weight".into(), &[32, 96, 16]);
    add("enc.emb_gru.linear_in.0.weight".into(), &[16, 32, 16]);
    gru(&mut add, "enc.emb_gru.gru", 1);
    add("enc.emb_gru.linear_out.0.weight".into(), &[16, 16, 32]);
    add("enc.lsnr_fc.0.weight".into(), &[1, 512]);
    add("enc.lsnr_fc.0.bias".into(), &[1]);
    // ERB decoder.
    add("erb_dec.emb_gru.linear_in.0.weight".into(), &[16, 32, 16]);
    gru(&mut add, "erb_dec.emb_gru.gru", 2);
    add("erb_dec.emb_gru.linear_out.0.weight".into(), &[16, 16, 32]);
    for i in (0..=3).rev() {
        add(format!("erb_dec.conv{i}p.0.weight"), &[64, 1, 1, 1]);
        bn(&mut add, &format!("erb_dec.conv{i}p.1"), 64);
        if i > 0 {
            add(format!("erb_dec.convt{i}.0.weight"), &[64, 1, 1, 3]);
            add(format!("erb_dec.convt{i}.1.weight"), &[64, 64, 1, 1]);
            bn(&mut add, &format!("erb_dec.convt{i}.2"), 64);
        }
    }
    add("erb_dec.conv0_out.0.weight".into(), &[1, 64, 1, 3]);
    bn(&mut add, "erb_dec.conv0_out.1", 1);
    // Deep-filter decoder.
    add("df_dec.df_convp.1.weight".into(), &[10, 32, 5, 1]);
    add("df_dec.df_convp.2.weight".into(), &[10, 10, 1, 1]);
    bn(&mut add, "df_dec.df_convp.3", 10);
    add("df_dec.df_gru.linear_in.0.weight".into(), &[8, 64, 32]);
    gru(&mut add, "df_dec.df_gru.gru", 2);
    add("df_dec.df_skip.weight".into(), &[16, 32, 16]);
    add("df_dec.df_out.0.weight".into(), &[16, 16, 60]);
    add("df_dec.df_fc_a.0.weight".into(), &[1, 256]);
    add("df_dec.df_fc_a.0.bias".into(), &[1]);
    m
}

/// Every tensor of a PyTorch zip checkpoint. candle's pickle reader finds the tensors (their
/// storage file, offset, shape and strides) but refuses strided storage, and the release stores
/// `df_dec.df_convp.1.weight` channels-last, so the values are gathered here, every offset
/// bounds-checked against the storage actually present.
fn read_tensors(path: &Path) -> Result<Vec<(String, Tensor)>, ModelError> {
    let wanted: HashMap<String, usize> = manifest().into_iter().map(|(n, d)| (n, d.iter().product())).collect();
    read_pth(path, &wanted, MAX_FILE_BYTES, "DeepFilterNet3")?
        .into_iter()
        .map(|(name, dims, values)| Ok((name, Tensor::from_vec(values, dims, &candle_core::Device::Cpu).map_err(err)?)))
        .collect()
}

/// The f32 tensors named in `wanted` (name → element count) of a PyTorch zip checkpoint, as
/// (name, shape, row-major values); `max_bytes` caps the storage read, `model` names the model in
/// errors. Shared by DeepFilterNet3 and MossFormer2 (fork AUDIO-PLAN A3.7).
pub(crate) fn read_pth(path: &Path, wanted: &HashMap<String, usize>, max_bytes: u64, model: &str) -> Result<Vec<(String, Vec<usize>, Vec<f32>)>, ModelError> {
    use std::io::Read;
    let infos = candle_core::pickle::read_pth_tensor_info(path, false, None).map_err(err)?;
    let file = std::fs::File::open(path).map_err(err)?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(err)?;
    // Only the manifest's tensors are read, each name once, each storage file decompressed once
    // and all of them together capped: a hostile file with thousands of entries over one
    // deflate-bomb storage would otherwise keep a full copy per entry (A2 review, 2026-10-15).
    let mut storages: HashMap<String, Arc<Vec<f32>>> = HashMap::new();
    let mut storage_bytes = 0u64;
    let mut out: Vec<(String, Vec<usize>, Vec<f32>)> = Vec::with_capacity(wanted.len());
    for info in infos {
        // `num_batches_tracked` (i64) isn't used.
        if info.dtype != DType::F32 || out.iter().any(|(n, _, _)| *n == info.name) {
            continue;
        }
        let Some(&expected) = wanted.get(&info.name) else { continue };
        let storage = match storages.get(&info.path) {
            Some(s) => s.clone(),
            None => {
                let entry = zip.by_name(&info.path).map_err(|e| err(format!("`{}`: {e}", info.name)))?;
                let left = max_bytes.saturating_sub(storage_bytes);
                if entry.size() > left {
                    return Err(err(format!("`{}`: storage too large", info.name)));
                }
                let mut bytes = Vec::new();
                entry.take(left).read_to_end(&mut bytes).map_err(err)?;
                storage_bytes = storage_bytes.saturating_add(bytes.len() as u64);
                let s: Arc<Vec<f32>> = Arc::new(bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect());
                storages.insert(info.path.clone(), s.clone());
                s
            }
        };
        let (dims, strides) = (info.layout.dims().to_vec(), info.layout.stride().to_vec());
        let count = dims
            .iter()
            .try_fold(1usize, |a, d| a.checked_mul(*d))
            .filter(|c| *c <= storage.len() && *c == expected)
            .ok_or_else(|| err(format!("`{}`: shape {dims:?} doesn't fit {model} or its storage", info.name)))?;
        // candle's pickle reader reports the storage offset in bytes (it skips that many bytes
        // itself); GRU weights are views into one shared storage, so it matters here.
        let start =
            Some(info.layout.start_offset()).filter(|b| b % 4 == 0).map(|b| b / 4).ok_or_else(|| err(format!("`{}`: misaligned storage offset", info.name)))?;
        // contiguous row-major (every MossFormer2 tensor): one bounds-checked copy
        let mut contiguous = vec![1usize; dims.len()];
        let mut acc = 1usize;
        for (c, d) in contiguous.iter_mut().zip(&dims).rev() {
            *c = acc;
            acc = acc.saturating_mul(*d);
        }
        if contiguous == strides {
            let src = start.checked_add(count).and_then(|end| storage.get(start..end)).ok_or_else(|| err(format!("`{}`: outside its storage", info.name)))?;
            out.push((info.name, dims, src.to_vec()));
            continue;
        }
        let mut values = Vec::with_capacity(count);
        let mut idx = vec![0usize; dims.len()];
        for _ in 0..count {
            let mut off = start;
            for (i, s) in idx.iter().zip(&strides) {
                off = i.checked_mul(*s).and_then(|o| o.checked_add(off)).ok_or_else(|| err(format!("`{}`: bad strides", info.name)))?;
            }
            values.push(*storage.get(off).ok_or_else(|| err(format!("`{}`: offset {off} outside its storage", info.name)))?);
            // Next index, last dimension fastest.
            for (i, d) in idx.iter_mut().zip(&dims).rev() {
                *i += 1;
                if *i < *d {
                    break;
                }
                *i = 0;
            }
        }
        out.push((info.name, dims, values));
    }
    Ok(out)
}

/// The checked DeepFilterNet3 tensors (f32, CPU).
#[derive(Debug)]
pub struct Weights {
    map: HashMap<String, Tensor>,
    /// The same values flattened, shared by every network instance.
    flat: HashMap<String, Arc<[f32]>>,
}

impl Weights {
    /// Read and check a checkpoint file.
    pub fn load(path: &Path) -> Result<Weights, ModelError> {
        let size = std::fs::metadata(path).map_err(|e| err(format!("{}: {e}", path.display())))?.len();
        if size > MAX_FILE_BYTES {
            return Err(err(format!("{}: {size} bytes is too large for a DeepFilterNet3 checkpoint", path.display())));
        }
        let cancel = filmcraft_ml::job::Cancel::new();
        let raw = filmcraft_ml::job::guarded(&cancel, |_| read_tensors(path))
            .map_err(|e| err(format!("{}: not a readable DeepFilterNet3 checkpoint ({e})", path.display())))?;
        Weights::from_tensors(raw)
    }

    /// Check named tensors against the manifest; extra entries (`num_batches_tracked`) are dropped.
    pub fn from_tensors(raw: Vec<(String, Tensor)>) -> Result<Weights, ModelError> {
        let mut raw: HashMap<String, Tensor> = raw.into_iter().collect();
        let mut map = HashMap::new();
        let mut flat = HashMap::new();
        for (name, dims) in manifest() {
            let t = raw.remove(&name).ok_or_else(|| err(format!("the checkpoint has no `{name}`: not DeepFilterNet3")))?;
            if t.dims() != dims.as_slice() {
                return Err(err(format!("`{name}` is {:?}, DeepFilterNet3 has {dims:?}", t.dims())));
            }
            let t = t.to_dtype(DType::F32).map_err(err)?;
            let values = t.flatten_all().and_then(|f| f.to_vec1::<f32>()).map_err(err)?;
            if !values.iter().all(|v| v.is_finite()) {
                return Err(err(format!("`{name}` holds non-finite values: the file is corrupt")));
            }
            flat.insert(name.clone(), Arc::from(values));
            map.insert(name, t);
        }
        Ok(Weights { map, flat })
    }

    pub fn get(&self, name: &str) -> Result<&Tensor, ModelError> {
        self.map.get(name).ok_or_else(|| err(format!("missing weight `{name}`")))
    }

    /// A tensor's values in row-major order, shared (cheap to clone).
    pub fn values(&self, name: &str) -> Result<Arc<[f32]>, ModelError> {
        self.flat.get(name).cloned().ok_or_else(|| err(format!("missing weight `{name}`")))
    }

    /// A BatchNorm layer (inference) as a per-channel affine `y = x · scale + shift`.
    pub fn batch_norm(&self, prefix: &str) -> Result<(Vec<f32>, Vec<f32>), ModelError> {
        let v = |t: &str| -> Result<Vec<f32>, ModelError> { self.get(&format!("{prefix}.{t}"))?.to_vec1::<f32>().map_err(err) };
        let (g, b, mean, var) = (v("weight")?, v("bias")?, v("running_mean")?, v("running_var")?);
        let mut scale = Vec::with_capacity(g.len());
        let mut shift = Vec::with_capacity(g.len());
        for (((g, b), m), var) in g.iter().zip(&b).zip(&mean).zip(&var) {
            let s = g / (var.max(0.0) + BN_EPS).sqrt();
            scale.push(s);
            shift.push(b - m * s);
        }
        Ok((scale, shift))
    }

    /// Bins per ERB band, read from the checkpoint's `erb_fb` (non-zero rows per column).
    pub fn erb_widths(&self) -> Result<Vec<usize>, ModelError> {
        let fb = self.get("erb_fb")?.to_vec2::<f32>().map_err(err)?;
        let bands = fb.first().map_or(0, Vec::len);
        Ok((0..bands).map(|b| fb.iter().filter(|row| row.get(b).is_some_and(|v| *v != 0.0)).count()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::Device;

    fn synthetic() -> Vec<(String, Tensor)> {
        let mut v: Vec<(String, Tensor)> = manifest().into_iter().map(|(n, d)| (n, Tensor::ones(d.as_slice(), DType::F32, &Device::Cpu).unwrap())).collect();
        v.push(("enc.erb_conv0.2.num_batches_tracked".into(), Tensor::new(3i64, &Device::Cpu).unwrap()));
        v
    }

    #[test]
    fn manifest_covers_the_released_model() {
        let m = manifest();
        let params: usize =
            m.iter().filter(|(n, _)| !n.ends_with("running_mean") && !n.ends_with("running_var")).map(|(_, d)| d.iter().product::<usize>()).sum();
        let stats: usize = m.iter().filter(|(n, _)| n.ends_with("running_mean") || n.ends_with("running_var")).map(|(_, d)| d.iter().product::<usize>()).sum();
        // 2 167 969 values in the released state dict, 15 of them `num_batches_tracked` scalars.
        assert_eq!(params + stats + 15, 2_167_969);
        let mut names: Vec<_> = m.iter().map(|(n, _)| n.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), m.len());
    }

    #[test]
    fn checks_names_shapes_and_values() {
        let w = Weights::from_tensors(synthetic()).unwrap();
        let (s, b) = w.batch_norm("enc.erb_conv0.2").unwrap();
        assert_eq!(s.len(), 64);
        assert!((s[0] - 1.0 / (1.0f32 + 1e-5).sqrt()).abs() < 1e-6 && (b[0] - (1.0 - s[0])).abs() < 1e-6);

        let mut missing = synthetic();
        missing.retain(|(n, _)| n != "df_dec.df_out.0.weight");
        assert!(Weights::from_tensors(missing).unwrap_err().0.contains("df_out"));

        let mut wrong = synthetic();
        wrong[5].1 = Tensor::ones(&[3usize, 3], DType::F32, &Device::Cpu).unwrap();
        assert!(Weights::from_tensors(wrong).is_err());

        let mut nan = synthetic();
        let dims = nan[0].1.dims().to_vec();
        nan[0].1 = (Tensor::ones(dims.as_slice(), DType::F32, &Device::Cpu).unwrap() * f64::NAN).unwrap();
        assert!(Weights::from_tensors(nan).unwrap_err().0.contains("non-finite"));
    }

    #[test]
    fn hostile_files_are_errors() {
        let dir = std::env::temp_dir().join(format!("fc-voice-weights-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(Weights::load(&dir.join("absent.ckpt")).is_err());
        let cases: [(&str, Vec<u8>); 4] = [
            ("empty.ckpt", vec![]),
            ("garbage.ckpt", (0..4096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect()),
            ("zip-header.ckpt", b"PK\x03\x04\x14\x00\x00\x00\x00\x00garbage".to_vec()),
            ("pickle-only.ckpt", b"\x80\x02}q\x00.".to_vec()),
        ];
        for (name, bytes) in cases {
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            assert!(Weights::load(&p).is_err(), "{name}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A PyTorch zip checkpoint (pickle protocol 2) whose entries all view storage `0`.
    fn torch_zip(path: &Path, entries: &[(String, usize)], storage_floats: usize) {
        use std::io::Write;
        let mut pk: Vec<u8> = b"\x80\x02}(".to_vec();
        let uni = |pk: &mut Vec<u8>, t: &str| {
            pk.push(b'X');
            pk.extend((t.len() as u32).to_le_bytes());
            pk.extend(t.as_bytes());
        };
        let int = |pk: &mut Vec<u8>, v: usize| {
            pk.push(b'J');
            pk.extend((v as i32).to_le_bytes());
        };
        for (name, len) in entries {
            uni(&mut pk, name);
            pk.extend(b"ctorch._utils\n_rebuild_tensor_v2\n((");
            uni(&mut pk, "storage");
            pk.extend(b"ctorch\nFloatStorage\n");
            uni(&mut pk, "0");
            uni(&mut pk, "cpu");
            int(&mut pk, storage_floats);
            pk.extend(b"tQ");
            int(&mut pk, 0);
            pk.push(b'(');
            int(&mut pk, *len);
            pk.extend(b"t(");
            int(&mut pk, 1);
            pk.extend(b"t\x89ccollections\nOrderedDict\n)RtR");
        }
        pk.extend(b"u.");
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        z.start_file("archive/data.pkl", stored).unwrap();
        z.write_all(&pk).unwrap();
        let deflated = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        z.start_file("archive/data/0", deflated).unwrap();
        z.write_all(&vec![0u8; storage_floats * 4]).unwrap();
        z.finish().unwrap();
    }

    /// A2 review finding 1: thousands of entries over one small-on-disk 4 MB storage used to keep a
    /// full gathered copy each (12 GB here: out of memory, an abort `catch_unwind` can't catch).
    /// Unknown names are skipped now and each storage file is read once.
    #[test]
    fn many_entries_over_one_storage_are_not_copied_each() {
        let dir = std::env::temp_dir().join(format!("fc-voice-bomb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bomb.ckpt");
        let floats = 1 << 20;
        let mut entries: Vec<(String, usize)> = (0..3000).map(|i| (format!("junk.{i}"), floats)).collect();
        // a known name with the wrong size proves the file got through the pickle reader
        entries.push(("erb_fb".into(), floats));
        torch_zip(&p, &entries, floats);
        assert!(std::fs::metadata(&p).unwrap().len() < 1 << 20);
        let e = Weights::load(&p).unwrap_err();
        assert!(e.0.contains("erb_fb") && e.0.contains("doesn't fit"), "{e:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The real checkpoint, when `FILMCRAFT_DFN3_CKPT` points at it: it loads, and its ERB
    /// filterbank has the band widths [`crate::dsp::erb_widths`] computes.
    #[test]
    fn released_checkpoint_when_present() {
        let Some(path) = std::env::var_os("FILMCRAFT_DFN3_CKPT") else {
            eprintln!("FILMCRAFT_DFN3_CKPT unset: skipped");
            return;
        };
        let w = Weights::load(Path::new(&path)).unwrap();
        let p = crate::DfParams::DFN3;
        assert_eq!(w.erb_widths().unwrap(), crate::dsp::erb_widths(p.sample_rate, p.fft_size, p.nb_erb, p.min_nb_erb_freqs).unwrap());
    }
}
