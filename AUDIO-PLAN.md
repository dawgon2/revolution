# AUDIO-PLAN.md: audio good enough that it's never a reason not to use FilmCraft

Written 2026-10-12 (live session, with the owner). Goal: not Fairlight, but audio that plays
reliably on Linux and voice-over that sounds clean with little effort. Every FORK-PLAN rule
applies (never crash, `Tick` time, everything is a command, every widget has an automation id,
layering, `cargo xtask ci` per phase, a build and a `cachyos` publish per phase, §1a.9 / §1a.33).
Order: FORK-PLAN §1a.6 (A1 after U0.2; A2 after C2 / AI2 and before STORAGE-PLAN S1 (§1a.51), reusing AI-PLAN AI1's inference layer; A3 after G4).

## What already exists (audited 2026-10-12, don't rebuild it)

- `crates/audio-dsp` (L1, pure Rust): all 53 of Premiere's audio effects, including `denoise`
  (STFT spectral gating), `dehum`, `dereverb`, `deesser`, `speech_enhance` (a DSP chain, not a
  model), `dynamics_rack`; EBU R128 / BS.1770 loudness (oracle-tested against ffmpeg `ebur128`);
  ducking; `normalize_gain_db*`.
- `crates/engine`: Essential Sound (`essential_sound.rs`), voice-over recording with punch-in /
  out and pre-roll (`voiceover.rs`), text-to-speech narration (`narration.rs`), mixer graph,
  automation, 5.1.
- `crates/llm`: own quantized inference on the CPU (run-time SIMD, no build flag since AI1.2), the runtime
  A2 reuses.
- **Weak spots:** device output and playback (`docs/gaps.md` G17: #336 cut-outs with overlapping
  clips, #410, #602 no audio, #605 Xonar DX, #617 Linux stutter; cpal only); noise removal only
  handles steady noise; no track-level effects (#623), no buses, no clip-gain envelope on the
  timeline.

## Phase A1: playback reliability on Linux (≈ 20–40 h; owner: "definitely needs to be tackled")

1. Reproduce each report as a test where possible: overlapping clips, long timelines, scrubbing,
   J/K/L shuttle, seeks during playback, device unplug / default-device change, 44.1 vs 48 kHz
   devices, PipeWire (this PC) and plain ALSA. Check what U0.2 brought from upstream first.
2. Fix the causes, not symptoms: the mixer must never allocate or lock on the audio callback
   thread (lock-free ring buffer, pre-rendered look-ahead sized from the device's buffer); decode
   and effects run ahead on a worker; underruns are counted in `perf.stats` (`audio.underruns`)
   instead of being silent.
3. Device handling: the output device is chosen and remembered in Settings; device loss falls
   back to the default device and says so in the UI; sample-rate mismatch is resampled
   (`resample.rs`) instead of failing.
4. A/V sync: audio stays within ±1 frame of video during 2× / 4× playback and after 100 random
   seeks (measured from the clock, not by ear).

**Accept:** a 30-minute playback soak of a 20-track timeline with overlapping clips, shuttles and
seeks has **zero underruns** and drift ≤ 1 frame (synthetic, §1a.3); every reproducible report
above has a regression test; device loss recovers without a restart, simulated in software (owner can't unplug hardware, 2026-10-13, FORK-PLAN §1a.59): a PipeWire null sink removed mid-playback (`pactl unload-module`) and the real card's profile switched off and back on (`pactl set-card-profile`), both automated in the soak harness; playback falls back to the default device, the UI says so, and it returns when the device does. Tag
`audio-1`.

## Phase A2: voice isolation and one-click voice clean-up (≈ 35–70 h)

1. **Voice isolation (ML).** A pure-Rust port of **DeepFilterNet** (MIT / Apache-2.0, model and
   weights) on candle, as a new `voice_isolate` audio effect with an `amount` (dry / wet) control.
   Runs offline (background job, cached like the render cache) and, if fast enough on the
   i5-11600K, live during playback; the existing `denoise` stays the fallback when the model isn't
   installed. **Download consent (owner, 2026-10-12): yes, but only once A2 starts** — not
   before. The model is downloaded on first use from Settings ▸ AI models, checksummed, never
   committed or bundled.
2. **Voice-over clean-up (owner's condition: only if it's as good as doing it by hand).** One
   click on a voice clip runs analysis and **sets the ordinary, visible controls**: voice
   isolation amount, high-pass, de-ess, EQ, compressor, loudness to the delivery target (−14 LUFS
   integrated, ≤ −1 dBTP for YouTube). It is a starting point you can adjust, never a hidden
   chain. Per-take review: recorded takes are kept, shown as waveforms, and one is picked.
   **Gate:** build a test set of speech + noise mixes where the clean speech is known (our own
   TTS output and recordings, plus generated fan / traffic / keyboard / wind / room noise). For
   each, compare one-click against a hand-tuned chain made by following a written manual
   process: loudness within ±0.5 LU of target, true peak ≤ −1 dBTP, residual noise and speech
   distortion (SI-SDR against the known clean speech) **no worse than manual**. If one-click
   loses on any noise type, it ships only as "Suggest settings" for that case, or not at all;
   record the numbers in §7.

**Accept:** voice isolation beats `denoise` by a clear SI-SDR margin on non-steady noise
(traffic, keyboard, wind) and doesn't harm clean speech; the one-click gate above passes;
everything works through commands (`audio.voiceIsolate.*`, `audio.cleanupVoice`) and the
control channel. Tag `audio-2`.

## Phase A3: mixer basics (later; ≈ 20–40 h)

Track-level effects (#623), submix / buses with their own effects, clip-gain envelopes drawn on
the timeline, track meters with peak hold. Tag `audio-3`. **Moved before S1 (owner, 2026-10-15,
FORK-PLAN §1a.76).**

**Intuitive first (owner: "make sure it's relatively intuitive"):**
- A track's effects work like a clip's: drag an effect from the Effects panel onto a track header
  or a mixer strip; the same Effect Controls UI edits it. No separate effect workflow to learn.
- Meters are always visible (track headers and the mixer), with peak hold and a clear red at
  clipping; the master meter shows LUFS for the target loudness.
- Clip gain is a line on the clip you drag up / down (keyframes by click), like volume today.
- Buses: "Send to…" on a track creates or picks a bus; a new sequence has none until asked, so
  the simple case stays simple.
- Plain names, sensible defaults, tooltips that say what a control does; Premiere and Resolve
  workflows each lay it out the way that app's users expect.
- Done means: a guide page with options and trade-offs (beginner vs pro), a coach lesson, and the
  UI looked at on screen in both workflows.

**A3 audit (2026-10-21, scheduled run): most of the mixer already exists, A3 fills gaps.** Exists:
track inserts (`Track.effects`, 5 slots pre / post fader, `mixer.addInsert` / `setInsert` / `removeInsert`, Mix
inserts), submixes + sends (`mixer.addSubmix`, `mixer.addSend`, output routing with a no-feedback rule, 5.1),
static clip gain (`TrackItem.gain_db`, `clip.audioGain`) and the Volume rubber band on timeline clips
(`panels/timeline_volume.rs`), strip meters with peak hold and a clip light (`panels/mixer.rs`), the Audio Meters
panel with M / S / I LUFS and dBTP. Gaps, in build order:
1. **Meter reset:** click a strip's meter / clip light (mixer and Audio Meters) to clear peak hold and clip; UI test.
2. **Track-header meters:** a thin always-visible meter on each audio track header in the timeline (same levels
   as the mixer strip), red at clipping; UI test.
3. **Drag an effect onto a track:** dropping an audio effect from the Effects panel on a track header or a mixer
   strip adds it as the first free insert (`mixer.addInsert`); selecting a track header shows its inserts in
   Effect Controls / Inspector with the same controls as a clip's.
4. **"Send to…"** on a track header's context menu: pick a bus or "New bus…" (`mixer.addSubmix` + `mixer.addSend`).
5. **Done-means:** guide page (options + trade-offs, beginner vs pro), a coach lesson, screenshots in both workflows.
6. Voice Isolation quality (below).

**Voice Isolation quality (owner, 2026-10-15; OWNER-QUESTIONS Q30):** an amount / attenuation
limit (default lowered for mild noise, and clean speech left alone); re-measure on recordings of
the owner's own voice in a quiet room if they're in `~/.local/share/filmcraft-fork/voice-test/`
(LibriSpeech's "clean" files carry background noise that counts as damage); try a stronger openly
licensed model only after the owner's yes to its download. Aim: the two missed bars (≥ 3 dB over
DeNoise at 10 dB SNR, ≥ 20 dB on clean speech) without losing the noisy-speech gains.

## Not planned

Fairlight-level features (ADR / cue lists, immersive / Atmos, surround panning beyond 5.1,
hardware control surfaces), VST3 hosting (PLUGIN-PLAN, needs the owner).
