//! Each nn op against a plain CPU loop on small odd shapes. Skips without a GPU.

use super::*;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    nn: NnKernels,
}

fn gpu() -> Option<Gpu> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }),
    )
    .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let nn = NnKernels::new(&device);
    Some(Gpu { device, queue, nn })
}

impl Gpu {
    fn buf(&self, data: &[f32]) -> wgpu::Buffer {
        let contents: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: &contents,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        })
    }
    fn zeros(&self, n: usize) -> wgpu::Buffer {
        self.buf(&vec![0.0; n])
    }
    fn run(&self, steps: &[NnStep], out: &wgpu::Buffer, n: usize) -> Vec<f32> {
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&Default::default());
            for s in steps {
                s.record(&mut pass);
            }
        }
        let read = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: n as u64 * 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        enc.copy_buffer_to_buffer(out, 0, &read, 0, n as u64 * 4);
        self.queue.submit([enc.finish()]);
        read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let data = read.slice(..).get_mapped_range().unwrap();
        data.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
    }
}

fn vals(n: usize, seed: u32) -> Vec<f32> {
    let mut s = seed.wrapping_mul(2_654_435_761).max(1);
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s as f32 / u32::MAX as f32 - 0.5) * 2.0
        })
        .collect()
}

fn close(a: &[f32], b: &[f32], tol: f32, what: &str) {
    assert_eq!(a.len(), b.len(), "{what}");
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        assert!((x - y).abs() <= tol * (1.0 + y.abs()), "{what}[{i}]: {x} vs {y}");
    }
}

fn gelu_tanh(v: f32) -> f32 {
    0.5 * v * (1.0 + (0.797_884_6 * v * (1.0 + 0.044715 * v * v)).tanh())
}

#[test]
fn matmul_with_every_epilogue_and_weight_layout() {
    let Some(g) = gpu() else { return };
    let (m, n, k) = (70, 37, 19);
    let a = vals(m * k, 1);
    let w = vals(n * k, 2);
    let b = vals(n, 3);
    let r = vals(m * n, 4);
    let wt: Vec<f32> = (0..k * n).map(|i| w[(i % n) * k + i / n]).collect();
    for (gelu, acc, after, kn) in [(false, false, false, false), (true, false, false, false), (false, true, true, false), (true, true, false, true)] {
        let mut want = vec![0f32; m * n];
        for i in 0..m {
            for j in 0..n {
                let mut v = b[j] + (0..k).map(|t| a[i * k + t] * w[j * k + t]).sum::<f32>();
                if gelu {
                    v = gelu_tanh(v);
                }
                if acc {
                    v += r[i * n + j];
                }
                if after {
                    v = gelu_tanh(v);
                }
                want[i * n + j] = v;
            }
        }
        let o = g.buf(&r);
        let s = Matmul { m, n, k, gelu, accumulate: acc, gelu_after: after, w_kn: kn };
        let step = g.nn.matmul(&s, &g.buf(&a), &g.buf(if kn { &wt } else { &w }), &g.buf(&b), &o).unwrap();
        close(&g.run(&[step], &o, m * n), &want, 1e-5, "matmul");
    }
    // too-small buffers are refused, not dispatched
    assert!(g.nn.matmul(&Matmul { m, n, k, ..Default::default() }, &g.zeros(5), &g.zeros(n * k), &g.zeros(n), &g.zeros(m * n)).is_err());
    assert!(g.nn.matmul(&Matmul { m: 0, n, k, ..Default::default() }, &g.zeros(5), &g.zeros(5), &g.zeros(5), &g.zeros(5)).is_err());
}

#[test]
fn convolutions_by_im2col_and_depthwise() {
    let Some(g) = gpu() else { return };
    let (cin, h, w, k, stride, pad) = (3, 11, 9, 5, 2, 2);
    let x = vals(cin * h * w, 5); // CHW
    let s = Im2col { cin, h, w, k, stride, pad, chw: true };
    let (oh, ow) = s.out_size().unwrap();
    let kd = k * k * cin;
    let col = g.zeros(oh * ow * kd);
    let step = g.nn.im2col(&s, &g.buf(&x), &col).unwrap();
    let got = g.run(&[step], &col, oh * ow * kd);
    for oy in 0..oh {
        for ox in 0..ow {
            for ky in 0..k {
                for kx in 0..k {
                    for c in 0..cin {
                        let (iy, ix) = ((oy * stride + ky) as isize - pad as isize, (ox * stride + kx) as isize - pad as isize);
                        let v = if iy < 0 || ix < 0 || iy >= h as isize || ix >= w as isize { 0.0 } else { x[c * h * w + iy as usize * w + ix as usize] };
                        assert_eq!(got[(oy * ow + ox) * kd + (ky * k + kx) * cin + c], v);
                    }
                }
            }
        }
    }
    // depthwise on HWC
    let (c, k, stride, pad) = (6, 3, 2, 1);
    let x = vals(h * w * c, 6);
    let wt = vals(c * k * k, 7);
    let b = vals(c, 8);
    let s = Depthwise { c, h, w, k, stride, pad, gelu: true };
    let (oh, ow) = s.out_size().unwrap();
    let o = g.zeros(oh * ow * c);
    let step = g.nn.depthwise(&s, &g.buf(&x), &g.buf(&wt), &g.buf(&b), &o).unwrap();
    let got = g.run(&[step], &o, oh * ow * c);
    let mut want = Vec::new();
    for oy in 0..oh {
        for ox in 0..ow {
            for ch in 0..c {
                let mut v = b[ch];
                for ky in 0..k {
                    for kx in 0..k {
                        let (iy, ix) = ((oy * stride + ky) as isize - pad as isize, (ox * stride + kx) as isize - pad as isize);
                        if iy >= 0 && ix >= 0 && iy < h as isize && ix < w as isize {
                            v += wt[ch * k * k + ky * k + kx] * x[(iy as usize * w + ix as usize) * c + ch];
                        }
                    }
                }
                want.push(gelu_tanh(v));
            }
        }
    }
    close(&got, &want, 1e-5, "depthwise");
    assert!(Im2col { cin: 1, h: 2, w: 2, k: 7, stride: 1, pad: 0, chw: false }.out_size().is_err());
}

#[test]
fn layer_norm_plain_windowed_and_channel_major() {
    let Some(g) = gpu() else { return };
    let (rh, rw, c, ws) = (5, 7, 12, 3);
    let x: Vec<f32> = vals(rh * rw * c, 9).iter().map(|v| v * 3.0 + 1.0).collect();
    let gm = vals(c, 10);
    let bt = vals(c, 11);
    let norm = |row: &[f32]| -> Vec<f32> {
        let mean = row.iter().sum::<f32>() / c as f32;
        let var = row.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / c as f32;
        row.iter().enumerate().map(|(i, v)| (v - mean) / (var + 1e-5).sqrt() * gm[i] + bt[i]).collect()
    };
    let rows = rh * rw;
    let o = g.zeros(rows * c);
    let s = LayerNorm { rows, c, eps: 1e-5, gather: None, channel_major: true };
    let got = g.run(&[g.nn.layer_norm(&s, &g.buf(&x), &g.buf(&gm), &g.buf(&bt), &o).unwrap()], &o, rows * c);
    let mut want = vec![0f32; rows * c];
    for r in 0..rows {
        for (ch, v) in norm(&x[r * c..(r + 1) * c]).into_iter().enumerate() {
            want[ch * rows + r] = v;
        }
    }
    close(&got, &want, 1e-5, "layer norm (CHW out)");
    let grid = WindowGrid { rh, rw, ws };
    let (nwx, _) = grid.windows();
    let o = g.zeros(grid.rows().unwrap() * c);
    let s = LayerNorm { rows: grid.rows().unwrap(), c, eps: 1e-5, gather: Some(grid), channel_major: false };
    let got = g.run(&[g.nn.layer_norm(&s, &g.buf(&x), &g.buf(&gm), &g.buf(&bt), &o).unwrap()], &o, grid.rows().unwrap() * c);
    for row in 0..grid.rows().unwrap() {
        let (win, t) = (row / grid.n().unwrap(), row % grid.n().unwrap());
        let (y, xx) = ((win / nwx) * ws + t / ws, (win % nwx) * ws + t % ws);
        let src = if y < rh && xx < rw { x[(y * rw + xx) * c..(y * rw + xx + 1) * c].to_vec() } else { vec![0.0; c] };
        close(&got[row * c..(row + 1) * c], &norm(&src), 1e-4, "windowed layer norm");
    }
}

#[test]
fn attention_windowed_global_cross_with_and_without_bias() {
    let Some(g) = gpu() else { return };
    // CPU reference over an explicit layout
    fn reference(s: &Attention, q: &[f32], kv: &[f32], bias: Option<&[f32]>, out_len: usize) -> Vec<f32> {
        let mut o = vec![0f32; out_len];
        for gr in 0..s.groups {
            for h in 0..s.heads {
                for i in 0..s.nq {
                    let qb = (gr * s.nq + i) * s.q_stride + s.q_off + h * s.q_head;
                    let scores: Vec<f32> = (0..s.nkv)
                        .map(|j| {
                            let kb = (gr * s.nkv + j) * s.kv_stride + s.k_off + h * s.kv_head;
                            let d: f32 = (0..s.head_dim).map(|t| q[qb + t] * kv[kb + t]).sum();
                            d * s.scale + bias.map_or(0.0, |b| b[(h * s.nq + i) * s.nkv + j])
                        })
                        .collect();
                    let m = scores.iter().copied().fold(f32::MIN, f32::max);
                    let e: Vec<f32> = scores.iter().map(|v| (v - m).exp()).collect();
                    let sum: f32 = e.iter().sum();
                    let ob = (gr * s.nq + i) * s.out_stride + h * s.out_head;
                    for t in 0..s.head_dim {
                        o[ob + t] = (0..s.nkv).map(|j| e[j] * kv[(gr * s.nkv + j) * s.kv_stride + s.v_off + h * s.kv_head + t]).sum::<f32>() / sum;
                    }
                }
            }
        }
        o
    }
    // windowed self-attention, TinyViT layout (per head q k v), with a bias
    let (groups, n, heads, hd) = (3, 9, 2, 16);
    let s = Attention::fused_qkv(groups, n, heads, hd, true).unwrap();
    let qkv = vals(groups * n * 3 * heads * hd, 12);
    let bias = vals(heads * n * n, 13);
    let out_len = groups * n * heads * hd;
    let qb = g.buf(&qkv);
    let o = g.zeros(out_len);
    let got = g.run(&[g.nn.attention(&s, &qb, &qb, Some(&g.buf(&bias)), &o).unwrap()], &o, out_len);
    close(&got, &reference(&s, &qkv, &qkv, Some(&bias), out_len), 1e-4, "windowed attention");
    // global self-attention, PyTorch layout ([q…][k…][v…]), head dim 64, no bias
    let s = Attention::fused_qkv(1, 21, 3, 64, false).unwrap();
    assert!(Attention::fused_qkv(1, 1, usize::MAX, 8, false).is_err());
    assert!(WindowGrid { rh: 4, rw: 4, ws: usize::MAX }.n().is_err());
    let qkv = vals(21 * 3 * 3 * 64, 14);
    let out_len = 21 * 3 * 64;
    let qb = g.buf(&qkv);
    let o = g.zeros(out_len);
    let got = g.run(&[g.nn.attention(&s, &qb, &qb, None, &o).unwrap()], &o, out_len);
    close(&got, &reference(&s, &qkv, &qkv, None, out_len), 1e-4, "global attention");
    // cross attention: 5 queries over 13 keys, separate buffers
    let (heads, hd) = (2, 8);
    let s = Attention {
        groups: 1,
        nq: 5,
        nkv: 13,
        heads,
        head_dim: hd,
        q_stride: heads * hd,
        kv_stride: 2 * heads * hd,
        out_stride: heads * hd,
        q_off: 0,
        k_off: 0,
        v_off: heads * hd,
        q_head: hd,
        kv_head: hd,
        out_head: hd,
        scale: 0.3,
    };
    let q = vals(5 * heads * hd, 15);
    let kv = vals(13 * 2 * heads * hd, 16);
    let o = g.zeros(5 * heads * hd);
    let got = g.run(&[g.nn.attention(&s, &g.buf(&q), &g.buf(&kv), None, &o).unwrap()], &o, 5 * heads * hd);
    close(&got, &reference(&s, &q, &kv, None, 5 * heads * hd), 1e-4, "cross attention");
    // bad shapes are refused
    assert!(g.nn.attention(&Attention { head_dim: 6, ..s }, &g.buf(&q), &g.buf(&kv), None, &o).is_err());
    assert!(g.nn.attention(&Attention { nkv: 14, ..s }, &g.buf(&q), &g.buf(&kv), None, &o).is_err());
}

#[test]
fn add_unary_softmax_and_resample() {
    let Some(g) = gpu() else { return };
    // plain add and window scatter
    let x = vals(40, 17);
    let src = vals(40, 18);
    let xb = g.buf(&x);
    let got = g.run(&[g.nn.add(&Add { len: 40, scatter: None }, &g.buf(&src), &xb).unwrap()], &xb, 40);
    close(&got, &x.iter().zip(&src).map(|(a, b)| a + b).collect::<Vec<_>>(), 1e-6, "add");
    let (grid, c) = (WindowGrid { rh: 4, rw: 5, ws: 3 }, 2);
    let src = vals(grid.rows().unwrap() * c, 19);
    let x = vals(4 * 5 * c, 20);
    let xb = g.buf(&x);
    let got = g.run(&[g.nn.add(&Add { len: 40, scatter: Some((grid, c)) }, &g.buf(&src), &xb).unwrap()], &xb, 40);
    let nwx = grid.windows().0;
    for (i, v) in got.iter().enumerate() {
        let (ch, tok) = (i % c, i / c);
        let (y, xx) = (tok / 5, tok % 5);
        let row = ((y / 3) * nwx + xx / 3) * 9 + (y % 3) * 3 + xx % 3;
        assert!((v - (x[i] + src[row * c + ch])).abs() < 1e-6);
    }
    // activations
    let x: Vec<f32> = vals(300, 21).iter().map(|v| v * 6.0).collect();
    let erf_gelu = |v: f32| 0.5 * v * (1.0 + libm_erf(v / std::f32::consts::SQRT_2));
    let sig = |v: f32| 1.0 / (1.0 + (-v).exp());
    let refs: [(UnaryOp, &dyn Fn(f32) -> f32); 5] = [
        (UnaryOp::GeluTanh, &gelu_tanh),
        (UnaryOp::GeluErf, &erf_gelu),
        (UnaryOp::Relu, &|v: f32| v.max(0.0)),
        (UnaryOp::Silu, &|v: f32| v * sig(v)),
        (UnaryOp::Sigmoid, &sig),
    ];
    for (op, f) in refs {
        let xb = g.buf(&x);
        let got = g.run(&[g.nn.unary(op, 300, &xb).unwrap()], &xb, 300);
        close(&got, &x.iter().map(|v| f(*v)).collect::<Vec<_>>(), 1e-5, &format!("{op:?}"));
    }
    // softmax
    let (rows, n) = (3, 150);
    let x: Vec<f32> = vals(rows * n, 22).iter().map(|v| v * 10.0).collect();
    let xb = g.buf(&x);
    let got = g.run(&[g.nn.softmax(rows, n, &xb).unwrap()], &xb, rows * n);
    for r in 0..rows {
        let row = &x[r * n..(r + 1) * n];
        let m = row.iter().copied().fold(f32::MIN, f32::max);
        let s: f32 = row.iter().map(|v| (v - m).exp()).sum();
        close(&got[r * n..(r + 1) * n], &row.iter().map(|v| (v - m).exp() / s).collect::<Vec<_>>(), 1e-5, "softmax");
    }
    // pooling and resizing (HWC)
    let (h, w, c) = (6, 8, 3);
    let x = vals(h * w * c, 23);
    let at = |y: usize, xx: usize, ch: usize| x[(y * w + xx) * c + ch];
    for mode in [ResampleMode::MaxPool(2), ResampleMode::AvgPool(2), ResampleMode::Nearest, ResampleMode::Bilinear] {
        let (oh, ow) = match mode {
            ResampleMode::MaxPool(k) | ResampleMode::AvgPool(k) => (h / k, w / k),
            _ => (13, 5),
        };
        let o = g.zeros(oh * ow * c);
        let got = g.run(&[g.nn.resample(&Resample { c, h, w, oh, ow, mode }, &g.buf(&x), &o).unwrap()], &o, oh * ow * c);
        let mut want = Vec::new();
        for oy in 0..oh {
            for ox in 0..ow {
                for ch in 0..c {
                    want.push(match mode {
                        ResampleMode::MaxPool(k) => (0..k * k).map(|t| at(oy * k + t / k, ox * k + t % k, ch)).fold(f32::MIN, f32::max),
                        ResampleMode::AvgPool(k) => (0..k * k).map(|t| at(oy * k + t / k, ox * k + t % k, ch)).sum::<f32>() / (k * k) as f32,
                        ResampleMode::Nearest => at(oy * h / oh, ox * w / ow, ch),
                        ResampleMode::Bilinear => {
                            let sy = ((oy as f32 + 0.5) * h as f32 / oh as f32 - 0.5).max(0.0);
                            let sx = ((ox as f32 + 0.5) * w as f32 / ow as f32 - 0.5).max(0.0);
                            let (y0, x0) = ((sy as usize).min(h - 1), (sx as usize).min(w - 1));
                            let (y1, x1) = ((y0 + 1).min(h - 1), (x0 + 1).min(w - 1));
                            let (fy, fx) = (sy - y0 as f32, sx - x0 as f32);
                            let top = at(y0, x0, ch) * (1.0 - fx) + at(y0, x1, ch) * fx;
                            let bot = at(y1, x0, ch) * (1.0 - fx) + at(y1, x1, ch) * fx;
                            top * (1.0 - fy) + bot * fy
                        }
                    });
                }
            }
        }
        close(&got, &want, 1e-5, &format!("{mode:?}"));
    }
    assert!(g.nn.resample(&Resample { c, h, w, oh: 4, ow: 4, mode: ResampleMode::MaxPool(2) }, &g.buf(&x), &g.zeros(100)).is_err());
}

/// erf in f64 by its Maclaurin / continued-fraction-free series (the test's reference).
fn libm_erf(x: f32) -> f32 {
    let x = f64::from(x);
    if x.abs() > 5.0 {
        return x.signum() as f32;
    }
    let (mut term, mut sum, mut n) = (x, x, 0.0f64);
    while term.abs() > 1e-17 * sum.abs().max(1e-300) && n < 200.0 {
        n += 1.0;
        term *= -x * x / n;
        sum += term / (2.0 * n + 1.0);
    }
    (sum * 2.0 / std::f64::consts::PI.sqrt()) as f32
}

#[test]
fn groups_wrap_and_refuse_impossible_grids() {
    assert_eq!(groups_1d(16_777_216, 256).unwrap(), [32_768, 2]);
    assert_eq!(groups_1d(0, 256).unwrap(), [1, 1]);
    assert!(groups_1d(usize::MAX, 1).is_err());
}

/// The strided, batched matmul (fork G3.4): batched heads out of a fused buffer, both weight
/// layouts, alpha, bias, each activation and the residual epilogue.
#[test]
fn bmm_strided_batched_with_every_epilogue() {
    let Some(g) = gpu() else { return };
    // the 64 × 64 tile, then the 128 × 128 one
    for (m, n, k, batch) in [(67, 45, 21, 3), (203, 150, 37, 2)] {
        // a: batch b at offset 5 + b·k inside rows of 3·k + 7 floats
        let (a_ld, a_off) = (3 * k + 7, 5);
        let a = vals(a_off + (m - 1) * a_ld + batch * k + 8, 21);
        let w = vals(batch * n * k + 3, 22);
        let bias = vals(n, 23);
        let resid = vals(batch * m * n + 9, 24);
        for (kn, act, acc, with_bias, alpha) in [
            (false, Act::None, false, true, 1.0f32),
            (true, Act::GeluErf, false, true, 0.5),
            (false, Act::Relu, true, false, 2.0),
            (true, Act::GeluTanh, true, true, 1.0),
        ] {
            let s = Bmm {
                m,
                n,
                k,
                batch,
                a: Strided { off: a_off, ld: a_ld, bs: k },
                w: if kn { Strided { off: 3, ld: n, bs: n * k } } else { Strided { off: 3, ld: k, bs: n * k } },
                o: Strided { off: 9, ld: n, bs: m * n },
                w_kn: kn,
                alpha,
                bias: with_bias,
                act,
                accumulate: acc,
            };
            let mut want = resid.clone();
            for b in 0..batch {
                for i in 0..m {
                    for j in 0..n {
                        let mut v = 0.0f32;
                        for t in 0..k {
                            let av = a[a_off + b * k + i * a_ld + t];
                            let wv = if kn { w[3 + b * n * k + t * n + j] } else { w[3 + b * n * k + j * k + t] };
                            v += av * wv;
                        }
                        v *= alpha;
                        if with_bias {
                            v += bias[j];
                        }
                        v = match act {
                            Act::None => v,
                            Act::GeluTanh => gelu_tanh(v),
                            Act::GeluErf => 0.5 * v * (1.0 + libm_erf(v / std::f32::consts::SQRT_2)),
                            Act::Relu => v.max(0.0),
                        };
                        let oi = 9 + b * m * n + i * n + j;
                        want[oi] = if acc { resid[oi] + v } else { v };
                    }
                }
            }
            let o = g.buf(&resid);
            let step = g.nn.bmm(&s, &g.buf(&a), &g.buf(&w), Some(&g.buf(&bias)), &o).unwrap();
            close(&g.run(&[step], &o, resid.len()), &want, 2e-5, &format!("bmm kn {kn} {act:?}"));
        }
    }
    // out-of-range operands and missing bias are refused before any dispatch
    let (m, n, k) = (67, 45, 21);
    let s = Bmm::linear(m, n, k);
    assert!(g.nn.bmm(&s, &g.zeros(m * k - 1), &g.zeros(n * k), Some(&g.zeros(n)), &g.zeros(m * n)).is_err());
    assert!(g.nn.bmm(&s, &g.zeros(m * k), &g.zeros(n * k), None, &g.zeros(m * n)).is_err());
    assert!(g.nn.bmm(&Bmm { batch: 0, ..s }, &g.zeros(m * k), &g.zeros(n * k), Some(&g.zeros(n)), &g.zeros(m * n)).is_err());
    assert!(g.nn.bmm(&Bmm { a: Strided { off: usize::MAX / 2, ld: 1, bs: 0 }, ..s }, &g.zeros(8), &g.zeros(8), Some(&g.zeros(8)), &g.zeros(8)).is_err());
}

#[test]
fn pool2_rope_ew_and_zero_padded_windows() {
    let Some(g) = gpu() else { return };
    // pool2: 2 grids of 5 × 6 tokens, 3 channels at offset 4 in rows of 9
    let (batch, h, w, c, stride, off) = (2, 5, 6, 3, 9, 4);
    let x = vals(batch * h * w * stride, 31);
    let o = g.zeros(batch * 2 * 3 * c);
    let got = g.run(&[g.nn.pool2(batch, h, w, c, stride, off, &g.buf(&x), &o).unwrap()], &o, batch * 2 * 3 * c);
    let mut want = Vec::new();
    for b in 0..batch {
        for oy in 0..2 {
            for ox in 0..3 {
                for ch in 0..c {
                    let mut m = f32::MIN;
                    for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                        m = m.max(x[(b * h * w + (oy * 2 + dy) * w + ox * 2 + dx) * stride + off + ch]);
                    }
                    want.push(m);
                }
            }
        }
    }
    close(&got, &want, 0.0, "pool2");
    assert!(g.nn.pool2(1, 4, 4, 8, 8, 4, &g.zeros(256), &g.zeros(64)).is_err());
    // rope: 7 rows of 3 pairs at offset 2 in rows of 10, period 4
    let (rows, pairs, stride, off, period) = (7, 3, 10, 2, 4);
    let x = vals(rows * stride, 32);
    let ang: Vec<f32> = vals(period * pairs, 33).iter().map(|v| v * 3.0).collect();
    let (cs, sn): (Vec<f32>, Vec<f32>) = ang.iter().map(|a| (a.cos(), a.sin())).unzip();
    let xb = g.buf(&x);
    let got = g.run(&[g.nn.rope(rows, pairs, stride, off, period, &xb, &g.buf(&cs), &g.buf(&sn)).unwrap()], &xb, rows * stride);
    let mut want = x.clone();
    for r in 0..rows {
        for j in 0..pairs {
            let t = (r % period) * pairs + j;
            let i = r * stride + off + 2 * j;
            want[i] = x[i] * cs[t] - x[i + 1] * sn[t];
            want[i + 1] = x[i] * sn[t] + x[i + 1] * cs[t];
        }
    }
    close(&got, &want, 1e-6, "rope");
    // ew with a broadcast second operand
    let a = vals(30, 34);
    let b = vals(6, 35);
    let o = g.zeros(30);
    let got = g.run(&[g.nn.ew(30, 6, 1.0, 0.1, &g.buf(&a), &g.buf(&b), &o).unwrap()], &o, 30);
    let want: Vec<f32> = (0..30).map(|i| a[i] + 0.1 * b[i % 6]).collect();
    close(&got, &want, 1e-6, "ew");
    assert!(g.nn.ew(30, 0, 1.0, 1.0, &g.zeros(30), &g.zeros(1), &g.zeros(30)).is_err());
    // zero-padded windows: padding tokens are zeros, real ones normalised
    let (rh, rw, c, ws) = (5, 4, 8, 3);
    let x = vals(rh * rw * c, 36);
    let grid = WindowGrid { rh, rw, ws };
    let rows = grid.rows().unwrap();
    let o = g.zeros(rows * c);
    let s = LayerNorm { rows, c, eps: 1e-6, gather: Some(grid), channel_major: false };
    let ones = vec![1.0; c];
    let shift = vec![0.5; c];
    let got = g.run(&[g.nn.layer_norm_with(&s, true, &g.buf(&x), &g.buf(&ones), &g.buf(&shift), &o).unwrap()], &o, rows * c);
    let (nwx, _) = grid.windows();
    for row in 0..rows {
        let (win, t) = (row / grid.n().unwrap(), row % grid.n().unwrap());
        let (y, xx) = ((win / nwx) * ws + t / ws, (win % nwx) * ws + t % ws);
        let r = &got[row * c..(row + 1) * c];
        if y < rh && xx < rw {
            let mean = r.iter().sum::<f32>() / c as f32;
            assert!((mean - 0.5).abs() < 1e-4, "row {row}: mean {mean}");
        } else {
            assert!(r.iter().all(|v| *v == 0.0), "padding row {row}: {r:?}");
        }
    }
}
