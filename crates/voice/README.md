# filmcraft-voice

Voice isolation for FilmCraft (fork AUDIO-PLAN A2): a pure-Rust port of
[DeepFilterNet3](https://github.com/Rikorose/DeepFilterNet) (MIT / Apache-2.0). The model weights
are never bundled or committed; they are downloaded on first use (a later A2 step).

- `fft::MixedFft`: complex FFT for `2^p × m` sizes (`m` odd, ≤ 63); DeepFilterNet uses 960 points.
- `dsp`: the model's signal path around the network, ported from `libDF` so the network sees the
  features it was trained on: `Stft` (Vorbis window, 960 / 480, perfect reconstruction one hop
  late), `erb_widths` (32 ERB bands), `Features` (ERB dB with exponential mean normalisation,
  complex bins with unit normalisation), `apply_band_gains`, `deep_filter` (order-5 complex
  filter over the last frames), `DfParams::DFN3`.

- `weights` (feature `dfn`): the released checkpoint (`model_120.ckpt.best`, PyTorch zip) read
  with candle's restricted pickle reader under `filmcraft_ml::job::guarded`, every tensor checked
  against `manifest()` (name, exact shape, finite values); BatchNorm as a per-channel affine.
  `FILMCRAFT_DFN3_CKPT=<path> cargo test -p filmcraft-voice --features dfn` also checks the real
  file (its ERB filterbank must match `dsp::erb_widths`).

- `net` (feature `dfn`): the network, one 10 ms frame at a time, as plain-Rust f32 kernels on
  preallocated buffers (candle only reads the checkpoint: per-frame candle ops cost more than
  the maths at this size). `denoise::Denoiser` runs 1–8 48 kHz channels like `libDF`'s defaults
  (30 ms latency); channels share one network step on their mean, the mask applies to each. Parity with DeepFilterNet's own `deep-filter` (built from source as an external
  oracle, `FILMCRAFT_DFN_ORACLE`): 68.2 dB, i.e. identical to 16-bit precision. Real-time factor
  0.071 on one core of the Dell's i5 (AVX2), `rtf` test; inside the app's mixer about 0.3 per
  stereo track on the i5-13500T's slower cores (A2.6 soak: one track 0 underruns in 5 min).

- `effect` (feature `dfn`): the `voice_isolate` audio effect (Amount = dry / wet, 1920 samples
  latency, any block size, no allocation per call, one shared `Denoiser` for all channels). `effect::install(path)` loads the model and
  registers it with `filmcraft_audio_dsp::register_external`; without it, or at sample rates other
  than 48 kHz, `voice_isolate` is audio-dsp's DeNoise fallback.

- `kaldi` (fork AUDIO-PLAN A3.7, no feature): the MossFormer2_SE_48K front end and STFT: Kaldi
  60-bin log-mel filterbank (40 ms Hamming frames every 8 ms, pre-emphasis 0.97, FFT 2048, seeded
  dither 1.0), torchaudio's `compute_deltas` (Δ, ΔΔ → 180 channels), 1920 / 384 symmetric-Hamming
  STFT / iSTFT without centring. Parity with an independent numpy implementation of the Kaldi /
  torchaudio definitions (`fbank_oracle_dump`, ignored): max abs error 1.2e-5 (fbank), 3.4e-6 (Δ).

- `mossformer` (feature `dfn`): MossFormer2_SE_48K (ClearerVoice-Studio, Apache-2.0, 55 M
  parameters): `MossFormer2::load` reads the official `last_best_checkpoint.pt` (929 tensors, shape
  and finite checks, ≤ 512 MB), `enhance` runs whole channels offline (≤ 20 s in one pass, longer in
  4 s windows every 3 s; the mask of the channels' mean applies to each). candle does the matrix
  products, the rest is plain Rust. Parity with a numpy implementation of the PyTorch modules
  (`mask_oracle_dump`, ignored): max abs mask error 6.8e-6. About 0.2–0.45 s of CPU per second of
  audio on the i5-11600K (all cores). Used by the engine's rendered "Better" voice isolation
  (`audio.voiceIsolate.render`); `effect::isolate_offline` is the DeepFilterNet3 equivalent.

Every function checks its lengths and returns `None` / `false` instead of panicking; Inf / NaN
input never latches into the running normalisation state. No `unsafe`; without `dfn` no dependencies.
Plan and step map: [`docs/fork/PREP-A2.md`](../../docs/fork/PREP-A2.md).
