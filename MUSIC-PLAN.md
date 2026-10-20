# MUSIC-PLAN.md: make the song and its music video in one Revolution project

Written 2026-10-17 (live session, with the owner). Goal (owner): "my goal would be to seamlessly
create an entire music video in one project", meaning write, record, mix and master the song, cut
the video to it, and export a distributor-ready album master (Spotify via DistroKid / TuneCore
etc.) plus the video, all from one project file. Order: FORK-PLAN §1a.106. Every FORK-PLAN rule
applies (never crash, `Tick` time, everything is a command, every widget has an automation id,
layering, `cargo xtask ci` per phase, a build and a publish per phase, §1a.9 / §1a.33).

## Starting point: SoundCraft (audited 2026-10-17, don't rebuild it)

SoundCraft (`storytold/soundcraft`; the owner supplied `soundcraft-main.zip`, v0.7.0) is the
Pro Tools sibling in the same Craft family: MIT OR Apache-2.0, pure Rust, egui 0.36 (same as us),
same never-crash / command / agent rules. About 97 k lines. Its own rating is pre-alpha, ~40 %
ready for real work.

| SoundCraft crate | Lines | Use here |
|---|---:|---|
| `time` (Bars/Beats, `TempoMap`, meters) | 1.4 k | **take**: the tempo map, bridged to `Tick` |
| `dsp` (EQ, dynamics + brickwall limiter + maximizer, reverb, synths, drums, sampler, LUFS meter, SRC, dither) | 8.5 k | **take**; merge with `crates/audio-dsp` (one implementation per effect, oracle tests kept) |
| `mix` (inserts, sends, buses, VCAs, solo logic, latency comp.) | 3.4 k | **take** as the mixer engine (A3 builds on it) |
| `automation`, `midi` (SMF import/export, piano-roll model) | 2.7 k | **take** |
| `audio-io` (WAV/BWF/RF64/AIFF r/w, own FLAC encoder, symphonia decode) | 3.5 k | **take**; symphonia is already in our lock file |
| `clap-host`, `vst3-host`, `au-host` (plugin hosting, `unsafe`) | 10.6 k | **later** (M4); needs the PLUGIN-PLAN / platform-FFI exception decided first |
| `engine`, `model`, `playback`, `ui-egui`, `video`, apps | ~65 k | **reference only**: their commands and windows guide ours; our engine, project file, video stack and UI stay |

**SoundCraft's `video` crate is not imported.** It is Pro Tools' "Video track" for scoring to picture: decode one
MP4 / MOV for a Video window and thumbnails. Its H.264 / ProRes / MP4 code is an older copy of *our own* upstream
crates (its `lib.rs` says so); no HEVC, no hardware decode, no editing, colour or video export. Revolution's video
stack already does all of it, so the Music workspace instead shows Revolution's own Program monitor (dockable, like
Pro Tools' Video window) playing the video sequence the song is used in, at the song's playhead.

Missing in SoundCraft (its own gaps G-01 / G-02 / G-06): choosing the audio interface and buffer
size, record to disk (takes live in RAM), MIDI keyboard input. Also missing: album sequencing, a
true-peak (dBTP) meter, album-wide loudness matching.

## Design

- **One project, a new Music workspace** (beside Edit / Color / Audio; Premiere and Resolve
  layouts both get it). The Audio workspace stays the video-post mixer; Music is the DAW: tracks
  on bars and beats, MIDI, instruments, Mix window, mastering.
- **A song is a "music sequence"**: a sequence with a tempo map and MIDI / instrument tracks.
  It is used in a video sequence like a nested sequence (live, not a bounce), so editing the
  song updates the video's audio. "Bounce to clip" is optional (for speed).
- **Beat-aware video editing:** the song's bars / beats / markers show on the video timeline's
  ruler; snapping, ripple / roll / Rate Stretch and "cut on beats" (auto-edit to every N beats or
  to markers) use them. Lip-sync: performance clips line up to the song by audio sync (A1 sync)
  or by a clap / slate marker.
- **Time:** everything stays `Tick`. The tempo map converts bars / beats ↔ `Tick` exactly;
  samples only at the I/O edge (as today).
- **No mixer / DSP written twice** (same rule as §1a.14 for colour): A3's track effects / buses
  are built on SoundCraft's `mix`; Music uses the same mixer.
- **Crate placement:** imported crates become `crates/music-*` (`filmcraft-music-time`, `-dsp`,
  `-mix`, `-midi`, `-automation`, `-audio-io`) at L1–L2, no UI deps (`cargo xtask layers`);
  `ATTRIBUTION.md` / NOTICE credit SoundCraft (MIT / Apache notices kept). Generic, upstream-
  contributable; nothing fork-only.

## Phases

### M1: import the engine + Music workspace (≈ 40–70 h, tag `music-1`)
Import the crates above (renamed, clippy / never-crash clean under our lints, their tests kept),
the `Tick` ↔ tempo-map bridge, music sequences in the project file (schema bump, old projects
open), the Music workspace with tracks, waveforms, MIDI piano roll, built-in instruments, Mix
window; music sequence nested in a video sequence; beat ruler + snapping on the video timeline.
Done: build a short song from built-in instruments and imported loops, nest it in a video
sequence, cut to the beats, play both in sync; guide page + coach lesson; looked at on screen.

### M2: mastering + album export (≈ 20–35 h, tag `music-2`)
Mastering chain on the music master (EQ, multiband / bus compression, limiter, true-peak meter
with 4× oversampling, LUFS integrated / short-term / range). **Album:** an album bin orders
songs, sets gaps, matches loudness across tracks, holds per-song metadata (title, artist,
ISRC / UPC fields filled from the distributor, explicit flag) and 3000×3000 cover art. Export:
WAV 16/24-bit 44.1 / 48 kHz with dither, FLAC, one file per song + a cue / metadata sheet.
Presets with options + trade-offs (guide §1a: not prescriptive): "Streaming (-14 LUFS, -1 dBTP)",
"Loud master", "Keep dynamics". Oracle: ffmpeg `ebur128` (true peak + integrated) on exports.
Done: three-song album exports and passes a distributor's published spec checklist.

### M3: recording (≈ 55–90 h, tag `music-3`)
Interface + buffer choice and a duplex stream (PipeWire / ALSA via cpal first; JACK later),
input monitoring, record to disk with crash-safe takes, playlists / comping, latency
compensation measured with a loopback test; MIDI keyboard input (ALSA seq / midir-style pure
Rust) with live instrument play. Done: record a vocal over a beat with a USB interface and a
MIDI keyboard part, no dropouts in a 10-minute soak; synthetic tests + owner's hardware check.

### M4 (later): third-party plugins (≈ 20–40 h)
CLAP first, then VST3, out of process or in the `platform` exception per PLUGIN-PLAN; LV2 is
the Linux-native option to weigh.

## Not in scope
Score / notation, Dolby Atmos music mixing, collaboration, Pro Tools session interchange (AAF
/ OMF stays EXPORT-PLAN's).
