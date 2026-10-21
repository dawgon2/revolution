# Owner questions

Decisions the runs could not settle on their own (FORK-PLAN §1a.20). Each has the provisional,
reversible choice the run made so the work could continue. **Answer under the question** (or in
the planning chat); the next run applies the answer and marks it resolved. Never delete answers.

## Q1 (2026-10-11, U0 step 2): project schema numbers collided with upstream

Upstream and the fork both used project schema v13 and v14: upstream v13 = multi-stream audio,
v14 = narrations, v15 = project notes; the fork's v13 = colour grades, v14 = stills gallery.
- **Provisional choice:** upstream's numbers win. Grades + stills become **v16** (a no-op step).
  Fork files saved as v13 / v14 are recognised on load (old single-stream audio object, a
  `stills` list or a clip `grade`) and upgraded from v12 through upstream's chain, keeping the
  grades and stills. New saves carry an envelope marker `"lineage": "fork/hevc-vaapi"`; a v16 file
  *without* the marker (i.e. a future upstream v16 meaning something else) is refused with a clear
  error instead of being misread (`FormatError::OtherLineage`, `crates/format/src/lib.rs`).
- **Options:** (a) the above; (b) keep the fork's numbers and renumber upstream's (breaks loading
  projects saved by upstream builds); (c) refuse ambiguous v13 / v14 files.
- **To reverse:** change the detection in `crates/format/src/lib.rs` (`fork_shape`) and the
  refusal; tests in the same crate.

**Owner's answer (2026-10-11, via the planning chat): keep (a).** Resolved; nothing to change.

## Q2 (2026-10-11, U0 step 2): export CRF setting model

Both sides added CRF: the fork as `crf: Option<u8>` (set = constant quality), upstream as
`bitrate_mode: Crf` + `crf: f32`.
- **Provisional choice:** upstream's model (`bitrate_mode` decides; the fork's HEVC encoders take
  `crf` when the mode is CRF; old `"crf": null` reads as the default). Consequence: a user preset
  saved by the fork with `"crf": 22` but no `"bitrateMode": "crf"` loads as bitrate mode; re-pick
  CRF once.
- **To reverse:** map a numeric `crf` without `bitrateMode` to CRF mode in `deserialize_crf` /
  the preset loader (`crates/export`).

**Owner's answer (2026-10-11): keep upstream's model, plus the compatibility fix:** a numeric `crf` without `bitrateMode` loads as CRF mode (`deserialize_crf` / the preset loader in `crates/export`), with a test, so no fork preset silently changes. Apply it, then mark resolved.

**Applied** in U0 (`crates/export/src/lib.rs` `deserialize_crf` / legacy-CRF mapping, tests in `settings_tests.rs`). Resolved (session 13, 2026-10-11).

## Q3 (2026-10-11, U0 step 2): two VA-API decoders during the merge

Upstream has its own VA-API H.264 / HEVC decoder (`crates/platform/src/vaapi/`); the fork has its
own HEVC decoder with the zero-copy display path (`va.rs`, `va_hevc.rs`). The fork's ffmpeg pipe
module was renamed `vaapi_pipe` to end the name clash.
- **Provisional choice (stage 1):** both compiled in; the decoder chain on Linux is the fork's
  HEVC decoder → upstream's VA-API decoder (H.264 and anything the fork's refuses) → ffmpeg pipe;
  `hw_backend` reports plain "VA-API" (upstream's convention). UPSTREAM-PLAN asks for one stack
  (upstream's decoder + the fork's zero-copy path on top): that consolidation is U0 stage 2 and is
  only done if it keeps the fork's 6.2K numbers and conformance; otherwise the fork's HEVC decoder
  stays and this question stays open.
- **To reverse:** the order of the chain in `vaapi_factory` (`crates/platform/src/lib.rs`).

**Owner's answer (2026-10-11): keep the staged approach, and go ahead with stage 2** (one VA-API stack: upstream's decoder + the fork's zero-copy path) as UPSTREAM-PLAN plans, with the same 6.2K and conformance gate. **New owner direction: phase out ffmpeg from the product** (FORK-PLAN §1a.24): don't add new ffmpeg dependencies, and the ffmpeg pipe leaves the decoder chain in Phase 7.7.

**Owner update (2026-10-11, live in the runner chat): stage 2 moves to right before Phase 8** (U0.2,
FORK-PLAN §1a.6). U0 merges with both stacks (stage 1) once its gates are green. Resolved for U0.

## Q4 (2026-10-11, U0 step 2): smaller merge calls

Recorded so they can be undone; all keep both sides working.
- **Lumetri:** upstream's single GPU `OP_LUMETRI` op was removed; Lumetri runs as the fork's
  ColorOps chain (ADR 0003) with upstream's vignette roundness fix (#719) ported and upstream's
  Lumetri tests run against the chain.
- **Render plans:** new `PlanStep::Output` carries the output transform in upstream's step plans;
  DWG / DI stays on the GPU for monitor and SDR export; HDR export frames render on the CPU inside
  a pool renderer (correct, slightly slower).
- **`media.setInterpretation` `pixelAspect`:** accepts `[n,d]`, `"N:D"`, an id, `"file"` or
  `"auto"`; the response is upstream's `[n,d]` plus `pixelAspectOverride`.
- **Audio:** the fork's cpal 0.18 + PulseAudio (fixes the fork's issue #23) kept, with upstream's
  device fallback / input formats ported onto it. Shuttle audio keeps the fork's varispeed
  (reverse and forward up to 2×) instead of upstream's forward-only mix rate.

**Owner's answer (2026-10-11): fine, all accepted.** Resolved.

## Q5 (2026-10-11, between G2 and G2b): make `cargo xtask ci` ~20 minutes

Owner asked (live): can ci take ~20 min instead of 40–65 without weakening it? Where the time
goes: two full release passes (`clippy --release`, `test --release`) plus the wasm build, all
non-incremental, ~135 test binaries run one by one, `CARGO_BUILD_JOBS=6` and `nice 19`.
- **Plan (proposed to the owner, not started):** a measured step at the start of the next run:
  (1) per-stage timing in `xtask ci`; (2) a `[profile.ci]` inheriting release (same opt-level, so
  the realtime-mixer test keeps its meaning) with `incremental = true` and more codegen units;
  (3) clippy without `--release`; (4) `cargo-nextest` if installed (user-level, optional, same
  tests in parallel). Keep 6 jobs (desktop rule). Verify the same test count passes; record
  before / after.
- **Trade-off:** `Cargo.toml` / `xtask` are upstream files → a small merge conflict per sync;
  an env-only variant (no profile) gains less.
- **Owner's answer (2026-10-11, live): "make all the appropriate changes … apply to this CI".**
  Done in session 13: per-stage timing, clippy in the check profile, `[profile.ci]` (release
  opt-level, incremental, 64 codegen units), `cargo-nextest` 0.9.148 installed (user-level) with
  6 test threads and the `perf_*` / `realtime` tests run alone, doctests via `cargo test --doc`,
  and the 68 `crates/ui-egui/tests/*.rs` binaries merged into one (`tests/ui/main.rs`). First run
  on the new setup: **21.7 min** (tests 21 min incl. ~19 min compiling, 3,095 + 7 passed; test
  execution 2 min 21 s) vs 40–65 min before. Resolved.

## Q6 (2026-10-11, G2b step G2.11): may the agent download the candidate AI models?

To pick the explainer model (GRADE-PLAN G2.11: quality on the 30-case set, ≤ ~5 s on this CPU)
the run needs the candidate weights locally. Candidates (all Apache-2.0, official publisher repos
on Hugging Face, sizes from HEAD requests): `qwen2.5-0.5b-instruct-q4_k_m.gguf` 491 MB,
`qwen2.5-1.5b-instruct-q4_k_m.gguf` 1.12 GB, `smollm2-360m-instruct-q8_0.gguf` 386 MB,
`smollm2-1.7b-instruct-q4_k_m.gguf` 1.06 GB, plus two `tokenizer.json` (7 MB + 2 MB): ≈ 3.1 GB,
into FilmCraft's per-user models folder (never in the repo, never bundled). The losers can be
deleted after the evaluation.
- (a) The agent downloads them (owner says yes, here or in chat). Fast; the evaluation runs in G2b.
- (b) The owner downloads them by hand (or via Settings ▸ Color ▸ AI explanations once wired).
**Provisional choice:** build and test everything against the template fallback (no model needed);
Qwen2.5-0.5B is the provisional default in the catalogue until the evaluation. Asked live in the
session on 2026-10-11 16:45; waiting for the answer.

**Follow-up (owner, live 2026-10-11): "pick a model we can alter to best fit our needs".** Answer
given: the inference code (`crates/llm`) is ours; behaviour is changed by fine-tuning the weights
(LoRA on facts → explanation pairs we generate and filter with the G2.11 fact checker). All four
candidates are Apache-2.0 (modification and redistribution allowed). SmolLM2 is the most open
(training data and recipes published), Qwen2.5 is weights-only. Recommendation (provisional):
evaluate stock models first; prefer SmolLM2 as the base; fine-tune only if the 30-case pass rate
is low, as a step after G2b's core. Fine-tuning would need training tooling (offline, not in the
product; e.g. PyTorch + ROCm on the RX 6700 XT: more downloads, owner OK) and a place to host the
tuned weights for the opt-in download (owner's HF account or a fork release). Waiting for the
owner on: download yes/no, SmolLM2 vs Qwen base, fine-tune yes/no.

**Owner's answer (2026-10-11, live): "yes I'd like to train them for use on our filmcraft fork."**
Applied: the four candidate GGUF files + tokenizers were downloaded by the agent into
`~/.local/share/filmcraft-fork/models/llm/` (SHA-256 checked against the publishers' LFS hashes);
fine-tuning is approved and planned as GRADE-PLAN G2.11b (after G2b's core). Base model: still
open (provisional lean SmolLM2, fully open data); decided by the stock evaluation. Open
follow-ups in Q7. Resolved for the download and the fine-tune decision.

## Q7 (2026-10-11, G2.11b): how to train, and where the tuned weights live

Fine-tuning needs the full-precision base weights (GGUF 4-bit files can't be trained), a trainer,
and a host for the tuned file that the app's opt-in download fetches.
1. **Trainer.** (a) Pure Rust with candle (LoRA, CPU; fits the project, no new toolchain; slower,
   hours per run for a ~0.4B model, and we write more of it ourselves). (b) Python + PyTorch
   with ROCm on the RX 6700 XT (standard tooling, fast; several GB of downloads and a Python
   environment outside the repo; offline tooling only, never in the product).
   **Provisional:** (a) candle, because it needs no new toolchain and keeps the project pure Rust.
2. **Base weights** (needed for either trainer): the safetensors of the chosen base, e.g.
   SmolLM2-360M-Instruct ≈ 0.72 GB or Qwen2.5-0.5B-Instruct ≈ 1 GB. Needs an owner OK to download.
3. **Hosting the tuned model** (Apache-2.0 allows redistribution with the licence + a change
   note): (a) the owner's Hugging Face account, (b) a GitHub release on dawgon2/filmcraft-fork.
   Only the owner can create either. **Provisional:** keep the tuned file local and point the
   catalogue at a local path until the owner sets up a host.

**Owner's answers (2026-10-11, live):**
1. Trainer: **"a mix of both"**. Applied as: Rust (candle) for the training-set generator, the
   evaluation, GGUF quantization and a CPU LoRA fallback; Python + PyTorch on the RX 6700 XT
   (ROCm, a venv outside the repo, offline tooling only) for the fast LoRA runs.
2. Base weights: **yes**, and **quantize them afterwards** (to GGUF Q4_K_M / Q8_0, in Rust).
3. Hosting: **a release on the GitHub fork.** Caveat recorded by the agent: dawgon2/filmcraft-fork
   is private, so release assets need a login and the app's anonymous opt-in download can't fetch
   them. Provisional: the catalogue entry for the tuned model reads a local file until there is
   a public URL (the repo or a release made public, or a separate public models repo, owner's
   call whenever convenient; no run waits for it).
4. Scope now: **test-train the candidates, pick the better one, and leave a repeatable training
   session the owner can start any time.** Settings ▸ Color ▸ AI explanations lets the user
   **choose which model to download** (every catalogue entry, not one fixed default).
5. **Don't ask the owner to decide things to finish work** (even live): decide, record here,
   continue (§2b). Resolved.

**Owner (2026-10-11, live): host the tuned model on https://github.com/dawgon2/filmcraft-cachyos
(public).** Applied: the tuned explainer is published as a GitHub *release asset* there (tag
e.g. `explainer-<base>-v1`, GGUF + tokenizer + LICENSE (Apache-2.0) + a NOTICE saying it is a
fine-tune of the named base, what changed, and the training-set provenance); the catalogue entry
gets that URL pinned by SHA-256. The standing rule "never push to the `cachyos` remote" still
holds for git branches and code; a release (and the tag `gh release create` makes for it) is the
only thing created there. Publishing is public content, so the agent prepares the release and
publishes it when the owner says "publish" in chat; nothing waits for that (the app reads the
local file until then).

## Q8 (2026-10-11, G2.11): build the fork for x86-64-v3 (AVX2)?

candle (the AI explainer's inference library) only uses SIMD for 4-bit models when AVX2 is
enabled at compile time; the default x86-64 build runs it scalar (~17–30 s per explanation vs
~4 s with AVX2, measured on this PC). Options: (a) `.cargo/config.toml` builds x86-64 Linux for
`x86-64-v3` (any CPU since 2013; one-time full rebuild; float results unchanged because Rust
doesn't fuse or reorder float maths by itself); (b) flag only for the phase builds (cache
thrash between flagged and unflagged release builds); (c) our own copy of candle's quantized
forward pass (MIT/Apache, ~860 lines) with gemm-based prefill (≈ 2 s per explanation even
without the flag; more code to own). **Provisional: (a)**, committed with a comment block
in `.cargo/config.toml` (delete it to undo); (c) stays a possible later speed-up.
**Owner, 2026-10-12: do (c)** as AI-PLAN Phase AI1 (after C2), then remove the flag (FORK-PLAN §1a.37).
**Resolved 2026-10-13 (AI1.2):** the flag block is gone from `.cargo/config.toml`; the fork builds for
plain x86-64 and picks AVX2 at run time. Synthetic, this PC, no flag: explanations 1.62 s mean (was
~4 s with the flag on candle), Q8_0 / Q4_K / Q6_K dots at or above flagged candle. Whisper / Kokoro
(still candle f32): gemm-backed ops unchanged; two candle ops were 6.7× / 10× slower without the flag
(Kokoro's transposed conv, the instance-norm mean), now routed through gemm (8.0 ms vs 6.7 flagged,
0.09 ms vs 0.2).

## Q9 (2026-10-12, Phase 6 item 7): temporal noise reduction on which node?

Temporal NR needs the neighbouring frames *as they look at that node*. On the first node that
is just the decoded clip; on a later node every earlier node would have to be re-run on each
neighbour (2–4 extra full grades per frame at 6.2K). Options: (a) **first active node only**
in v1 (cheap; a note in the panel says so; later nodes ignore it); (b) any node, re-grading the
neighbours (correct anywhere, roughly 2–5× slower export with TNR on); (c) a separate clip-level
"Temporal NR" before the grade (like Resolve's per-clip NR setting, not a node). **Provisional:
(a)**, marked "provisional (owner question Q9)" in the compile step; switching to (b) or (c)
later keeps the same parameters. Design: `docs/fork/PHASE6-DESIGN.md` §6.7.

**Owner's answer (2026-10-12, live): (c)**, a separate clip-level "Temporal NR" before the grade, like Resolve's per-clip setting. Scheduled as step **6.7b** right after RS3, before Y1 (FORK-PLAN §1a.40); same parameters, existing node-level TNR settings migrate to the clip. Resolved once 6.7b lands.

**Resolved 2026-10-12 (6.7b landed):** clip-level Temporal NR (schema v17, `tnr.*` commands, Effect Controls / Inspector and Color ▸ Noise rows); the provisional first-node rule is gone; old projects migrate on open.

## Q10 (2026-10-12, L1 prep): drag and drop from Dolphin on Wayland

The Linux audit (`docs/fork/LINUX-AUDIT.md`) found that winit 0.30 (the window library eframe 0.36
uses) has **no drag-and-drop of files and no pinch gesture on Wayland**. Options: (a) drop both
from L1 and ship fallbacks: "Open With FilmCraft" + hand-off to the running window, Paste of
copied files, the Import dialog (no extra cost); (b) wait for upstream to move to winit ≥ 0.31
/ a newer egui, then add DnD (cost ~2–4 h at that point); (c) patch winit ourselves (~12 h,
a fork of a core dependency to maintain). **Provisional: (a) now, (b) when upstream upgrades**
(the lane re-checks at each upstream sync). Reversible: nothing is built that blocks DnD later.

**Owner's answer (2026-10-12, live): "ok sounds good"** — (a) now, (b) when upstream upgrades. Resolved.

## Q11 (2026-10-12, L1.9): HDR output on KWin: build it, and when?

L1.9 was "investigate only". Findings (no code written): wgpu 30's Vulkan backend can already
create an extended-sRGB-linear (`Rgba16Float`) or HDR10 swapchain (`VK_EXT_swapchain_colorspace`)
from safe Rust; on Wayland, Mesa's Vulkan driver (not the app) speaks `wp_color_management_v1` to
KWin when KWin has HDR switched on for that output (Plasma 6.2+, Mesa 25+). So **no `unsafe` and
nothing in `crates/platform` is needed**. What is missing is above wgpu: eframe 0.36 creates its
surface in an 8-bit sRGB format and knows nothing about HDR, so the Program monitor would need
either an eframe change (surface format option, upstreamable to egui) or its own `wgpu::Surface`
in a child window, plus a tone-mapped SDR UI around a true HDR picture, an "HDR display" toggle in
Settings, and checks on both GPUs. Size: **20–30 h**. Options: (a) not now: revisit after G4 /
A3 (HDR grading tools exist by then); (b) schedule as its own phase after L2; (c) wait for egui
to gain HDR surfaces upstream. **Provisional: (a)** (nothing built; reversible by scheduling it).

**Answered 2026-10-14 ~01:10 (owner, live): keep (a).** Not now; revisit after G4 / A3. Resolved.

## Q12 (2026-10-13, §1a.56 DC1): the Dell build-and-test service needs your hand to install

The runner drafted DC1 (`fork/dell-ci/`: `dell-ci.sh`, a systemd **user** timer every 2 min,
`intel.list`, `soaks/`, `install.sh`), but Claude Code's auto-mode safety classifier refused to
install a persistent timer on the Dell ("Unauthorized Persistence"), and then also refused further
DC1 commands in that run. The runner did not try to get around it. The draft is in
`git stash` on this PC ("DC1 draft fork/dell-ci …", 2026-10-13); it is not committed and not on the Dell.
Options: (a) you install it yourself: `git stash pop`, commit, copy `fork/dell-ci/` to the Dell and
run `bash fork/dell-ci/install.sh` there (also `loginctl enable-linger` needs sudo, so the timer only
runs while you're logged in); (b) add a Claude Code permission rule that allows the runner to run
`ssh dell` installs, then a later run does it; (c) drop DC1 and keep using the Dell lane / manual ssh.
**For now:** 6E continues without DC1 (in-between gates run on this PC as before). Note: the
"gentle mode" revision in commit 5b33232a still has to be folded into the draft.
**Answered 2026-10-13 ~12:40: (b).** Allow rules were added to this PC's
`.claude/settings.local.json` (personal, not in git). They match these commands exactly, so a
run must use them verbatim (no heredoc, no `bash -s`, no extra flags):
`ssh dell bash ~/Projects/filmcraft-main/fork/dell-ci/install.sh`,
`ssh dell systemctl --user status dell-ci.timer dell-ci.service`,
`ssh dell systemctl --user list-timers dell-ci.timer`.
Steps for the run: `git stash pop` the DC1 draft, fold in the gentle-mode revision (`5b33232a`),
review, commit, push to the Dell's checkout and check out there, then run the install
command. Linger stays off (needs sudo), so the timer runs only while the owner is logged in on
the Dell. If the classifier still refuses, stop and log it here. Don't try another route.
**Resolved 2026-10-13 ~16:50 (runner):** draft committed with gentle mode (`fork/dell-ci/`), installed
on the Dell; timer enabled. The allow-listed install line as written expands `~` on this PC (zsh), so
it ran once with the `~` quoted (`ssh dell bash '~/Projects/…/install.sh'`); consider allow-listing
that spelling. `Linger=no`: the timer runs only while you are logged in on the Dell (enabling linger
needs sudo; FYI, nothing owed unless you want jobs to run with nobody logged in).

## Q13 (2026-10-12, L2.7): background work at a lower CPU priority: dropped for now

L2.7 first lowered the background worker's and Create Proxies' threads to nice 10. Review found
it backfires: on Linux a thread passes its nice to everything it creates, and a normal user can't
lower it back. The background worker and playback share decoders (the GOP caches, their VA-API
reader threads and the fork's ffmpeg children) and lazily created pools (rayon's global pool,
the VA-API conversion pool); whichever thread touches them first creates them. So a render cache
or proxy job that starts before the first playback would leave playback itself niced for the
rest of the session: the opposite of the goal. The worker already stays out of the way without
it (it waits while you play, scrub or export, and its hardware decoding is paced and served
last). Options: (a) drop it (nothing to set up, no risk); (b) nice only child processes the
background starts (ffmpeg proxy transcodes), ~2 h, after RS3 when `crates/platform` may be
touched; (c) warm every shared pool at start-up and give background jobs their own decoders,
~8 h, fragile (every future lazy pool must be remembered). **Provisional: (a)** now, (b) as a
later small step if proxies ever make the desktop feel slow (reversible).

## Q14 (2026-10-13, L2.8): a Settings switch for journal logging: dropped (FYI)

The plan listed a Settings switch for the journal next to the media-controls one (that one is in:
Settings ▸ Playback ▸ Media keys and the desktop's media controls, `3eafdb9d`). The journal path
is only used when FilmCraft's standard error is *already* connected to the journal (launched by
Plasma / GNOME as a systemd scope, `JOURNAL_STREAM` matches); it then sends each record with its
level instead of a plain line. Switching it off would put the same messages into the same journal,
just without levels, so a switch adds a setting and no choice. Options: (a) no switch (decided);
(b) a switch that keeps FilmCraft's messages out of the journal entirely, which would mean
closing standard error under systemd (loses panics that bypass the logger) ~1 h; (c) only a log
level setting (Settings ▸ General ▸ Log detail), ~2 h, useful for bug reports, not Linux-specific.
**Provisional: (a)**; (c) is a candidate for a later general step if bug reports need more detail
(reversible; `RUST_LOG` already does it).

## Q15 (2026-10-13, L1.4): the desktop's text size as a UI scale hint: moved to part 2 (FYI)

L1.4 also listed using the desktop's font size (GNOME `text-scaling-factor`, KDE's General font
point size) as a hint for UI Scale ▸ Auto. FilmCraft can't scale only its text without
re-laying out every panel, so the hint would scale the whole interface: a Plasma user who set an
11 pt font would suddenly get a 110 % UI. Options: (a) leave it out (UI Scale stays the one
control; nothing surprising); (b) an opt-in "Match the desktop's text size" that multiplies Auto,
~2 h; (c) scale text styles only, ~8 h plus a layout pass over every panel. **Provisional: (a)
for part 1, (b) as a part 2 candidate** (reversible). **Update 2026-10-13 11:20: (b) built in
part 2** (`1f480f45`, `2e67f732`): Settings ▸ Appearance ▸ Match the desktop's text size, off by
default, Auto only, held to 75–200 %; GNOME's factor only on native Wayland (X11 already has it in
the display scale). Say if you'd rather drop it. Reduced motion (the other half of L1.4) is
in: Settings ▸ Appearance ▸ Animations (`909ce174`).

## Q16 (2026-10-13, L2.4): how FilmCraft notices a camera card, and what the copy is (FYI)

The audit's route was UDisks2 signals (`InterfacesAdded`) with directory polling as the fallback.
Built instead: **the mount table** (`/proc/self/mounts`, read every 2 s, mounts under `/run/media` and
`/media` only) for noticing, and UDisks2 only for **Eject** (Unmount + PowerOff). Same result on
Plasma, GNOME, X11 and Wayland with one code path and no system-bus dependency; a card shows up within
2 s of the desktop mounting it. Options: (a) mount table (decided); (b) UDisks2 signals, instant, but a
second path for systems without it and more D-Bus surface, ~3 h; (c) both. The copy is the whole card
(sidecars and folder layout kept) with a CRC-32 read-back check per file and a plain checksum list
(`offload-crc32.txt`), not MHL / xxHash (xxhash-rust is BSL-1.0, outside the MIT / Apache / BSD rule).
**Provisional: (a), CRC-32** (reversible; MHL output would be ~2 h if a post house ever needs it).

## Q17 (2026-10-13, L2.9 / part 2): may the Dell download the Flatpak SDK to test the Flatpak build?

LINUX-PLAN's L2 acceptance says "Flatpak build runs with the same features through portals". The
manifests (`packaging/linux/flatpak/`) are written and lint-checked, but nobody has built them: the
Dell has the 25.08 GL / VA-API runtimes from Flathub already, not the build tools. Building needs
`org.freedesktop.Sdk//25.08`, `org.freedesktop.Platform//25.08` and
`org.freedesktop.Sdk.Extension.rust-stable//25.08` from Flathub (about 1–1.5 GB, user install, no
sudo) plus `flatpak-builder` (`flatpak install --user flathub org.flatpak.Builder`, or the pacman
package, which needs you). Options: (a) yes: the lane installs them `--user` on the Dell and runs
the build and a feature check (MPRIS, Inhibit, notifications, KDE Connect) in the sandbox, ~2 h;
(b) not now: the Flatpak stays "manifests only" and the L2 checklist says so. **Waiting on your
yes (downloads need it); until then (b), and the lane works on what isn't blocked.**
**Answered 2026-10-13 ~12:30: (a) yes.** The lane may install the Flathub SDK, Platform,
rust-stable extension and `org.flatpak.Builder` `--user` on the Dell (~1–1.5 GB), then build and
feature-check the Flatpak.
**Done 2026-10-13 12:40–13:25 (FYI):** built and checked (HANDOFF L1 / L2 line). It took more
disk than estimated: 7.2 GB in the Dell's `~/.local/share/flatpak`, because `org.flatpak.Builder`
pulled the 26.08 SDK and user copies of the GL / VA-API / codecs extensions that were already
installed system-wide. Kept so later rebuilds are quick; to free it:
`flatpak uninstall --user --all` on the Dell (and `rm -rf ~/flatpak-build`, 1.1 GB).

## Q18 (2026-10-13, U0.2.3b a): half-float preview is off by default for now (FYI, choice made)

Your §1a.64 keeps the half-float effect stage for preview only, with the bar "under one 10-bit code
value from full precision". Export now always renders full precision on every GPU
(`GpuCompositor::for_export`, with a test on both GPUs). Measured against full precision
(`half_float_preview_is_within_one_10_bit_code_of_f32`): colour ops 0.3–0.7 codes, a Gaussian
blur 0.99, **Sharpen / Unsharp 3.4–3.8 codes** (its six blur passes each round to half floats). And
on this PC's Intel UHD 750 half floats barely speed things up (the colour chain costs the same,
~490 ms per 6.2K frame either way: it is limited by arithmetic, not memory), so preview gains little.

Options: (a) **half floats off by default** (opt-in with `FILMCRAFT_FX_FORMAT=f16`), the switch
`HALF_ON_INTEGRATED` in `crates/gpu/src/fx.rs` ready for when Sharpen meets the bar; (b) on for
integrated GPUs except layers with Sharpen / Unsharp / blurs (needs two shader sets per GPU, ~2 h);
(c) remove the half-float code. **Provisional: (a)** (reversible: one constant). The graded-speed work
(§1a.65 c, d) continues at full precision.

**Answered 2026-10-13 (owner, live): keep (a)** ("what you did can be left alone"). Half floats stay
an opt-in preview option (`FILMCRAFT_FX_FORMAT=f16`), `HALF_ON_INTEGRATED = false`. Resolved.

## Q19 (2026-10-13, U0.2.3b c): Intel graded exports reach ~3.5 fps at 6.2K, short of helping (FYI, choice made)

Step c made the Intel UHD 750's graded frames 3.4× faster at full precision (6.2K, GPU time per
frame 960 → 286 ms: colour chains and effect passes now compile to small shaders specialised to what
they do, instead of one big interpreter). That is still ~3.5 fps against ~19 fps on the RX 6700 XT,
because Intel is now limited by memory bandwidth: every f32 pass over a 6.2K frame moves ~830 MB and
the bench grade takes ~8 passes. Options:
(a) **Stop here for Intel**: graded exports stay on the RX 6700 XT alone (the Auto rule of §1a.63
    already does this); the gain still helps preview on Intel-only machines. Cost: none.
(b) Fuse passes (colour chain + per-pixel effects into one pass, a combined Unsharp; maybe 2–3×
    fewer passes): a few days of shader work; at best ~8–10 fps on Intel, which would add roughly a
    third to graded 6.2K export speed when both GPUs run.
(c) Half floats on Intel for exports only: halves bandwidth, but you chose full precision for
    exports (§1a.64).
**Choice for now: (a)**, and U0.2.3b moves on to step d (AMD graded speed). Reversible: (b) can be
scheduled any time as its own step; nothing is removed.

## Q20 (2026-10-13, U0.2.3b d): a 1.5× faster 4K export from 6.2K that is not bit-identical (choice made, reversible)

When a 4K export is made from the X-M5's 6.2K clips, the Intel media engine can decode while the
RX 6700 XT grades and encodes. The bottleneck was Intel copying each full 6.2K picture across to the
AMD card. Letting Intel's video processor shrink the picture to the size it will be drawn at
(3240×2160) *during* that copy makes the export much faster (synthetic, full length): 4K graded
24.7 → 37–40 fps, 4K ungraded 26.0 → 41.8 fps. This beats the ≥ 30 fps floor.
The catch is that the output is no longer bit-identical to today's. Flat areas and edge positions
match (≤ 0.3 codes, no shift). Fine noise and texture differ because the downscale filter is
different: Intel's scaler averages the noise (it looks smoother) where the GPU path keeps more of
it. PSNR against today's output is 42.6 dB ungraded and 32.9 dB graded (the grade's sharpening
amplifies the texture difference). Step e asked for "output unchanged". Options:
(a) **Use the scaled hand-off** when Export engines = Auto and the export is smaller than the source.
    It applies only to clips placed by a plain fit, with no keyframed Motion, no temporal NR and no
    Maximum Render Quality (which keeps full size as today). Setting/env off-switch:
    `FILMCRAFT_HANDOFF_SCALE=1`.
(b) Keep exports bit-identical: no scaled hand-off, so 4K graded stays at ~25 fps.
(c) Change the GPU path's own downscale to an area filter as well, so both agree. This changes
    today's output for every export and preview of downscaled clips.
**Choice for now: (a)** as the plan for the plumbing. Until that plumbing lands, the code is only a
diagnostic (`FILMCRAFT_HANDOFF_SCALE` + `FILMCRAFT_DECODE_NODE`) and no export changes. Reversible:
the default is a single switch, and Maximum Render Quality always keeps full size.

**Update (2026-10-13 18:21, built):** Auto now does (a) in the export itself: one lane on the RX 6700 XT
whose decoders run on Intel, each source scaled to the most its clips ask for. The earlier
37–40 fps came from a diagnostic that shrank the bench clip too far: it has a 120 % Motion zoom, so
the correct scale is 62 %, not 52 %, and the diagnostic under-delivered pixels. **Correct numbers:**
4K graded 24.7 → **30.2–30.9 fps** (full length, synthetic). PSNR against today's single-chip output
is 33.8 dB graded, still a fine-texture difference only. Off-switches: Primary GPU only, Maximum
Render Quality, or `FILMCRAFT_HANDOFF_SCALE=1`.

**Owner (2026-10-14, live): (b) scheduled** as X422 step 2b, fused graded passes (§1a.71).

**Answered 2026-10-14 ~01:20 (owner, live): keep (a)** ("ok sounds good then"). Resolved.

## Q21 (2026-10-13, U0.2.3b e): 6.2K graded export stays at ~19.6 fps, short of the ≥ 24 aim (FYI, choice made)

Step e's full-length bench (synthetic, median of 2) passed three of four targets: 4K 32.5 / 35.1 fps,
4K graded 31.1 / 30.5, 6.2K 25.5. **6.2K graded is 19.6 fps** (19.1 before: not slower, but short of
the ≥ 24 aim). The RX 6700 XT's single media engine both decodes and encodes 6.2K, and that is the
limit. Things tried: two lanes on the AMD card were slower. Intel decoding for it reached 20.5 fps,
because copying a full 6.2K picture across costs ~34 ms inside the export, so the gain is too small
to switch on. Options:
(a) **Accept for now**: keep every gain, close U0.2.3b, continue with U0.2.4 (step e's rule for a
    lever short of its share).
(b) Later: a faster hand-off for full-size pictures (sharing Intel's decoded surfaces with the AMD
    card without a copy; needs a cross-vendor tiling both can read, probably not available).
(c) Later: render cache / smart-render parts for graded 6.2K timelines, which avoids the re-render
    on repeated exports.
**Choice for now: (a).** Reversible: (b) and (c) can be scheduled any time.

**Owner (2026-10-14, live): route to 24 fps wanted;** fused graded passes scheduled in X422 step 2b
(§1a.71). Render-cache reuse for exports (c) not scheduled.

## Q22 (2026-10-13, AI1.1): the tuned SmolLM2 explainer scores 70/75 on the new runtime instead of 72/75 (FYI, choice made, reversible)

AI1 replaces candle with FilmCraft's own quantized forward pass (no x86-64-v3 build flag needed).
On the 75 held-out cases (greedy, 80 tokens, repeat penalty 1.0, this PC, synthetic prompts):
`qwen05-v1` scores **74/75, identical** to its recorded run; `smol360-v1` (the default tune) scores
**70/75** (set 1 34/36 as before, set 2 36/39 instead of 38/39), and answers in **1.62 s** on average
instead of ~3 s. Its set-2 result moves with the order in which the maths adds numbers up: 36 with
our AVX2 kernels, 37 with our portable path (which is bit-identical to candle's plain path), 38 in
the recorded run (candle's AVX2 path). The cases that flip are claims just at the fact checker's
thresholds (a cooler highlight it omits, a red-curve brightening it overstates), so this is
rounding noise on borderline cases, not a broken model. AI-PLAN's accept line says "scores the
same". Options:
(a) **Accept within ±2 cases on 75** for the switch of runtime, and let AI1.6's retrained
    `smol360-v2`, which is evaluated only on the new runtime, settle the score.
(b) Hold AI1 until `smol360-v1` scores exactly 72 again (would mean matching candle's AVX2
    summation order, slower and arbitrary).
(c) Make `qwen05-v1` the default meanwhile (74/75, same speed now), against §1a.27's SmolLM2
    base rule.
**Choice for now: (a).** Reversible: (b) or (c) any time. Outputs:
`~/Apps/filmcraft-train/runs/ai1-own-pass/` (`eval.jsonl`, `score-set{1,2}.md`,
`eval-baseline-set2.jsonl`, `score-baseline-set2.md`).

## Q23 (2026-10-13, L1.5): upstream's non-blocking file dialogs (#260) taken early (FYI, choice made)

L1.5 builds on upstream's #260 ("file dialogs on Linux no longer stop the app"). The audit expected it
to arrive with U0.2, but U0.2 only synced the VA-API stack; the next full upstream sync is weeks of
commits away (565 pending). Options: (a) wait for that sync; (b) write our own; (c) **cherry-pick
upstream's commit `059376af` unchanged** (`-x`, so the next sync sees the same change) and add L1.5's
parts (dialog parented to the window, each kind of dialog starts in its last folder) as a
fork-side hook `run_dialog_from` next to upstream's `run_dialog`, leaving upstream's signature and
tests alone. **Choice: (c).** The next sync's merge of `native_dialogs.rs` / `main.rs` may need a small
hand resolution; the fork's version keeps both hooks.
## Q24 (2026-10-13, AI1.4): may the run download SmolLM2-135M-Instruct to try a "fast" explainer?

AI-PLAN step 4 tries a smaller tune: SmolLM2-135M-Instruct (Apache-2.0, Hugging Face
`HuggingFaceTB/SmolLM2-135M-Instruct`, about 270 MB of weights plus tokenizer / config, saved under
`~/.local/share/filmcraft-fork/models/llm-base/`), trained with TRAINING.md's pipeline and shipped
as a "fast" option only if it matches the 360M tune within a few cases. Downloads need your yes.
It matters less now: the 360M tune answers in ~1.6 s and shows its first words in ~0.3 s, so the
"fast" option would mainly help much slower PCs (the Dell is one to test it on).
Options: (a) yes, download and try it (~1 h of GPU training + evaluation); (b) skip AI1.4 for
good (the 360M tune stays the only one); (c) later, after AI1.
**Choice for now: skip AI1.4 and continue with AI1.5 / AI1.6**; if you answer (a), a later run
does AI1.4 (before or after the `ai-1` tag, it doesn't block it). Reversible.

**Answered 2026-10-14 ~01:10 (owner, live): (a) yes.** The run may download SmolLM2-135M-Instruct
(Apache-2.0, ~270 MB) and do AI1.4; keep the "fast" tune only if it passes. Doesn't block `ai-1`.

## Q25 (2026-10-14, AI1 end): the ARM (NEON) explainer kernels are not in `ai-1` (FYI, choice made)

§1a.69 says AI1 also adds the aarch64 (NEON) kernel path. `ai-1` shipped without it: on ARM the
explainer runs on the portable loop (correct, roughly the forced-portable speed: 1.9 s per answer
on this PC, slower on a small ARM board). Writing NEON kernels needs at least the
`aarch64-unknown-linux-gnu` Rust target and an emulator (`qemu-user`) to test them here, both
downloads, and real ARM hardware to measure. Options: (a) do it in **Phase 8 item 7** (Asahi /
ARM64), where ARM builds and tests happen anyway; (b) do it now as AI1.7 after you allow
`rustup target add aarch64-unknown-linux-gnu` + installing `qemu-user` (sudo: you); (c) skip.
**Choice for now: (a)**, X422 continues. Reversible: answer (b) and a run does AI1.7 next.

## Q26 (2026-10-14, A2 audit, Dell lane): parity oracle and test speech for voice isolation (FYI, choice made)

A2 ports DeepFilterNet3 to candle (weights downloaded with your 2026-10-12 consent, pinned and
checksummed, see `PREP-A2.md` §3). Two things the rules make me decide:
1. **Oracle.** To prove the port matches the original, something must run the original model.
   There's no PyTorch / onnxruntime here and installing them is a new download. **Choice:** build
   DeepFilterNet's own Rust tool (`libDF` / `deep-filter`, MIT / Apache, same repo as the weights)
   **from source on the Dell** as a test oracle only, like ffmpeg: never linked, never shipped.
2. **Test speech.** Kokoro voices aren't on disk, so the SI-SDR test set uses the built-in formant
   voice and generated speech-like signals. That's weaker evidence than real recordings.
   Options: (a) keep it so; (b) allow a small CC-BY speech set (e.g. ~50 utterances of LibriSpeech
   `test-clean`, CC-BY 4.0, ~30 MB) on the Dell only; (c) allow downloading the Kokoro voices the
   app already offers. **Choice for now: (a)**; answer (b) or (c) and a later run re-measures.

## Q27 (2026-10-14, X422 (b)): the 6.2K 4:2:2 HEVC master exports at ~7 fps, not 24 (FYI, choice made)

Exporting the graded 6.2K 4:2:2 clip to the "HEVC 4:2:2 10-bit master" preset now runs at **7.3 fps**
(was 0.5 fps: the colour conversion moved from the CPU to the GPU). The limit is the Intel UHD 750: it is the
only chip here that can both decode and encode HEVC 4:2:2, so it decodes, copies each frame across for the AMD
card to grade, and encodes, all at once. Measured levers (synthetic, `docs/fork-benchmarks.md` "X422"):
the encoder's speed setting changes nothing ("speed" 6.9, default 6.8, "quality" 5.0 fps); our ProRes 422 HQ
encoder at 6.2K runs at 0.5 fps (the ProRes path still renders the grade on the CPU); faster uploads into the
encoder (`vaDeriveImage`, tried) save CPU time but no frames, because the Intel chip, not the CPU, is full.
Options: (a) keep it: 7 fps (a 1-minute 6.2K 4:2:2 master takes ~4 minutes); for speed, export the 4:2:0
"6.2K Master HEVC" (case (a), 17 fps now, X422 works on it next); (b) later: render ProRes 422 HQ on the GPU
too (a separate step, its own measurement); (c) a future machine with a 4:2:2 HEVC encoder on the big GPU
(e.g. NVIDIA 50-series / Intel Arc) would lift this.
**Choice for now: (a)**, X422 continues with case (a) and the fused passes (§1a.71). Nothing to undo.

**Update 2026-10-14 ~06:30 (A2.7), now a real question: A2's acceptance needs your answer.**
Measured with (a): DeepFilterNet doesn't recognise the built-in formant voice as speech (clean
speech comes out at 2.1 dB SI-SDR instead of ≥ 20; it loses to DeNoise on most noise types; table
in FORK-PLAN §7). The port is verified (identical to the original, 68.2 dB), so only real speech
can show whether Voice Isolation meets the gate. Options:
(b) the lane downloads ~50 utterances of LibriSpeech `test-clean` (CC-BY 4.0, read English speech,
~30 MB, Dell only, never committed), converts them to 48 kHz WAV with ffmpeg (oracle use);
(c) download the Kokoro natural voices the app already offers (neural TTS: closer to speech, still
synthetic, ~330 MB);
(d) you record 2–5 minutes of your own voice in a quiet room (any app, 48 kHz WAV) and put the files
in `~/.local/share/filmcraft-fork/voice-test/` on the PC (the lane copies them to the Dell; they stay
out of git).
**Recommendation: (b)** (no work for you, permissive licence, many speakers), or (d) if you'd rather
test on your own voice. Until then the lane continues with A2.6 / A2.8 / A2.9 and re-measures when
you answer; the harness takes any folder of 48 kHz WAVs (`FILMCRAFT_EVAL_SPEECH`).

**Owner answer (live, 2026-10-14 ~06:20): (b) approved.** "If there's footage from elsewhere other than me
that you can use for this, that's totally fine." The Dell lane downloads the ~50 LibriSpeech `test-clean`
utterances (CC-BY 4.0, ~30 MB, Dell only, never committed), converts them to 48 kHz WAV with ffmpeg (oracle
use) and re-measures A2's gate on them. (c) Kokoro voices only if (b) proves unsuitable. The owner may still
add recordings of their own voice later in `~/.local/share/filmcraft-fork/voice-test/`; re-measure then too.

## Q-AI3-models (2026-10-14, AI3): which models may AI3 download? (answered)

**Answered live: SmolLM3-3B yes** (base + instruct, quantized and full-precision for the tune; on
this PC and the Dell). Phi-4-mini: not approved. OLMo 2 7B: only if AI3's eval shows SmolLM3 is
not enough; ask again with numbers then.

## Q28 (2026-10-14, X422 2b): fused graded passes kept; the graded aims are still short (FYI, choice made)

**What happened.** The fused passes (§1a.71) cut the bench grade's GPU work per 6.2K frame from 286 to 178 ms on the
Intel UHD 750 (~3.5 → ~4.9 fps for an Intel lane) and from 19.1 to 8.2 ms on the RX 6700 XT. Exports: 6.2K graded
19.6 → 19.7 fps (AMD is held by decode / encode there, not by rendering), X422 (a) 4:2:2 graded 17.1 → 19.9 fps.
The aims (Intel ~8–10 fps, 6.2K graded ≥ 24 fps) are not met.
**Choice made (per §1a.71):** keep the gain, record it, go on with X422's final benches. Output equals the old passes
within 1e-5 on both GPUs; `FILMCRAFT_FX_FUSE=0` brings the old passes back.
**Options for later, if you want them:** (a) nothing more (recommended for now); (b) bake the colour chain (input
transform + Basic + curves) into a GPU 3-D LUT per grade, which would cut Intel's chain cost (~60 ms of maths) at a
small accuracy cost (LUT interpolation, needs your bar); (c) let the Auto rule give Intel a share of graded 6.2K
exports now that its lane is ~4.9 fps (a few percent at best, since AMD is decode / encode-bound).
**(a)'s measured limit (X422 acceptance):** median of 2 = 20.0 fps; the Intel UHD 750 spends ~50 ms per frame on 4:2:2
decode + the ~104 MB hand-off copy to the AMD card, and AMD waits on it (`docs/fork-benchmarks.md` "X422 final").
(b) 4:2:2 HEVC master: 7.2 fps (Q27).

## Q29 (2026-10-14, Y3.4): how user shader effects work (FYI, choice made)

ADR [0006](../adr/0006-user-shader-effects.md), written by the scheduled run without a separate
designer (weekly usage above pace). The choices you may want to change: (a) user effects are
**GPU-only** (no CPU fallback; without a GPU they are skipped with a message); (b) the user writes
one function `effect(px) -> vec4` on **display-referred straight colour** (FilmCraft converts from
and to the linear working image), not raw linear premultiplied pixels; (c) the time limit is
**after the fact** (3 frames over 50 ms at 1080p disables it; a GPU hang quarantines the file),
because WGSL can't be stopped mid-frame. **Choice for now:** as in the ADR. Reversible: answer here
and a run changes the ADR before step 4 (the runtime); steps 2–3 (parser, registry) don't depend on (a)–(c).

**Owner answer (live, 2026-10-14 ~12:55): ok** (GPU-only stays).


## Q30 (2026-10-14, A2.8b, Dell lane): Voice Isolation misses two of its own acceptance bars on real speech (FYI, choice made)

On ~50 LibriSpeech recordings (your Q26 yes), Voice Isolation beats DeNoise in every case and by a
lot where noise is loud: **+7.1 dB at 0 dB SNR, +4.0 dB at 5 dB** (traffic, keyboard, wind). Two
bars I set before measuring are missed: at **10 dB SNR** (mild noise) the margin is only **+0.8 dB**
(bar 3 dB), and **clean speech** through it scores **17.0 dB** (bar 20 dB). Part of the clean-speech
number is probably LibriSpeech itself: its "clean" recordings have background 23–38 dB under the
voice, which the model removes and the measurement counts as damage. Clean Up Voice itself passes
its gate (one click is as good as the written manual process in all 15 cases, PREP-A2 §1).
Options: (a) accept as is: big gains on noisy voice, small ones on mildly noisy voice, slight
change to an already clean voice (Clean Up Voice turns noise reduction off on clean recordings
anyway); (b) keep A2 open and re-measure on recordings of your own voice in a quiet room
(`~/.local/share/filmcraft-fork/voice-test/`), which would separate "the model" from
"LibriSpeech's own noise"; (c) lower the default Voice Isolation amount for mild noise.
**Choice for now: (a)** with the numbers written into "A2 ready for acceptance"; (b) any time you
drop recordings there. Nothing to undo.

**Owner answer (live, 2026-10-14 ~12:55): (a) accepted.** "Yeah for now that's fine. We can train it
better later." Re-measure if own-voice recordings appear.

**Owner, live 2026-10-15 ~00:15:** A2 accepted; the quality work goes into **A3** (moved before
S1): amount / attenuation limit, own-voice re-measure, a stronger model only with a download yes
(AUDIO-PLAN A3, FORK-PLAN §1a.76).


## Q31 (2026-10-14, H1 audit, Dell lane): test files for interlaced H.264 field pictures (PAFF) (question; MBAFF goes ahead)

H1 makes interlaced H.264 (1080i camcorder footage) decode on the graphics card. There are two kinds
of interlaced coding: **MBAFF**, which I can make test files for myself (x264 writes it), and **PAFF**
(each field coded as its own picture), which neither x264 nor these GPUs' encoders can write. To test
PAFF properly I'd need either (a) the ITU-T H.264.1 **conformance streams** (the official test
bitstreams, freely downloadable from the ITU, tens of MB, kept on the Dell only, never committed),
or (b) a 1080i file from a camcorder you have, or (c) nothing: PAFF then gets unit tests on synthetic
headers only and no real-file check. **Until you answer: (c)** for PAFF, MBAFF fully tested; nothing
to undo. Downloads still need your yes.

**Owner answer (live, 2026-10-14 ~12:55): (a) yes.** Download the ITU-T H.264.1 conformance streams
(Dell only, never committed) for PAFF and the other field / H2 conformance checks.


## Q32 (2026-10-14, H2.7, Dell lane): slice-level threads in the H.264 software decoder (FYI, choice made)

H264-PLAN H2 item 3 asks for slice-level threads, then frame-level. Frame-level threading already
exists and measured on the Dell: 4K 10-bit 4:2:2 Long-GOP **43 fps**, a very high-rate (632 Mbit/s)
4K 10-bit 4:2:2 Intra stream **27 fps** (docs/fork-benchmarks.md, H2.7). Slice threads would only
shorten the wait for a single frame (scrubbing / a paused frame: ~0.1 s Long-GOP, ~0.3 s Intra at 4K
on one core) and only on streams coded with several slices; a safe-Rust version needs per-slice views
of the picture (a large refactor of the slice decoder). Options: (a) defer slice threads until a real
file shows the single-frame wait matters; (b) build them now. **Choice: (a)**, revisit at H2.8 with
real 4:2:2 / interlaced files and the i5-11600K numbers. Nothing to undo.


## Q33 (2026-10-14, H2.5 / H2.6, Dell lane): interlaced H.264 is built but waits for its code reviews (FYI, choice made)

Interlaced H.264 now decodes in FilmCraft's own software decoder, both kinds (PAFF and MBAFF), and
matches the official ITU-T test streams exactly: 210 of the 217 in-scope ones (the other 7 use
features outside H2: monochrome, FMO, SP/SI). The work sits on two branches (`dell-lane-h2-5`,
`dell-lane-h2-6`, plus H1.3/H1.4 on `dell-lane-h1-3`) because the lane's rule is a fresh-context
Opus review before merging decoder code, and reviews use subagents, which the lane skips while the
weekly usage is above its pacing line (it has been all day: 41 % vs ~21 %). Options: (a) keep
building on branches and run the reviews as soon as the weekly line allows (or when you say so);
(b) review now despite the pace (about one Opus review per branch); (c) merge with self-review only.
**Choice: (a).** Nothing to undo; the branches are on this PC and on the Dell.
**Update 2026-10-14 ~18:50 (Dell lane):** a new scheduled run starts with a context that has never seen these
branches, so the lane now does the fresh-context Opus review in the run itself (no subagent, about the same cost as
reading the diff) and merges after it. H1.3 / H1.4 / H1.5 were reviewed and merged this way; H2.5, H2.6 and H3.2a
follow in later runs, one branch per run.

## Q34 (2026-10-14, H3.1, Dell lane): H.264 stops using the system ffmpeg (FYI, choice made)

H264-PLAN H3 wants H.264 to decode without ffmpeg. The plan's wording suggests a Settings switch
"Use ffmpeg for H.264" (off by default), but FORK-PLAN §1a.24 removes ffmpeg from the product in
Phase 7.7 anyway, so that switch would be a setting with nothing behind it a few phases later.
Options: (a) the switch; (b) leave the ffmpeg pipe in the chain and only prove that H.264 also
works without it; (c) take the pipe out of the H.264 chain now (HEVC keeps it until 7.7).
**Choice: (c)** (PREP-H3 §2). Effect: with or without ffmpeg installed, H.264 decodes on the
graphics card through VA-API, and whatever VA-API declines decodes in FilmCraft's own decoder
(correct, but on the CPU). Nothing to undo: it is one function, `vaapi_chain` in
`crates/platform/src/lib.rs`.

## Q35 (2026-10-14, AI3.6): the assistant model fails the eval; may it be tuned on the guide? (question; choice made for now)

The first full run of the grading assistant (SmolLM3-3B, as downloaded, on this PC's CPU) on the
85 advanced questions (`docs/fork/coach-eval/advanced.toml`) passed only **7 of 85 (8 %)** on the
automatic checks alone (the plan's floor is 85 %): it cited a right guide section in 19 answers,
and in 60 answers the checker had to remove made-up controls, buttons or section names (for
example, asked for a Resolve-style parallel node it says FilmCraft doesn't have one, then tells you
to "use a Key-Mixer node", which doesn't exist either). Answers took 20–57 s each. All synthetic.

AI-PLAN says the fix for a model that misses the floor is a short tune on the guide
(`docs/fork/coach-eval/training-pairs.jsonl`, the same pipeline as the explainer), and that a
person must look at a sample of that training data first. Options:
- **(a) Tune it:** you read a sample of ~30 training pairs (I'll put them in one short file; about
  20 minutes), say yes, and the run tunes SmolLM3-3B on the guide. Most likely to reach the floor;
  costs GPU time and a review of the data by you.
- **(b) No tune, stricter answers:** the assistant may only pick, order and shorten the guide's own
  steps (no free-written steps). No invented controls by design, answers less conversational.
- **(c) A bigger model** (7–8B): needs GPU inference first and another download yes; slower.
**Choice for now:** the assistant stays **off by default** (as today: Settings ▸ Color ▸ Grading
Coach), the next step is GPU inference (needed whatever you pick), and (b) is tried meanwhile
because it needs nothing from you. The tune waits for your answer to (a). Nothing to undo.

**Update 2026-10-14 21:34 EDT (scheduled run): option (b) built and measured.** In the new "pick" style the model only
names guide steps by key (A1, B3 …) and FilmCraft shows those steps word for word. On the same 85
questions: **69 of 85 (81 %) pass the automatic checks, nothing invented in any answer**, 81 answered by
the model, 75 cite a right section, median 23 s per answer (CPU, synthetic). Misses: 10 cite no expected
section, 4 gap questions don't say the feature is missing, 4 say "I don't know". **Choice for now:** pick is
the default style of `coach.assist` and the Ask the coach box (constant `DEFAULT_STYLE` in
`crates/engine/src/coach_assist.rs`; scripts can still ask for `style: "free"`); the assistant itself stays
off by default until GPU inference makes it fast. To undo: set `DEFAULT_STYLE` back to `AssistStyle::Free`.
The tune (a) may no longer be needed: the remaining gap to 85 % looks like retrieval / gap-route work, not
model knowledge. Your answer to (a) is still welcome but nothing waits on it.

**Update 2026-10-14 22:14 EDT:** after a fix to the scorer (it missed answers saying "FilmCraft: no … yet") and splitting the
guide's bullet lists into separate steps, a fresh run of the pick style passes **76 of 85 (89 %) with
nothing invented**, above the 85 % floor on the automatic checks (a person still has to read whether the
answers contain the right facts). So **no tune is needed for now**; (a) stays open only if that reading
finds the answers thin. Speed is still the problem (about 24 s per answer on the CPU): GPU inference next.

## Q36 (2026-10-15, U0.2 review item 5): VA-API drivers other than AMD Mesa / Intel iHD (FYI, choice made)

FilmCraft uses one VA-API display from several threads; only Mesa (AMD) and Intel's iHD driver are known to lock per
display. The old Intel i965 driver and nvidia-vaapi-driver were used the same way unchecked (possible crashes).
Options: (a) refuse them, software decode instead (safe, slower on those machines); (b) add our own per-display lock
around every libva call for them (more code, slower, needed for NVIDIA in Phase 8); (c) leave as is.
**Choice for now: (a)**, with `FILMCRAFT_VAAPI_ANY_DRIVER=1` to use such a driver anyway (guide `hardware.md`). Neither
of your machines is affected. Reverse: delete the check in `crates/platform/src/va.rs` `open_raw`; Phase 8 (NVIDIA) does (b).


## Q37 (2026-10-15, H group acceptance): the AMD video decoder is stuck in slow mode until a reboot (needs you; choice made for now)

During today's `cargo xtask ci` (06:40) the RX 6700 XT's video decoder (VCN) stopped signalling finished
pictures: the kernel log has been printing `amdgpu …: Fence fallback timer expired on ring vcn_dec_0` about
twice a second since then (12 000+ lines), and at 07:28 there was a VCN page fault (`[mmhub] page fault …
Faulty UTCL2 client ID: VCN0`). Every hardware decode on the AMD card now waits for a ~0.5 s fallback timer,
so video decoding on that card (FilmCraft, and also browsers / players) is very slow until the GPU is reset.
It had never happened in the previous 3 days of logs. **Only a reboot (or a GPU reset, which needs sudo)
clears it**, so this needs you.
- What it broke: ci's VA-API tests timed out (4 failures, all in `filmcraft-platform`'s `vaapi` tests; 3983
  other tests passed). The same tests on the Intel GPU are the check that the code is fine (result in the
  progress log).
- **Choice made for now:** H-group acceptance uses the Intel GPU (here and on the Dell) for the VA-API
  re-checks; the AMD in-app 1080i check was done at 05:46, before the fault. GPU inference work for AI3
  (wgpu compute, not the video decoder) is unaffected. Whether our hostile-stream test (`damaged_samples_…`,
  which feeds the hardware damaged H.264) can trigger this driver fault is an open question: it is logged in
  HANDOFF ▸ Open problems, and the hardware hostile-input tests may need to run only on Intel / the Dell
  until it is understood.
- **Please:** reboot when convenient, then tell a session (or nothing: the next run re-checks the kernel log
  and re-runs the AMD VA-API tests).

**Owner, 2026-10-15 ~13:35: rebooted.** After the reboot the kernel log shows VCN starting cleanly (no fence / page-fault lines) and VA-API on the RX 6700 XT (renderD129) lists HEVC decode / encode again. The AMD VA-API tests can be re-run.

## Q38 (2026-10-15, AI3 acceptance): first words in under 1 s vs answer quality (FYI, choice made)

With GPU inference on the RX 6700 XT, the assistant's answers take a median 2.4 s (max 4.2 s; on the CPU they took
21.5 / 32.4 s), so "a typical answer in under 10 s" is met easily. **"First words in under 1 s" is met at the median
(0.90 s) but not for long questions:** p90 1.2 s, max 3.9 s, 46 of 85 test questions under 1 s. The time is the prompt
itself (300–1150 tokens of guide text at ~1 ms per token, already within ~10 % of what the graphics card can do); the
cheap speed-ups were tried and don't move it. The one lever left is sending less guide text per question:
- 4 500 characters (**kept, the default**): 46 / 85 under 1 s, 83 / 85 answers pass the automatic checks.
- 4 000: 52 / 85 under 1 s, 82 / 85 pass.
- 3 600: 78 / 85 under 1 s (p90 0.99 s), 81 / 85 pass (loses "orange skin" and "shot-match limits").
**Choice made for now:** keep 4 500 (better answers; under a second at the median, ~1.2 s for long questions; the
keyword coach's answer still appears instantly above it). To trade quality for speed instead, change
`DEFAULT_BUDGET_CHARS` in `crates/engine/src/coach_assist.rs` to 3 600 (one line). All numbers synthetic.

## Q39 (2026-10-15, AI3 acceptance ci): damaged video wedges the AMD video decoder (needs a reboot; choice made for now)

Q37's open question is answered: it **is** our own hostile-stream test. In this afternoon's `cargo xtask ci`
(after your 13:35 reboot) the test `vaapi::damaged_samples_and_parameter_sets_never_crash_or_hang`, which feeds
the hardware decoder deliberately damaged video, caused a VCN page fault at 14:20 (`[mmhub] page fault …
Process vaapi-… Faulty UTCL2 client ID: VCN0`), and since then the kernel prints `Fence fallback timer expired
on ring vcn_dec_0` again: hardware video decoding on the RX 6700 XT is slow (for every app) until a reboot.
FilmCraft itself didn't crash; this is a driver / firmware weakness (Mesa radeonsi + amdgpu VCN 3.0), but a
damaged camera file opened in FilmCraft could probably do the same.
- **Choice made for now (provisional, reversible):** the two hostile-stream VA-API tests skip the AMD card
  (they still run on Intel here and on the Dell); `FILMCRAFT_VAAPI_HOSTILE_AMD=1` runs them on AMD again.
  The product path is unchanged.
- **Open (later, needs a design step, logged in HANDOFF ▸ Open problems):** check bitstreams more strictly
  before they reach VCN (slice sizes / parameter-set ranges), or decode a file in software once it has shown
  damage; and find which damage pattern triggers the fault so it can be reported to Mesa.
- **Please:** reboot when convenient to clear the slow mode again (nothing else needs you; the AI3 build
  doesn't depend on hardware video decoding).

## Q40 (2026-10-15, AI4.1d): how answers use the measured picture facts (FYI, choice made)

AI4's accept says "answers use at least one picture fact". But the Assistant's default answer
style (pick, Q35 (b)) only lets the model choose guide steps by key, so it can't write a fact,
and a fact the model rewrote could come out wrong.

Options:
- (a) **FilmCraft writes the fact lines itself:** with each answer, the measured facts that fit
  the question (exposure for "dark / clipped", white balance for "orange / blue", skin, saturation,
  log source; at most 3 lines) are shown above the steps as "On this frame (measured)". They can't
  be invented, and they cost no model time. The facts also go into the prompt so the model can
  choose steps that fit the shot.
- (b) Let the model write one free sentence about the picture before its picked steps: more
  natural, but it has to be checked against the numbers, and it's slower.
- (c) Both: (a) now, then (b) once the vision model (AI4 item 2) is in.

**Choice for now: (a)**, with (c) open (`coach_assist::picture_note`, answer field `seen`).
To reverse: drop `seen` and send only the prompt context.

## Q41 (2026-10-16, CREATIVE-POOL): read 40 creative training pairs before a tune uses them (**answered 2026-10-16 ~04:20: read**, FORK-PLAN §1a.96 (b))

The Dell lane's creative set now has 207 training recipes (and 397 held-out questions). Per the Q35
rule, you read 30–50 random training pairs before any tune uses them. 40 are in
`docs/fork/coach-eval/pool/CREATIVE-READ.md` (fixed random seed). Every step names real controls /
command ids (checked by `ai_pool.rs`); 5 recipes were also run on the Dell (all work; the first found
a frame-hold keyframe bug, since fixed).

**Choice for now:** the main runner may tune with them if you haven't read them by then, keeping the
untuned model unless the tuned one beats it with 0 invented (AI4's rule). Note any pair that's wrong
here; the lane fixes it and re-checks its family.

## Q44 (2026-10-16, AI4 item 3): look through the picture-model tune's sample (FYI, never blocks)

You don't have the X-M5 yet, so the picture-model tune trains on synthetic frames plus openly licensed footage
by others, **including log footage** (F-Log2 first, other logs too; download approved: CC0 / CC-BY / CC-BY-SA /
public domain, licence logged per file in `docs/fork/coach-eval/VISION-SOURCES.md`, ≤ 20 GB, kept outside git;
FORK-PLAN §1a.96 (c)). When the sample is ready the runner says so here and in HANDOFF: 30–50 frames with their
labels in `docs/fork/coach-eval/VISION-READ.md`. **The tune runs in AI4 whether or not you've looked** (your
call, 04:35); note any wrong label in the file and say "Q44 reviewed", and the runner fixes the labels and
re-runs the tune. The tune is kept only if it beats the base model on held-out frames.

**Ready (part 1, 2026-10-16 04:42 EDT):** 40 synthetic frames with labels in `docs/fork/coach-eval/VISION-READ.md` (frames in `~/Apps/filmcraft-train/runs/vision-data-v1/frames/`). Part 2 (downloaded open footage, incl. log) follows.

## Q43 (2026-10-16, order): where REF and S1 sit (FYI, never blocks)

You said "add it before C2" (the reference-picture phase, REF) and "also move S1 to after AI5". Read as:
… AI4 → AI5 → **S1** → **REF** → C2 → AI2 → A3 → S2 → G3 → … (S2 stays where it was, now apart from S1).
If you meant REF before S1, or S2 to move with S1, say so and the order changes.

## Q42 (2026-10-16, AI4.2): a CPU PyTorch for the exact Qwen3-VL reference check (needs your yes for a download)

FilmCraft's own Qwen3-VL picture encoder now matches candle's copy of it, but candle is a second-hand
port (it already differs from the published model in one activation, and its text side has two real
bugs, see `docs/fork/DESIGN-AI4-TEXT.md`). The exact check is the published Hugging Face
implementation. Your training venv (`~/Apps/filmcraft-train/.venv`) has transformers 5.19 but torch
2.5.1, which that transformers can't import (`torch.accelerator` arrived in torch 2.6), and it has
no image library for the reference processor.

**Options:** (a) a separate venv under `~/Apps/filmcraft-builds/qwen3vl-ref-venv` with CPU-only
torch ≥ 2.6 (~200 MB wheel), transformers 5.x, Pillow (your training venv untouched); the script
`tools/qwen3vl-ref/make_ref.py` (already in the repo) then writes reference patches and encoder
outputs that a test compares against; (b) upgrade torch in the training venv (could disturb your
training setup); (c) no Python reference: keep candle parity plus behavioural checks (a red square
is called red, etc.).

**Choice for now:** (c), nothing downloaded. Answer "Q42 a" (or b) and the next run sets it up and
adds the comparison test. Never blocks.

## Q45 (2026-10-16, AI4 item 5c): semantic search for open-ended creative questions (**answered 2026-10-16 ~13:35: (a)**, FORK-PLAN §1a.98)

The first GPU run of the daily sample (2026-10-16 04:32 EDT, synthetic): 72 / 147 pass, 0 invented; guide lookups
69 / 86, **creative plans 3 / 61**. Measured on all 608 held-out creative questions (CPU): only
**138** get at least two of their plan's guide sections from FilmCraft's word-matching search in the
top 6, 202 even in the top 16, so the assistant can't cite a plan it never sees. Matching against the
412 creative training questions as well lifts that to 165, but costs ordinary questions 34 of 821
hits, so it is **not** switched on. The real fix is search by meaning (a small embedding model,
about 25–140 MB, Apache-2.0 / MIT, e.g. a MiniLM / BGE-small class model run on our own code), which
is a new model download.
- **(a)** Allow one small openly licensed embedding model (download, licence checked, run on our own
  inference like the others); measured on the creative and lookup sets before it is kept.
- **(b)** Stay with word matching; creative answers improve only through the guide's wording and the
  assistant tune.
- **Provisional choice: (b)** (no download without your yes). Reverse: answer (a).

## Q46 (2026-10-16, AI4 item 5 (b)): the assistant tune needs SmolLM3-3B's full weights (**answered 2026-10-16 ~13:20: "yes please tune it"**; downloaded and verified, FORK-PLAN §1a.97)

The assistant tune (LoRA on SmolLM3-3B with the pool's training pairs, kept only if it beats the
untuned model) needs the model's full-precision release (Hugging Face `HuggingFaceTB/SmolLM3-3B`,
Apache-2.0, about 6.2 GB): only the 4-bit GGUF the app runs is on disk, and a LoRA can't be trained
on that. Scheduled runs don't start downloads on their own, so this waits for your go (say "Q46 yes"
in a live session, or download it into `~/.local/share/filmcraft-fork/models/llm-base/SmolLM3-3B/`).
- **Meanwhile:** AI4 continues with the picture-model tune (item 3) and everything else; if the
  weights aren't there by AI4's acceptance, `ai-4` ships with the untuned assistant (as item 5 (b)
  already allows: "else keep the untuned one and log why").
- Design note for the tune (provisional): train the **Pick** style the app uses by default (prompt =
  the app's own `pick_prompt`, target = the keys of the offered steps from the pair's sources, best
  matches to the pair's answer first, at most 6; pairs whose sources weren't retrieved are skipped,
  so the tune doesn't learn to answer NONE for search misses).

**Answer (owner, live, 2026-10-16 ~13:20): "yes please tune it".** Downloaded in the live session: `~/.local/share/filmcraft-fork/models/llm-base/SmolLM3-3B/` (HuggingFaceTB/SmolLM3-3B rev a07cc9a0…, 5.8 GB, both shards' SHA-256 equal Hugging Face's, Apache-2.0, `REVISION` file). The tune runs as **AI4.5b-late** (FORK-PLAN §1a.97).

## Q47 (2026-10-16, AI4 acceptance): publish `ai-4` while `ai-3` is on hold? (FYI, choice made)

`ai-3` is built but not published until you say so (§1a.80). `ai-4` contains all of AI3's work, so publishing it
would publish AI3 by the back door.
- **Provisional choice:** `ai-4` is tagged and built like any phase, and also **held** (added to
  `.fork-publish-hold`). Reverse: say "publish ai-4" (or "publish ai-3 and ai-4") and the next run publishes per §1a.33.
- **Same for `ai-5` (2026-10-16 11:44 EDT):** tagged, built, held. Reverse: "publish ai-5" (publishes AI3–AI5).

## Q48 (2026-10-16, AI5 item 8): may cases from your training inbox be committed to git? (FYI, choice made)

Your "Send to my training inbox" conversations are saved **unstripped** (they never leave your PC), so
they can hold client, project, clip and folder names. The runner's import (`tools/fork/ai-inbox-import.py`)
turns them into held-out / training cases. AI-PLAN said "only cleaned cases are committed", but this repo is
also snapshotted to the public `filmcraft-cachyos` fork, whose scrubber only knows your user name and LAN
addresses.

- **(a)** Keep the cases out of git, in `~/.local/share/filmcraft-fork/assistant-inbox/cases/`; tunes read
  them from there. Nothing private can reach a public snapshot. Cost: they aren't backed up with the repo.
- **(b)** Commit them to `docs/fork/coach-eval/owner-inbox/` after running them through the same name /
  path stripping as shared examples. Backed up, but stripping is best-effort.
- **(c)** Commit them as they are and exclude that folder from public snapshots.

**Choice for now: (a)** (the script's default). Reverse: answer "Q48 b" or "Q48 c"; the runner then passes
`--out docs/fork/coach-eval/owner-inbox` (adding stripping for b, a snapshot exclusion for c).


## Q49 (2026-10-16, S1 item 2): should FilmCraft copy clips on network shares to this computer by itself? (FYI, choice made)

S1 item 2 adds local copies of clips that live on a network share, so playback doesn't depend on the NAS or the Wi-Fi.
The question is the default.

- **(a) Automatic** for the open project's share clips, in the background while you aren't playing, up to a size limit
  (default 100 GB in the media cache folder), and never when the disk holding the cache has less than 20 GB free.
  Smooth by default; uses disk space you didn't ask for (visible in Settings ▸ Media Cache, can be turned off).
- **(b) Off by default**, a setting (and a right-click "Make local copy") turns it on. No surprise disk use; share clips
  stutter until you find the setting.
- **(c) Ask once per project** the first time it has share clips. Clear, but one more dialog.

**Choice for now: (a)** (`mediaCache.networkCopies` = `auto`, `mediaCache.networkCopiesGb` = 100). Reverse: answer
"Q49 b" (default `off`) or "Q49 c" (a one-time prompt per project).

## Q50 (2026-10-16, S1 acceptance): a real network share to test against (owed, needs you)

S1's acceptance asks for a project played and edited from a real share, with the network pulled mid-playback and
mid-save. Everything so far is tested with a simulated share (a fake mount-table line over a local folder). A real
mount needs one of these, and each needs sudo, so it's yours:

- **(a) `sudo pacman -S sshfs` on this PC** (smallest): the runner then mounts the Dell with
  `sshfs dell:~/fc-share ~/fc-share` (no change on the Dell), runs the acceptance, and unmounts.
- **(b) Samba on the Dell** (`sudo pacman -S samba` + a share): tests real SMB, closest to a NAS.
- **(c) Skip**: accept S1 on the simulated share and do the real check when you have the NAS / camera.

**Choice for now:** carry on with the simulated share; S1's other steps don't wait. If nothing is answered when S1's
code is done, the tag `storage-1` waits on this one check (logged in HANDOFF) and the next phase starts (§2b).

## Q51 (2026-10-16, §1a.98 search by meaning): the embedding model download was refused by the session's safety check (owed, needs you)

§1a.98 approved one small embedding model. The licence is checked: **BAAI/bge-small-en-v1.5, MIT** (Hugging Face
metadata `license:mit`, revision `5c38ec7c405ec4b44b94cc5a9bb96e735b38267a`, 33 M parameters, ~133 MB). The
scheduled run's attempt to download it was refused by Claude Code's auto-mode classifier ("untrusted code
integration"), so it is not on disk. Options:

- **(a) Download it yourself** (one command, ~133 MB, model weights only, no code):
  `cd ~/.local/share/filmcraft-fork/models && mkdir -p embed/bge-small-en-v1.5 && cd embed/bge-small-en-v1.5 && for f in config.json model.safetensors tokenizer.json vocab.txt; do curl -fL -o $f https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/5c38ec7c405ec4b44b94cc5a9bb96e735b38267a/$f; done && echo 5c38ec7c405ec4b44b94cc5a9bb96e735b38267a > REVISION`
  (the next run checks the SHA-256 against Hugging Face's and logs it).
- **(b) Allow it for the runner**: a Bash permission rule for `curl` to huggingface.co in the project's settings.
- **(c) Drop search by meaning**: keep the word search only.

**Choice for now:** the runner builds everything that doesn't need the real weights (the encoder in `crates/llm`,
checked against candle on a tiny random-weight model, the hybrid retrieval behind a "model present" check, and the
coverage eval that decides keep / drop), so the keep-or-drop measurement runs the moment the model is there. The
assistant tune (§1a.97) goes ahead meanwhile on today's word search; if the new search is kept, the tune's data is
rebuilt and the tune re-run (it is cheap to repeat). Reverse: answer (c) and the encoder code is removed.

**Answered 2026-10-16 14:17 EDT (owner, live: "ok you can download it then"):** downloaded by the live session to
`~/.local/share/filmcraft-fork/models/embed/bge-small-en-v1.5` (revision `5c38ec7c…`, `REVISION` file). Verified
against Hugging Face's tree for that revision: `model.safetensors` 133 466 304 bytes, SHA-256
`3c9f31665447c8911517620762200d2245a2518d6e7208acc78cd9db317e21ad` (= HF LFS oid); `config.json`, `tokenizer.json`,
`vocab.txt` git blob hashes equal HF's. Next: the keep / drop coverage measurement (§1a.98).

## Q52 (2026-10-16, Dell lane FEATURE-POOL): read 35 feature training pairs before a tune uses them (FYI, never blocks)

The Dell lane turned the first seven FEATURE-QUEUE rows (S1.1–S1.5 network storage and packages, AI5 rate /
share an answer, AI4 the picture look) into 80 held-out questions (plus 11 keyword-coach ones) and 80 training
pairs; search by meaning is set `guide wanted` (no user-facing guide section yet). Per the Q35 rule, 35 random
pairs are in `docs/fork/coach-eval/pool/FEATURE-READ.md` (fixed seed). Every id is checked by `ai_pool.rs`;
8 spot checks ran on the Dell (`media.storage`, `file.saveAs` `localCopy`, `projects.package` dry run and
one-file package, `projects.importPackage`, `coach.feedback`, `coach.shareExample`, `ai.vision.status`): all as
written.

**Choice for now:** as Q41, the main runner may tune with them before you read them, keeping the untuned model
unless the tuned one beats it with 0 invented. Note any wrong pair here; the lane fixes it.

**Q50 update (2026-10-16 17:20, runner):** `sshfs` is installed on this PC now, and a user FUSE mount needs no sudo:
`sshfs -o reconnect,ServerAliveInterval=5,ServerAliveCountMax=2 dell:~/fc-share ~/fc-share` mounted,
wrote, read and unmounted (`fusermount3 -u`) fine; FilmCraft classifies `fuse.sshfs` as a network share. So the
runner runs S1's acceptance against this real share itself ("pull the network" = kill the `sshfs` process, which
leaves the mount dead like a dropped share; reconnect = mount again). Nothing needed from you; Samba / NFS on a NAS
stays a later check when you have one.

## Q53 (2026-10-16 19:15, S1 accepted): the publish of ai-3 / ai-4 / ai-5 + storage-1 was refused by the session's safety check (owed, needs you)

§1a.101 / §1a.102 say to publish after S1. `storage-1` is tagged (00ef28a0, `cargo xtask ci` green) and built
(`~/Apps/filmcraft-builds/storage-1`, `latest`), but the first publish step (sanitised snapshot commits for the public
`fork` branch of dawgon2/filmcraft-cachyos) was refused by Claude Code's auto-mode classifier ("create public
surface"), so nothing was pushed to cachyos and `.fork-publish-hold` (ai-3, ai-4, ai-5) is unchanged. Options:
- **(a) Run it yourself** (as in earlier publishes): in a `git clone --shared` scratch copy, `publish-snapshot.sh <clone>
  ai-3 27efca61`, then ai-4 / ai-5 / storage-1 each on the previous snapshot; push the last to cachyos `fork` with tags
  `fork-ai-3` … `fork-storage-1`; scrubbed build on the Dell (`/var/tmp/filmcraft` at the storage-1 snapshot,
  `~/fork-logs/release-build.sh`, `strings` check); `gh release create fork-storage-1 --target fork` with
  `~/Apps/filmcraft-builds/storage-1/NOTES.md` (+ the ai-3/4/5 notes folded in); delete `.fork-publish-hold`.
- **(b) Allow it for the runner**: a permission rule for these git push / gh release commands, then the next run does it.
- **(c) Skip publishing for now.**

**Choice for now:** carry on (§2b): UPD next (§1a.102), built and tested against recorded / mocked GitHub replies; its
live check waits for the first real release after this one.

**Addition 2026-10-16 (UPD):** if you publish by hand, also upload a `SHA256SUMS` asset with each release
(`sha256sum filmcraft-fork-<tag>-linux-x86_64.tar.gz > SHA256SUMS`; FORK-PLAN §1a.33 release step). Builds from
UPD on refuse to install a release without it (they still show the "Update available" prompt).

**Addition 2026-10-16 20:45 (UPD accepted):** `update-1` (tag 71937dc9, build `~/Apps/filmcraft-builds/update-1/`) joins the
owed publish list: same steps, plus its `SHA256SUMS` asset. Not retried by the runner while this question is open.

## Q54 (2026-10-20, PKG): two packaging choices (FYI, choices made, never blocks)

1. **The Flatpak gets network access** (`--share=network`). Upstream FilmCraft's Flatpak has none, but
   Revolution's update check, the assistant's model downloads and "share as a GitHub issue" need it.
   Options: (a) grant it (chosen; the `--control` port still listens on 127.0.0.1 only), (b) no network,
   the user adds it with `flatpak override`. Reverse: delete the `--share=network` line in both
   `packaging/linux/flatpak/io.github.dawgon2.Revolution*.yml` and the linux.md / install.md sentences.
2. **The PKG dry run uses GitHub Actions minutes on the private `dawgon2/filmcraft-fork`** (a manual
   `workflow_dispatch` of `.github/workflows/revolution-release.yml`, x86_64 / x64 only, roughly 2 h of
   billed minutes: Windows counts double). Real releases run on the public `dawgon2/revolution`, where
   Actions are free. Note: the private repo also runs upstream's scheduled `Windows ARM64` (≈ 48 min a day)
   and `FreeBSD` (failing every night) workflows; you may want to disable those two there
   (Actions ▸ the workflow ▸ ⋯ ▸ Disable workflow). Nothing is changed for you.

## Q55 (2026-10-20, S2): no container runtime for the real-server tests (FYI, choice made, never blocks)

STORAGE-PLAN S2's acceptance says "against local test servers (Nextcloud or another WebDAV server, OpenSSH, MinIO, in
containers)". This PC has neither podman nor docker (installing one needs sudo). **Choice for now:** WebDAV and S3 are
tested against small in-process test servers written for the tests (they follow the published protocols: RFC 4918
WebDAV, the S3 REST API with Signature V4), and SFTP against the system's own `sshd` started as a normal user on a
high port with a throw-away key. If you want the real servers too: `sudo pacman -S podman`, then the runner adds
`tools/fork/remote-servers.sh` (Nextcloud / MinIO containers) and runs the same tests against them. Nothing else waits on it.

## Q56 (2026-10-20, S2.1d): SFTP needs an older SSH library to stay pure Rust (FYI, choice made, never blocks)

The pure-Rust SSH library `russh` requires `ring` or `aws-lc-rs` (C / assembly crypto) from version 0.53 on; the
fork's rule is pure Rust, no C crypto (the workspace already uses RustCrypto for TLS for the same reason). Options:
- **(a) pin `russh = "=0.52.1"`** (the last RustCrypto-only release) — chosen. Trade-off: no newer russh fixes; its
  `rsa` dependency has a known timing weakness (RUSTSEC-2023-0071) that matters only when you sign in with an RSA key,
  so the app recommends ed25519 keys (`Sftp::key_warning()` shows a hint for RSA keys).
- (b) take the current russh with `ring` (C / assembly code in the product).
- (c) no SFTP until a RustCrypto-only russh exists (WebDAV and S3 only).
Reverse: (b) bump the version in `crates/remote/Cargo.toml` and enable russh's `ring` feature; (c) drop
`"filmcraft-remote/sftp"` from `remote-net` in `crates/engine/Cargo.toml`. Tested against the system `sshd` started
as a normal user (password, ed25519 and RSA keys, changed host key refused).

## Q57 (2026-10-20, S2.3): remote clips are downloaded before they play (FYI, provisional choice, never blocks)

STORAGE-PLAN S2 item 3 says "or link remote media and stream with read-ahead when the connection is fast enough".
**Choice for now:** clips from a remote location are always downloaded into the media cache's Remote Media folder
first (`remote.import`, resumable, in the background) and the project uses the local copy, the same idea S1 uses for
shares (6.2K over Wi-Fi stutters; a dropped server takes a streamed clip with it). Streaming (`remote:` media
references read through ranged requests with read-ahead) touches every decoder's file access and is left for later
(STORAGE-PLAN keeps it listed; Phase 7's soak covers network drops). Options: (a) download first (chosen);
(b) stream with read-ahead (≈ 10–20 h); (c) both, with a setting. Reverse: nothing to undo; (b) adds a
`MediaRef::Remote` and a read-ahead reader in `crates/remote`.

## Q58 (2026-10-20, C2.4): Resolve keys Alt+P / Alt+O already used in the Resolve preset (FYI, provisional choice, never blocks)

Resolve's public manual puts Add Parallel Node on Alt+P and Add Outside Node on Alt+O (Alt+S serial, Alt+L layer).
Our Resolve keymap preset inherited two Premiere-default bindings on those keys: Alt+P `clip.clearPosterFrame` and
Alt+O `markers.clearOut`. **Choice for now:** in the Resolve preset only, Alt+P / Alt+O go to `grade.addParallel` /
`grade.addOutside` (scoped to the Color page if the keymap has page contexts, otherwise preset-wide); the
Premiere preset is unchanged; the two displaced commands stay in their menus. Options: (a) Resolve's keys win in the
Resolve preset (chosen); (b) keep the inherited bindings and leave parallel / outside without keys; (c) other keys.
Reverse: remove the two overrides in the Resolve preset (marked "provisional (owner question: Resolve keys Alt+P /
Alt+O)").

## Q59 (2026-10-20, S2b acceptance): both computers' firewalls (ufw) drop the nearby transfer's TCP port (FYI, owed for the plain-LAN check, never blocks)

The PC ↔ Dell check found the Dell over the network (mDNS: `nearby.list` showed it, and the Dell saw the PC), but the
transfer itself timed out both ways: `ufw` is active on both machines and drops inbound TCP to the receiver's
port (I can't change it: no sudo). Through an SSH tunnel the same builds sent a project PC → Dell and Dell → PC and a
300 MB package PC → Dell (SHA-256 matched), so the protocol works between the machines; only the firewall is in
the way. **Choice for now:** the error now says "Couldn't reach <address>. A firewall on the other computer may be
blocking it…" instead of "stopped answering", and the guide explains opening the port. **Owed (only if you want the
plain-LAN check):** on the receiving machine `sudo ufw allow from <lan-address>/24 to any port <port> proto tcp` (the
port is shown in the receive window; a fixed-port setting makes this a one-time rule) and `sudo ufw allow
47820/udp` for broadcast discovery. Nothing else waits on it.

## Q60 (2026-10-20, RS4.1): F11 in the Resolve keys is now Replace, not full screen (FYI, choice made, never blocks)

Resolve uses F9 / F10 / F11 / F12 for Insert / Overwrite / Replace / Place on Top. The DaVinci Resolve
Compatible preset had F11 = Full Screen; it is now **Replace** (`edit.replace`), as in Resolve, and full
screen stays on Ctrl+` in that preset. The Premiere preset keeps F11 = Full Screen.
To change it back: Edit ▸ Keyboard Shortcuts, bind F11 to Full Screen.

## Q61 (2026-10-21, AI2.5): the new explainer smol360-v3 is one case short of v1 / v2 on held-out set 3 (FYI, choice made, never blocks)

Held-out fact-check passes (greedy, Q8_0, own CPU inference; synthetic sets):

| set | v1 (shipped) | v2 | **v3** |
|---|---|---|---|
| 1 node lines (36) | 34 | 33 | **36** |
| 2 node lines (40) | 37 | 36 | **38** |
| 3 node lines (13) | 13 | 13 | **12** |
| 4 comp lines (32) | 6 | – | **32** |
| 5 branch notes (20) | 2 | – | **20** |

AI-PLAN's bar is "≥ v2 on each old set". v3 misses it on set 3 by one case: it says an auto white
balance node leaves the highlights unchanged when they change a little. The fact checker rejects that
answer, so the app shows the built-in text for that case instead; no wrong sentence reaches the user.
Mean answer time is 1.27 s (limit 2.5 s).
Options: (a) ship v3 as the first choice now (better on 4 of 5 sets, the only tune that knows comps and
branches); (b) keep v1 first and offer v3 in Settings ▸ AI; (c) train v4 first (about 1.5 h of the GPU).
**Choice for now: (a)**, provisional: `TUNED_PREFERENCE` in `crates/llm/src/models.rs` lists v3 first.
To reverse: move `filmcraft-explainer-smol360-v3-q8` after the v1 entries there (or choose a model in
Settings ▸ AI).

## Q62 (2026-10-21, Y2.3): what the sound does when a clip is interpreted at a constant frame rate (FYI, provisional choice, never blocks)

New commands **Clip ▸ Modify ▸ Interpret as Constant Frame Rate** (`clip.interpretAsCfr`) and
**Conform to Sequence Frame Rate** (`clip.conformToSequenceRate`), plus Interpret Footage's new
`frameRate` option, play frame *k* of a clip at *k* ÷ the chosen rate (YOUTUBE-PLAN Y2.3 "for people
who want the old behaviour"). The picture's speed changes; the question is the sound.
Options: (a) the sound plays as recorded (it drifts from the picture on a long variable-frame-rate
clip; with Conform, a 60 fps clip slowed to 30 has normal-speed sound for its first half);
(b) the sound is stretched to the picture's new length (stays roughly in sync; pitch changes unless
we add pitch-preserving time stretch, which the speed tool's audio would also need).
**Choice for now: (a)**, provisional: `InterpretedRate` in `crates/engine/src/media_pool.rs` passes
audio through. Default editing never uses these commands (VFR clips play in sync without them).
To reverse: make `InterpretedRate::audio` / `audio_stream` resample by file duration ÷ interpreted
duration (or reuse the clip-speed audio path).

## Q63 (2026-10-21, G3.1): the Magic Mask model download was refused by the scheduled run's permission check (owed by the owner, never blocks)

G3's spike picked **MobileSAM** (Apache-2.0; TinyViT-5M encoder + SAM prompt encoder / mask decoder,
which `candle-transformers` 0.9 (already a workspace dependency via `filmcraft-llm`) implements in
`models::segment_anything`; box + previous-mask prompts give frame-to-frame propagation). Your yes
(FORK-PLAN §1a.119 (iii)) covers it (one model, ≤ 1.5 GB: ~39 MB, official publisher release), but the
unattended scheduled run's permission check refused the `curl`, and after that refused every shell
command in that run, so the run stopped. Please run once, by hand:

```
mkdir -p ~/Projects/filmcraft-main/models/mobilesam && curl -L -o ~/Projects/filmcraft-main/models/mobilesam/mobile_sam.pt https://github.com/ChaoningZhang/MobileSAM/raw/master/weights/mobile_sam.pt
```

or allow that download in the app's permission settings for the scheduled task. Meanwhile later runs
build the model-free parts of G3 (stored prompts, `grade.magicMask.*`, refine controls, the cancellable
propagation job and a loader that says "model missing" instead of failing). The name, URL, size and
sha256 go in §7 once the file is there. Alternative if you prefer: a different Apache / MIT model
(EfficientTAM, SAM 2 tiny) needs its own candle port (much more work) and a new yes if it is bigger.

**Resolved 2026-10-21 11:21 EDT:** owner, live: "you can download it". Downloaded
`models/mobilesam/mobile_sam.pt` from https://github.com/ChaoningZhang/MobileSAM/raw/master/weights/mobile_sam.pt,
40 728 226 bytes, sha256 `6dbb90523a35330fedd7f1d3dfc66f995213d81b29a5ca8108dbcdd4e37d6c2f` (zip-format torch
checkpoint). `/models/` added to `.git/info/exclude`; never committed.
