# Handoff: FilmCraft fork (6.2K HEVC on CachyOS + RX 6700 XT)

For a fresh session. Read `FORK-PLAN.md` first (§1 fork rules, **§1a standing instructions** incl. §1a.6 order and the
newest entries **§1a.97–102**; §5 phases; only the last ~80 lines of §7), then [`FORK-ONLY.md`](FORK-ONLY.md),
[`OWNER-QUESTIONS.md`](OWNER-QUESTIONS.md) (Q51 answered, **Q52** FYI, **Q53 owed**), this file. Rewritten at its top
2026-10-16 19:20 after S1's acceptance (scheduled run); older notes below are kept for reference.

## Exact next step (2026-10-18, laptop session): XPS signed off (`xps-1`, local); next: owner pushes, then S2

**State:** `revolution-wip` at eef8b517, tag `xps-1` on it, **both unpushed** (owner approves pushes and tag pushes; ~45
commits ahead of `origin/revolution-wip`). Build `~/Apps/filmcraft-builds/xps-1` (`latest`), launcher "Revolution".
Every number is synthetic. Details: FORK-PLAN §7 2026-10-17 19:45 → 2026-10-18.
**Done this session:** 8.6d two-GPU export (NVENC lane + VA-API lane, `hev1`; 63–69 s vs 80–103 s on the 4050 alone);
NVENC → VA-API mid-export fallback; NVDEC-first stepped aside on bound threads (the bug that got the Claude app killed
by systemd-oomd); monitor dropdowns / Scopes footer at 2×; D2 NVENC encodes from GPU memory (94 vs 151 s, byte-identical
files); B3 NVDEC 13 slots at 6.2K (Performance playback 0.17 % dropped); §1a.105 (b) start without NVIDIA's Vulkan
driver (no NVIDIA device file held on Power saving / Auto); **never crash on a lost window GPU** (restart + `--recover`,
reproduced); llm test exit crash; `shared_picture` Linux-only (wasm); `va_interop` test on the decode GPU. D1 (H.264 /
HDR exports on the GPU) moved to Phase 8 item 8 (owner).
**Next:** owner OK → push `revolution-wip` + tag `xps-1`, publish per §1a.33; then S2 (§1a.104). Before more laptop
sessions read the gotchas below.
**Owner to do / decide:** (1) approve the push (+ tag); (2) measure the start-up D3 yourself: close System Monitor's GPU
sensor (KDE ksystemstats runs `nvidia-smi dmon`, which keeps the RTX 4050 awake), start Revolution on Auto,
`cat /sys/bus/pci/devices/0000:01:00.0/power/runtime_status` should read `suspended` within ~6 s; (3) **memory on the
16 GB laptop:** 6.2K playback + a 4K export + the render cache's background parts exceed what is free (swapping; the Arc
ran out of GPU memory; the 4050's two-GPU export ran at 2.7 fps in the app vs 19 fps in the bench): decide whether the
render cache / auto proxies should back off on < 32 GB machines or under memory pressure; (4) FYI: after a real GPU
hang the app abandons the device on a thread; quitting during that teardown could crash at exit (work saved first);
(5) FYI: `revolution --version` still prints "filmcraft"; the old `filmcraft-fork.desktop` launcher points at
`~/Apps/filmcraft-laptop` (left as is).
**Gotchas (this session):** run heavy things in `systemd-run --user --scope -p MemoryMax=… -p MemorySwapMax=…` (an
uncapped bench and an uncapped UI test suite each got the Claude app killed by oomd); give NVIDIA runs swap and ≥ 12 GB
(a cap without swap starves NVIDIA's pinned memory and silently turns NVDEC sharing off); killed bench runs leave
`/tmp/filmcraft-bench-xm5-*` (tmpfs, RAM): bench runs now use `TMPDIR` under `target/`; the full UI suite in one process
needs > 14 GB: run it one module per process; always put a timeout on test runs (a GPU test hung 17 h); bench `--only`
matches the short case name (`play-full`, not `xm5-play-full`); zsh doesn't split `set -- $var` (use `bash -c`).

## Was: Exact next step (2026-10-17 18:40, laptop session): XPS §3 items 1–17 pass; 8.6c in; next 8.6d, then sign-off (`xps-1`)

**State:** `revolution-wip` HEAD 036c48c7, **not pushed** (owner approves pushes). Every number is from synthetic clips.
**Done since 17:10 (FORK-PLAN §7 17:40 → 18:35):** the `va_422_handoff` failure was a real bug (linear copy surfaces
shared between copiers: a clip after another of the same size showed the previous clip's pictures), fixed with per-copier
pools + regression test; item 13 mains half (back to Full ~7 s after plugging in); items 15 / 16 with the owner changing
the panel to 150 % / 200 % on the Arc and the RTX 4050 (all good; cosmetic: Lumetri Scopes footer squeezes "Rec. 709" at
2×); the "slow Performance export" was the bench not engaging 8.6c — with it xm5-export-4k on the 4050 is 73.4 s (was 449),
graded 76.8 s (the Arc: 50.5 / 89.9 s).
**Next, in order:** (1) **8.6d** two-GPU export (NVENC lane + VA-API lane; `export_lanes` only pairs VA-API nodes today;
mid-stream NVENC → VA-API fallback belongs here too); (2) small leftovers: narrow-monitor dropdowns at 2×, the Lumetri Scopes
footer at 2×, the frame cache's colour management still reads NVDEC pictures back to the CPU; (3) sign-off: `cargo xtask ci`
(machine idle, `-j 6`), `va_422_handoff` (now 6 / 6), tag `xps-1`, build per §1a.9.
**Owner decisions open:** H.264 Hardware encoding uses NVENC on Power saving (wakes the dGPU; no VA-API H.264 encoder);
the ~20 s dGPU wake at every start (needs `VK_LOADER_DRIVERS_SELECT`, outside the rules). **Owner to do:** approve the
push; delete the `xps-test-*` projects.

## Was: Exact next step (2026-10-17 17:10, laptop session): 8.6c in; next the NVIDIA export bottleneck, 8.6d, then sign-off (`xps-1`)

**Owner order (relayed 2026-10-17):** item 18 (8.6c **and** 8.6d) before tagging `xps-1`; then sign-off: `cargo xtask ci`,
re-run `va_422_handoff`, tag, build. Leave for the owner only what needs a person, batched at the end; no questions meanwhile.
**Since 07:35 (FORK-PLAN §7 08:00 and 17:00; all unpushed on `revolution-wip`, HEAD 197b7fff):** item 1 re-checked after the
Auto change; items 15 / 16 solo parts (page bar and monitor timecodes fit at 2× UI scale; kscreen-doctor declined: OS
display settings); **8.6c done**: NVDEC pictures stay on the NVIDIA GPU (Vulkan opaque-fd memory imported by CUDA;
`nvdec::shared`, `interop::alloc_cuda_shared` / `import_cuda_shared`, `shared_picture`), Performance playback 92.6 → 0.6 %
dropped (synthetic 6.2K). **Next, in order:** (1) the Performance export is still ~3 fps vs ~35 on the Arc, ~300 ms CPU a
frame, not the decode transfer — find it (export trace counters don't print on this path; no `perf` installed: ask the
owner to install `perf`, or instrument); (2) 8.6d two-GPU export (NVENC lane + VA-API lane; `export_lanes` only knows VA-API
nodes); (3) the CPU paths (frame cache colour management) still read NVDEC pictures back; (4) sign-off. **Fixed 17:40:** the `va_422_handoff` failure was a real bug
(linear copy surfaces shared between copiers; §7 17:40), now 6 / 6.
**Owner end list:** plug-in half of item 13 (playback
back to full within ~10 s); item 15/16 with the panel at 1.5× / 2× (kscreen-doctor or System Settings) on Auto and
Performance, look for black / corrupt frames; delete the `xps-test-*` projects; approve pushes of `revolution-wip`.

## Was: Exact next step (2026-10-17 07:35, laptop session): XPS §3 A–D done on the XPS 16; next 8.6c (now the priority), then `xps-1`

**Where:** branch `revolution-wip` (merged with `hevc-vaapi` @ 757c622f at 6991cf99), commits up to dbfd7604, **not pushed**
(owner approves pushes; the desktop session merges `revolution-wip` into `hevc-vaapi`). Every number is from synthetic
clips (no real X-M5 footage). Details: FORK-PLAN §7 2026-10-17 02:40 → 07:30.

**Results (XPS-SETUP §3):** 4 pass (P1–P7 GUIDs = nvEncodeAPI.h 12.1, pinned). 5 fixed (HEVC NVENC tests were Windows-only;
16 green on the RTX 4050). 6 **fixed**: every NVENC H.265 export on Linux failed at frame 0 (planar GPU frames); Performance →
NVENC, Power saving → VA-API on the Arc; P1/P4/P7 logged; Speed ≈ Balanced in real exports (pipeline-bound), Quality ~2×
slower; Power saving no longer wakes the dGPU for an NVENC probe. 7 **enabled**: NVENC Main 10 / PQ / HLG on Linux (tags,
HDR10 SEI, Y PSNR 49.7 dB vs source). 8 **fixed**: cancelling / failing an NVENC export segfaulted the app (drain before
teardown); simulated loss → error naming NVENC; no mid-stream VA-API fallback (left for 8.6d). 9 pass + **crash fixed**
(concurrent vaGetImage in iHD) + `FILMCRAFT_NO_VA_DECODE`; lazy NVDEC probe fires and decodes. 10 **fixed**: Intel → NVIDIA
4:2:2 hand-off (Y210 as Rg16 pairs; NVIDIA has no 64-bit DMA-BUF formats). 11 pass (vainfo oracle test). 12 pass. 13 battery
half pass (`perf.stats` `effectiveResolution`); **mains half owed** (plug in, check it returns within ~10 s). 14 pass (portal
inhibit). 17: **Auto now picks the Intel iGPU on battery-powered Intel + NVIDIA computers** (0.3 % vs 86–93 % dropped,
4K export 47–51 s vs 424–449 s, ~⅕ energy on battery). Also: Play waits for the Project Manager (owner); NVIDIA's VA-API
node never opened (dGPU wake); refused VA-API nodes cached; bench `FILMCRAFT_GRAPHICS_DEVICE`; xm5 stand-ins bounded to
6.6 GB (they OOM-killed a session). Items 15–16 (HiDPI 1.5× / 2×, NVIDIA Wayland suspend/resume) not run: need the owner.

**Why 8.6c first:** on the RTX 4050 every Intel-decoded picture is read back through the CPU (NVIDIA imports no Intel
tiled modifier; a linear two-plane NV12 copy hung NVIDIA's queue in vkWaitSemaphores, no Xid; the one-plane Y210 copy works),
and NVDEC frames are read back too. Keeping NVDEC frames on the GPU (Vulkan ↔ CUDA) is what can make Performance worth it.
**Owner decisions open:** H.264 "Hardware encoding" uses NVENC even on Power saving (no VA-API H.264 encoder); each start
wakes the dGPU ~20 s (Vulkan ICD enumeration; fix needs `VK_LOADER_DRIVERS_SELECT` or raw Vulkan, both outside the rules).
**Gotchas:** run ffmpeg 6.2K encodes alone (6.6 GB); `-j 6`; `--control` turns desktop integration off unless
`FILMCRAFT_DESKTOP_INTEGRATION=1`; `export.quick {wait:true}` runs on the UI thread; zsh doesn't split `$var` (use bash);
`pgrep -f` patterns match their own shell.

## Was: Exact next step (2026-10-17, live): Revolution 0.1 RELEASED; next XPS on the laptop (`xps-1`), then S2

**Done 2026-10-17 ~02:15:** everything in the paragraph below up to the launcher (see §7). Remaining: the laptop session works from `hevc-vaapi`; delete `.fork-paused` when the main runner should resume.

### Was: Revolution rebrand → release Revolution 0.1

**Owner, live (FORK-PLAN §1a.103):** the fork is now **Revolution** (asked first as Renaissance). Everything the user
sees says Revolution; internals keep `filmcraft-*`. `tools/fork/rebrand.py` does the string part and is idempotent:
**re-run it after every upstream sync**, then the gates. Also: ArtCraft marks removed (docs/brand licence: a modified
build must not carry them), Help / About link to dawgon2/revolution, header Discord button gone; photo app icon
(`tools/fork/revolution-icon.sh`, source `assets/app-icon/revolution-photo.png`, public domain); app id
`io.github.dawgon2.Revolution`; data folder `revolution` with fallback to an existing `filmcraft` one; old "FilmCraft
Default" preset and "FilmCraft … (renderer)" names read as Revolution's; updater channel `dawgon2/revolution`, tags
`revolution-<ver>`, tarball `<tag>-linux-x86_64.tar.gz`, program `revolution` (older `filmcraft` accepted).
Work is on branch **`revolution-wip`** (pushed to origin so the laptop can clone it). **Next:** `cargo xtask ci` green
→ fast-forward `hevc-vaapi` → tag `revolution-0.1` → scrubbed Dell build (binary renamed `revolution`,
`revolution-cli`) → rename GitHub repo filmcraft-cachyos → revolution (+ issues on, default branch `fork`) → snapshot
"Revolution 0.1" on `fork` (publish-snapshot.sh, title changed) → release with tarball + SHA256SUMS + NOTES →
delete `.fork-publish-hold` → local build + launcher "Revolution". The laptop (XPS 16) is set up by the owner and
its own session works from `revolution-wip` / `hevc-vaapi` (XPS-SETUP §3); `.fork-paused` can go once 0.1 is out.
**After `xps-1`: S2** (owner §1a.104), and S2 renames packages `.fcpackage` → `.revpkg` (both open).

## Was: Exact next step (2026-10-16 23:04 EDT): PAUSED for the laptop. On resume: XPS with the laptop (tag `xps-1`)

**State:** XPS pre-laptop (§1a.99 a–f) is done and accepted with fakes (ci green at 5210abfb; build
`~/Apps/filmcraft-builds/xps-pre`, `latest` → it, not tagged / published). `.fork-paused` exists with the line
`dell-lane: feature-pool` (§1a.79 / §1a.100): the main runner stops at its gate; the Dell lane only runs queued
FEATURE-POOL rows. The owner resumes by deleting `.fork-paused` (or saying so live) once the XPS 16 is set up.

**On resume:** (1) follow `docs/fork/XPS-SETUP.md` §1 checks (owner installs) and run `tools/fork/xps-probe.sh` on the
laptop (ssh `xps`, like the Dell); (2) work through XPS-SETUP §3 items 1–18 in order, fixing with tests; (3) then
8.6c zero-copy (Vulkan ↔ CUDA) and 8.6d two-GPU export (NVENC lane + VA-API lane); (4) accept: plays and exports on
the iGPU, the dGPU and Auto, falls back cleanly, never crashes; tag `xps-1`, build per §1a.9, publish per §1a.33
(Q53 decides how). After `xps-1`: the rest of S1 → REF → C2 → AI2 → A3 → S2 → G3 → AI6 → 7 (+ Y4) → … (§1a.99 / §1a.102).

**What XPS pre-laptop built (details §7 2026-10-16 20:48 – 23:05):** `filmcraft_gpu::adapter_pick` (one GPU rule:
window, interop, headless, export renderer; Settings Power saving / Performance; `chosen_vendor`); VA-API nodes by maker
(AMD, Intel, other, NVIDIA); NVDEC behind another GPU's VA-API (`nvdec_in_front`, `FILMCRAFT_NVDEC_FIRST`); NVENC
presets P1–P7 from Encoder speed (P5 fallback; P1/P2/P3/P6/P7 GUIDs unchecked against the header: XPS-SETUP item 4);
NVENC H.265 on Linux in Auto (`hevc_export::nvenc_place`, 8-bit SDR only, no mid-stream switch); shared-memory decode
budget (`decode_memory`); `filmcraft_engine::power` + Settings ▸ Playback ▸ Lower playback resolution on battery
(peripheral batteries ignored); runbook + probe. Known gaps listed in xps-pre/NOTES.md.
**Gotcha:** new guide text with "laptop" / "nothing" moved ai_pool floors; check `--test ai_pool` after guide edits.

## Was: Exact next step (2026-10-16 20:57 EDT): XPS b (NVENC on par), then c–f, then the pause

**XPS a done** (2026-10-16 20:57 EDT, see §7): `filmcraft_gpu::adapter_pick` is the one GPU rule (window, interop, headless, export renderer); Settings gains Power saving / Performance; VA-API ranks nodes by maker (Intel before NVIDIA). Left from (a) for later / the laptop: Power saving → VA-API `prefer_gpu` (Intel node) and the sleeping-dGPU check (probe item). **XPS b done** (2026-10-16 21:05 EDT): presets P1–P7; NVENC H.265 on Linux in Auto (first when exports render on NVIDIA, else before x265; 8-bit SDR only; no mid-stream switch, see §7). **XPS c done**: NVDEC behind the iGPU's VA-API. **XPS d done** (2026-10-16 21:26 EDT): shared-memory decode budget, battery preview setting, inhibit already there. **XPS e done** (2026-10-16 21:29 EDT): `docs/fork/XPS-SETUP.md` (install list + laptop test list) and `tools/fork/xps-probe.sh`; peripheral-battery fix in `power`. **Next: XPS f**: `cargo xtask ci` (machine idle), then build `~/Apps/filmcraft-builds/xps-pre` per §1a.9 (NOTES.md; not tagged, not published; leave `latest` → update-1? §1a.9 says latest → newest build: point it at xps-pre), FORK-PLAN §7 + HANDOFF rewrite for the laptop session (start at XPS-SETUP §3), then create `.fork-paused` containing the line `dell-lane: feature-pool` and stop.

## Was: Exact next step (2026-10-16 20:45 EDT): XPS pre-laptop (FORK-PLAN §1a.99 a–f), then the pause

**Order (§1a.102):** … S1 ✓ → UPD ✓ (`update-1`, built, **publish owed: Q53**) → **XPS without the laptop** → **pause**
(create `.fork-paused` containing the line `dell-lane: feature-pool`; the main runner stops; the Dell lane keeps doing
queued FEATURE-POOL rows). After the pause: XPS on the laptop (`xps-1`) → rest of S1 → REF → C2 → AI2 → A3 → S2 → G3 …

**XPS (Dell XPS 16: Core Ultra 7 155H Arc iGPU `renderD128`, RTX 4050 Laptop 6 GB `renderD129`; Linux assumed):**
read §1a.99 in full first. Re-check upstream (§1a.34: NVDEC, NVENC `nvenc/export.rs`, `graphics.rs` #820) and take what
exists. Items: (a) one `pick_adapter(prefs, adapters)` for interop / `graphics.rs` / export renderer, VA-API node by
vendor never node order, Settings ▸ Playback ▸ Graphics device (Auto / Power saving / Performance / by name),
`graphics.listDevices`, honour `DRI_PRIME` / `__NV_PRIME_RENDER_OFFLOAD` / `MESA_VK_DEVICE_SELECT`, never wake a
sleeping dGPU to list adapters; fake-adapter tests for this laptop's layout. (b) NVENC on par (HEVC Main / Main 10,
P1–P7 from the 6E.2 preset, rate control, colour tags, VA-API fallback mid-stream), mocked driver tests. (c) Meteor
Lake iHD caps read from the driver; 4:2:2 10-bit decode on the iGPU, never NVIDIA; the Dell's UHD 770 is the stand-in.
(d) VRAM budgets from real adapter memory, UPower battery / AC (optional lighter preview), suspend inhibit while
exporting, HiDPI / fractional scaling, NVIDIA Vulkan + Wayland quirks guarded. (e) `docs/fork/XPS-SETUP.md` +
`tools/fork/xps-probe.sh` + the laptop test list; guide hardware section for hybrid laptops. (f) ci green, build
`~/Apps/filmcraft-builds/xps-pre` (not tagged, not published), then create `.fork-paused` and stop.
Every feature commit: guide text + FEATURE-QUEUE row (§1a.100).

**UPD state (for reference):** see §7 19:56 / 20:45. Owed: a real update between two releases carrying `SHA256SUMS`
(the release procedure §1a.33 now uploads it). Known limit: `newer()` orders by `published_at` (CLOUD-QUEUE 35).
**Gotchas from this run:** guide retrieval floors break on common words: keep new pages' `does:` fields and titles free
of words the pool questions use ("update", "once", "change", "series"); `--test ai_pool -- --nocapture` and diff the
`misses` lists with / without the page to find the one question that moved. The Dell lane makes pool sets for a new
feature within the hour: fetch + merge before tagging.

## Was: Exact next step (2026-10-16 19:58 EDT): UPD acceptance (ci, tag `update-1`, build, publish), then XPS pre-laptop, then the pause

**UPD items 1–4 are built and tested (mocked GitHub; see §7 19:56).** Left for UPD: `cargo xtask ci` → tag `update-1`
→ build per §1a.9 → publish per §1a.101/102 **with a `SHA256SUMS` asset** (if the safety check refuses again, it stays
owed under Q53). Acceptance item "one real update between two fork releases on the PC" is **owed** until two
releases with `SHA256SUMS` exist (the first real one after this). How to try it then: start the older published
build from `~/Apps/filmcraft-builds/<old>/` with `latest` → it, `app.update.check {"wait":true}` over the control
channel, click Update, check `latest`, Restart Now, Roll Back. Known limit (review #7): `newer()` orders by
`published_at`, so re-publishing an old tag would offer it.
Code map: `crates/engine/src/update.rs` (check, commands), `update_install.rs` (install), `fork_update.rs` (channel,
FORK-ONLY), `crates/ui-egui/src/panels/update_prompt.rs`, Settings row `updateInfo` in `panels/settings.rs`, guide
`docs/guide/updates.md`, UI test `tests/ui/updates_ui.rs` (`FILMCRAFT_UI_SHOTS=<dir>` for screenshots).
**Gotcha:** the guide's retrieval floors broke on the words "update", "once", "change", "series": `does:` fields and
the page title avoid "update"; the setting label is "Check for updates every day". Run `--test ai_pool` after edits.

## Was: Exact next step (2026-10-16 19:20 EDT): UPD (FORK-PLAN §1a.77), then XPS pre-laptop (§1a.99 a–f), then the pause

**Order (§1a.102):** tune + search ✓ → S1 ✓ (`storage-1`) → publish (**owed: Q53**, refused by the session's safety
check; nothing pushed, `.fork-publish-hold` unchanged) → **UPD** → **XPS** without the laptop → **pause** (create
`.fork-paused` with the line `dell-lane: feature-pool`, §1a.99 / §1a.100). After the pause: XPS on the laptop → REF →
C2 → AI2 → A3 → S2 → G3 → AI6 → 7 …

**UPD (§1a.77, tag `update-1`):** (1) a background daily check of GitHub's releases API for the newest `fork-<tag>`
release (offline / rate-limited / hostile replies silent, reply size capped, never a stall); (2) the prompt "Update
available: X → Y" with notes, Update / Later / Don't check, Settings on/off + Check now, says what the check sends;
commands `app.update.check / status / install / setEnabled`; (3) one-click install: download the Linux tarball over
rustls, verify a `SHA256SUMS` asset (add it to the release procedure), unpack beside the build, switch `latest` /
launcher, offer restart, keep the previous build for rollback. Read the rest of §1a.77 first. **Test against
recorded / mocked GitHub replies** (no release since `fork-h264-3`; the live check waits for the next real release,
§1a.99 (iii)). Every feature commit: guide text + a FEATURE-QUEUE row (§1a.100).

**State of this run (2026-10-16 13:57 → 19:20, LEAN):** §1a.97 tune: tinylm + converter learn SmolLM3 (parity 5 / 5),
training data from the app's Pick prompts, LoRA v1 trained and **rejected** (one-section answers; v2 plan in §7);
§1a.98 search by meaning built (own BERT embedder, Words / Meaning / Mixed, eval) and measured, **Mixed not default**
(AI3 −1, daily −2); S1 5d finished (stills, user effects, size estimate, one-file package, trim by stream copy
`filmcraft_isobmff::trim_mp4`, newer-project import); real-share acceptance on sshfs (found + fixed: share writes had
no time limit); ci green; `storage-1` tagged and built (`latest`). CLOUD-QUEUE 32–34 added (meaning search in the
background, S1 UI-thread opens, free-disk floor).

**Gotchas learned this run:** the guide's pool floors are on a knife edge: almost any new guide sentence can push
`pool-c4-apply-still` out (the word "still", "clip", "frames"): run `ai_pool` after every guide edit and reword;
a new topic is safer as its own small section. Builders must use an ABSOLUTE `CARGO_TARGET_DIR` (a relative one
landed inside a crate and got committed: 2.4 GB). Downloads and publishing may be refused by the session's
auto-mode safety check: log an owner question and carry on. `HSA_OVERRIDE_GFX_VERSION=10.3.0` for PyTorch on the
RX 6700 XT outside `run.sh`. Real-model evals: ~1 s per case on the GPU.

## Older notes (kept)

**Owner, live 2026-10-16 ~13:35 (FORK-PLAN §1a.98): Q45 (a)**: search by meaning with one small openly licensed embedding model (runner downloads it; licence + revision + SHA-256 logged), kept only if creative coverage (138 / 608 today) rises without lookup loss; do it with / before the assistant tune (§1a.97) so the tune trains on the new search's prompts. **The rejected picture tune `tuned/qwen3-vl-4b-look-v1` and `text-q4k.gguf` / `text-own-q4k.gguf` were deleted** (owner's yes, disk cleanup; re-make the q4k files with the converter if a test needs them).

**Dell lane FEATURE-POOL (2026-10-16 21:20 EDT; SKILL.md §9, standing):** rows S1.1–S1.5, AI5, AI4, the S1.5 trim row, search by meaning, the S1.5 newer-FilmCraft import row, UPD (`feature-updates`, 14 / 12; no coach questions: Ask the coach skips updates.md) the four XPS rows (`feature-gpu-rule` 11 + 1 coach / 10, `feature-nvenc-presets` 4 / 5, `feature-nvenc-linux` 8 / 6, `feature-nvdec-order` 4 / 3) and XPS d (`feature-battery-igpu` 8 + 1 coach / 8, 21:45 run) done: 147 held-out (+ 16 coach) / 141 pairs in `pool/feature-*` (no creative families: none were honest for status / storage / hand-over / GPU features); 9 spot checks on the Dell all as written (9: trimmed package of fixture pt.fcproj; none possible for the XPS rows: one GPU, no NVIDIA). Floors: coach 142, recall@5 969, creative coverage 559. XPS d: the guide doesn't say whether the battery step applies on Windows / macOS or how fast FilmCraft notices unplugging, so nothing asks. FYI for the guide: the XPS GPU text now outranks color.coach.assistant for `feat-pl-igpu-speed` ("only Intel graphics, how long does a picture look take"), which misses retrieval since XPS a; and "what does power saving integrated gpu do" lands on hardware.switches in Ask the coach, not linux.gpu. Queue empty after XPS c. Newer-FilmCraft import (`feature-project-package-newer`, 3 + 1 / 3): `feat-pk-newer-version` updated from "refused"; the guide doesn't say whether the package must be imported again after updating FilmCraft, so nothing asks. FYI for the guide: `file.projectManager` (Media Management…) also takes `trim`, but the guide only documents `projects.package`'s; and the trim dry-run estimate runs under the real size (447 KB vs 569 KB on a 2 s cut: keyframe lead-in). Search by meaning done from `color.coach.assistant-search` (7 held-out + 1 coach / 6 pairs; no spot check: the model isn't on the Dell). FYI for the guide: it doesn't say how the embedding model gets onto the computer, or that without it the Assistant keeps the word search (the code does), so no question asks either; the section's own `when` phrase "can the Assistant understand other wordings" finds nothing in Ask the coach (`feat-asr-c-meaning`, kept as a miss). Owner read: `pool/FEATURE-READ.md` (35 pairs), FYI Q52. FYI for the guide: `projects.importPackage` with an absolute `folder` nests it under the projects root (`~/Videos/FilmCraft Projects/home/…`): safe, but the guide could say `folder` is a projects-list folder name. Retrieval misses kept as real tests: `feat-ns3-message`, `feat-ns4-read-back`, `feat-pk-collect-files`, `feat-as-frame`, `feat-asr-resolve`.

**Owner, live 2026-10-16 ~04:20 (FORK-PLAN §1a.96): mode floor LEAN** (where the line says MINIMAL, work LEAN; MINIMAL only near the real limits, §1a.53). **Q41 answered:** the owner has read the training pairs, so AI4 item 5 tunes may use them. **AI4 item 3 (picture-model tune), owner ~04:30 / 04:35: no X-M5 footage yet.** Train on synthetic frames + downloaded openly licensed footage by others (download approved: CC0 / CC-BY / CC-BY-SA / PD, licence + source logged per file in `coach-eval/VISION-SOURCES.md`, ≤ 20 GB under `~/.local/share/filmcraft-fork/training-footage/`, never in git). **Log footage required** (F-Log2 first, other logs too; synthetic F-Log2 from open scene-referred footage if scarce). Build `coach-eval/VISION-READ.md` (30–50 frames + labels) and tell the owner (Q44), but **do the tune regardless of the review** (not a gate; later corrections → fix labels, re-tune). Keep-if-better rule stays. `ai-4` after the tune, then AI5.

**Run length (owner, live 2026-10-15 ~22:55, FORK-PLAN §1a.92): never end a run after a step or two.** A run ends only at the 5-hour throttle (> L5 + 20), a phase end, a long context or a truly blocked step, and says which in its last log entry; being over the weekly line only means MINIMAL mode.

**PAUSED 2026-10-15 15:05 (§1a.79 done):** H group published (`fork-h264-3`), AI3 accepted, tag `ai-3`, build `~/Apps/filmcraft-builds/ai-3` (`latest`), **not published** (§1a.80, `.fork-publish-hold`). `.fork-paused` is set; resume only on the owner's word. **On resume:** publish `ai-3` if he has said so (remove it from `.fork-publish-hold`, then §1a.33), reboot check for Q39 (kernel log clean of `vcn_dec_0` fence lines, AMD `vaapi` tests), then the next phase in §1a.6 (C2, `docs/fork/DESIGN-C2.md`).

Was: **NOW (owner, live 2026-10-15 ~18:50, FORK-PLAN §1a.85): resumed.** Main runner: **AI4** (AI-PLAN Phase AI4: measured
picture facts first, then Qwen3-VL 4B (download approved after an Apache-2.0 licence check), then its tune; own X-M5
frames approved as local-only training data), normal usage rules. Dell lane: **AI-POOL** (AI-PLAN: question pools ×10,
held-out splits). The ship (§1a.79) is done: H group published (fork-h264-3), `ai-3` tagged and built, **on publish hold**
(`.fork-publish-hold`, §1a.80). Live fixes after the pause: builds ai-3b … ai-3e (`latest` → ai-3e), commits 673e9da2 …
8157bc68: models follow `--data-dir` (`autosave::data_dir`), colour-direction filter and clear-winner rule in
`coach_assist.rs`, guide recipes 2b / 2c / 3b and "Where the Assistant runs"; GPU eval 85 / 87, 0 invented (synthetic).
**AI4.1a done** (`coach.assist` `context: "auto"`: selection + current node's measured facts, sent by the panel and the Ask box). **AI4.1b done** (`filmcraft_render::picture_facts`: exposure / clipped / crushed / contrast, near-neutral cast in words + McCamy K, saturation, skin-tone colours vs the skin line; `grade_tools::current_picture_facts` before / after the grade, in `context: "auto"` as "Picture (measured)"; whole on-screen context now capped at 3 000 chars). **AI4.1c done** (sheet names the source colour space: log / HDR measured after the input transform, or display-referred; per-frame cache `AssistJobs::picture_cache` keyed on clip, frame, the clip's state, media and pipeline). **AI4.1d done** (answers carry `seen`: measured facts that fit the question, shown as "On this frame (measured)" in the Ask box and the Assistant panel; FilmCraft writes them, not the model: Q40 (a)). **~~Known failing test~~ fixed 2026-10-15 19:43:** `coach_ui::coach_ask_box_shows_the_assistant_answer_or_why_not` was a test timing bug, not the clear-winner rule: after `coach.exit` the Ask box lays out again and the click on New topic hit the old place, so the skin question led the "NONE" ask and the clear-winner rule rightly gave its recipe. The test now waits 3 frames and asserts New topic forgot it; `answer_picks` unchanged (coach_ui 21 / 21). **AI4.1e done** (`docs/fork/coach-eval/picture.toml`: 11 generated-frame cases (dark, clipped, warm, cool, green, skin red / yellow, muted, log, general, off-topic), `picture_fixtures_name_the_problem`: every answer's measured lines name the problem; a strong cast now falls back to a wider near-neutral threshold, `NEUTRAL_CHROMA_WIDE`). **Item 1 of AI4 is done except the GPU checks (STEADY+): the 85-case eval doesn't regress with `context: "auto"` and the pool's `needs_clip` questions on fixture clips.** **Exact next AI4 step: item 2** (Qwen3-VL 4B: **licence checked 2026-10-15: `apache-2.0` in the model card metadata, no other licence file; pinned `Qwen/Qwen3-VL-4B-Instruct` commit `ebb281ec70b05090aa6165b016eac8ec08e71b17` (BF16 safetensors in 2 parts + tokenizer / preprocessor configs)**; was: check its licence is Apache-2.0 first, then download; design the SigLIP-style encoder on wgpu, a Fable / Opus design task: wait for STEADY). Older note, kept: the accept fixtures were: `needs_clip` test frames with known problems (dark, clipped, warm cast, off-line skin) and eval cases that expect a picture fact in the answer.
**Owner ~23:45 (§1a.92): AI5 gains item 8**, a fork-only local "training inbox" for the owner's own Assistant conversations (built with AI5, not before; the runner imports it each run once it exists).
**Owner ~22:55 (§1a.91): the Dell lane builds the creative set (CREATIVE-POOL, its SKILL.md §8, marker `.dell-creative-complete`)** while the main runner continues AI4: the lane owns `pool/creative-*`, `pool/creative-gaps.md`, `ai_pool.rs` and the daily sample (family field + check first, held-out batches first, ~300 before training ones); **the main runner no longer writes creative batches** and keeps retrieval (`sub_goals` / `TECHNIQUES`), the creative scorer, guide / practice notes (watch the lane's "practice notes wanted" list here) and GPU work. CREATIVE-POOL (Dell lane, worktree `.claude/worktrees/dell-lane`, branch `dell-lane`): **CREATIVE-POOL done (2026-10-16 ~02:20 EDT, short of target, stated; marker `.dell-creative-complete`): 608 held-out questions in 200 families, 412 training pairs in 138 families (targets ~850 / ~800); admitted-gap or partial-gap families 45 / 338 (13 %; 31 / 200 held-out, 14 / 138 training, all in `creative-gaps.md`); recipe spot checks 24 / 412 (about 1 in 17; all work as written after the fixes logged below); creative coverage@6 555 / 1550; `ai_pool` 11 / 11 and `coach_assist` 30 green on the Dell. The honest supply ran out: the last run found only 2 new families (horizon straightening, TV switch-off, opened by the new Motion lines; `creative-42.jsonl`), fewer than the 3 the supply rule asks for; what the guide still leaves uncited is workflow / admin. More families need new guide content first: ~~a GIF export line~~ ✓ 2026-10-16 02:22 EDT (`export.gif`: preset Animated GIF 640×360, In to Out, silent loop, CLI line; opens the reaction GIF family), ~~Anchor Point usage~~ ✓ 2026-10-16 02:24 EDT (`effects.shape-move` Motion item 3: Anchor Point is the pivot in the clip's own pixels, move Position by the same amount; the section header is at the 600-character cap, so its `when:` could not grow), date / time-of-day burn-in, word-by-word caption pop (gap).** Everything merged, no Dell job running. Done: pool-1 (`family` field + checks), held-out `creative-2…11, 14, 16, 18, 22, 24, 26, 28, 30, 32, 34, 36, 38.toml`, training `creative-12, 13, 15, 17, 19, 20, 21, 23, 25, 27, 29, 31, 33, 35, 37, 39, 40, 41.jsonl`, pool-17 / 21 (answers and sources follow the effect catalogue and the effects sections). Totals: **608 held-out in 200 families, 406 training pairs in 136 families** (targets ~850 / ~800); gap or partial-gap families 31 / 200 held-out, 14 / 136 training (creative-gaps.md); recipe spot checks 22 / 406 (about 1 in 18; 20 worked as written; spot check 21: a submix with a `studio_reverb` insert and a send from A1 adds the reverb tail (−91 → −49 dB after the tone); spot check 22: Explain grade gives one sentence per node and calls a disabled node "disabled, no visible change"; spot check 19: two looks as Gallery stills, `stills.apply` of the warm one renders identical to the warm grade; spot check 20: add edit + Ripple Trim Previous Edit to Playhead removes the span and closes the gap; spot check 18: a caption colour per imported SRT track works, a new caption track becomes C1; spot checks 10–17: progress bar, sketch reveal, colour bloom-in, logo shine through Track Matte, Crop aspect reveal, Speed / Duration 400 % and 12 000 %, Hold flash all fine; a Dip to Black on a sequence's first clip holds black for its first half and fades up in the second, now said in the recipe; Track Matte needed the matte-track step, now in the answers; the first found the frame-hold keyframe bug, fixed in 9fe832e7); creative coverage@6 555 / 1550 (floor 555); lookup floors unchanged (recall@5 827 / 846, coach 128, picture 8 / 10). **Owner read ready:** `pool/CREATIVE-READ.md` (40 random pairs), OWNER-QUESTIONS Q41 (FYI). **Supply note (2026-10-16 run):** pool-42…45 used up nearly every guide section no creative family had cited yet (postcard sky / land, phone-speaker mix, Scene Edit Detection recut, vertical CJK titles, Luma vs Sat, dynamic trim, F-Log2 normalising and exposing, Convert to Comp Clip, KDE Connect send, Whisper model choice, parallel-node gap; training: tracked face window, Gallery stills as client looks, Search Bins, grey card, vlog ripple trims); what's left uncited is workflow / admin (backups, naming, render cache, proxies, shortcuts) with no honest creative goal. Further families must reuse already-cited sections for genuinely new goals, so expect 2–4 families per batch; held-out is near its honest end (~620–650), training can still grow slowly. If a run can't find 3 honest new families, write "CREATIVE-POOL done" with the counts (short of target, as AI-POOL did); also unused: a reaction GIF (FilmCraft exports GIF but the guide doesn't say so: guide detail wanted), unused: 8 mm home movie (careful: near `vintage-film-look`), TV switch-off and straightening a crooked horizon (both need a guide line on Motion's Rotation / Uniform Scale / Scale Height first: guide detail wanted), page-peel / 3D spin transitions (~~only `effect-reference` names them~~ ✓ 2026-10-16 01:38 EDT: `effects.transitions` names them now), handwriting write-on (✓ 2026-10-16 01:40 EDT: not a gap: Write-on with Reveal Original Image, `practice.titles.write-on`). Spot checks reuse `cr-pb.fcproj` / `cr-sk.fcproj` / `cr-ls1.fcproj` in the Dell's fixtures dir; `timeline.place` (item, track `new`) puts a copy of the media on a new track, `edit.paste` lands on V1. Spot-check how-to: `target/debug/filmcraft-cli` in `~/Projects/filmcraft-linux` on the Dell (`cargo build -p filmcraft-cli`), fixture `target/fixtures/cr-spot.mp4` (ffmpeg testsrc2), `import` → `exec file.newSequenceFromClip` → `run <script.jsonl>` → `render --seconds S --out f.png`, measure with ffmpeg rawvideo. Workflow rule: fetch + rebase **before** the Dell test, push right after; re-test if upstream moved with guide / coach changes. **~~Guide detail wanted (from spot check 16, 2026-10-16 01:05)~~ ✓ (2026-10-16 01:33 EDT: `effects.transitions` Dip at the first / last edit + Opacity fade; `effects.shape-move` Motion ▸ Rotation / Uniform Scale / Scale + **Scale Width** (FilmCraft has no "Scale Height": with Uniform Scale off, Scale is the height) for horizon straightening and the TV switch-off squeeze; creative floor 555 → 556). **FOR THE DELL LANE: `coach_assist::tests::gap_detection_on_the_pool` was red after creative-32 / 36** (6 false gap notes, max 3; the lane's runs don't run engine lib tests: please add `-p filmcraft-engine --lib coach_assist` to the lane's checks). Max set to 6 for now; `pool-cr32-western-overlay` and `pool-cr36-satluma-resolve` get a correct gap note (layer mixer / node hue curves are not yet built): make their plans admit it (a `must` with the gap wording), then lower `POOL_GAP_FALSE_MAX`; `pool-cr32-chase` is a word-overlap false positive.** `effects.transitions` could say that a Dip to Black / White on a sequence's first (or last) edit holds the colour for half its length and fades over the other half (measured: a 2 s Dip at the start is black until 1 s, then fades up), and that Opacity keyframes give a fade over the whole length; also a line on **Motion ▸ Rotation, Uniform Scale and Scale Height** (straightening a horizon, a TV switch-off squeeze) would open two creative families. **~~Guide detail wanted (from spot check 9, 2026-10-16 00:30)~~ ✓ (2026-10-16 00:35 EDT, effects.shape-move: hide the matte track, not the clip):** `effects.shape-move`'s Track Matte Key row doesn't say the matte track must be hidden: in FilmCraft the matte works with the matte **track's** output off (`timeline.setTrack` `enabled: false`, Premiere's eye toggle) but a disabled matte **clip** (`clip.enable`) stops being a matte and the result goes black. A sentence there would let the assistant give the working step. **Creative coverage floor and guide edits (for the main runner, 2026-10-16 00:05):** `CREATIVE_COVERAGE_FLOOR` in `ai_pool.rs` equals the measured count, so a guide section added or reworded can drop it by 1–5 and turn `creative_retrieval_covers_the_plan` red (50407a37 → 380, b87ca4df → 379; the lane lowered it each time). When a guide change of yours moves it, please set it to the new measured count in the same commit, with the reason in its comment. **Practice notes wanted (for the main runner, 2026-10-15 23:58):** since 4a8329ee keeps `effect-reference` out of retrieval, creative answers that need an effect only that page names can't be grounded by the assistant. Held-out `creative-16` and the pool-17 edits cite it for: **Camera Shake / Wiggle**, **Auto Reframe**, **3D Rotate**, **Metadata & Timecode Burn-in**, **Lens Flare**, **Volumetric Rays**, **Mirror**, **Magnify**, **Lens Distortion**, **Track Matte Key**, **Stroke** + **Drop Shadow**, **Rounded Crop**, **Bokeh / Focus Blur**, **Brush Strokes / Find Edges / Posterize**, **Horizontal Flip**. Short practice notes (or `effects.stylize`-style sections) for these would let retrieval find them. **Guide contradictions found (2026-10-15 23:56):** the effect catalogue (and the new `effect-reference` page) has **Camera Shake** and **Wiggle** (Transform), **Auto Reframe** (Transform, Target Aspect 9:16 / 1:1 / 4:5, follows the subject; `auto_reframe_follows_the_subject` test), **3D Rotate** (X / Y / Z Rotation, Perspective, Z Position) and **Metadata & Timecode Burn-in** (a running timecode / frame count), all rendered (`crates/render/src/vfx`). Practice notes still say otherwise: `practice.titles.shake` ("no built-in shake or wiggle effect"), `practice.delivery.vertical` ("Automatic reframing isn't offered"), `practice.looks.vhs` (no running clock: Timecode Burn-in gives a ticking counter, not wall-clock time), `compositor.basics` "no 3D" (true for comps; 3D Rotate gives flat-layer perspective). The lane's creative answers now follow the catalogue (pool-17). ~~Frame-hold keyframe bug (found by the recipe spot check)~~ ✓ fixed by the main runner in 9fe832e7 (effect_time_at + regression test). **Practice notes / guide fixes wanted (for the main runner):** ~~(1) stylised effects / transitions in the guide~~ ✓ (`effects.stylize`, 2026-10-15); ~~(2) speed ramp~~ ✓, ~~stabilizing~~ ✓, ~~flicker~~ ✓ (practice/editing.md); still open: day-for-night variants (5c's list).
**FOR THE DELL LANE (2026-10-16 01:06 EDT): commit `c151b959` (pool-42) also reverted unrelated main-runner work** (`crates/llm/src/forward.rs` / `gpu_forward.rs` Qwen3 support, one FORK-PLAN §7 line, one HANDOFF line), apparently stale copies in the Dell worktree committed with `git add -A`. Restored in the merge. Please stage only the lane's own files (`docs/fork/coach-eval/pool/*`, `crates/engine/tests/ai_pool.rs`, guide pages you edit) and check `git diff --cached --stat` before each commit.
**Frame-hold keyframes fixed ✓ (2026-10-16 00:08 EDT):** the lane's report (keyframes on a hold collapsed) fixed in `commands.rs` (`effect_time_at` everywhere a keyframe is placed or toggled), regression test in `clip_ops_tests.rs`.
**Effect reference ✓ (2026-10-15 23:50 EDT):** `docs/guide/effect-reference.md` names every effect / transition by folder (test-synced with the catalogue). **Lane: check it before listing any "no such effect" gap.**
**Effect reference ✓ (2026-10-15 23:50 EDT):** `docs/guide/effect-reference.md` names every effect / transition by folder (test-synced with the catalogue). **Lane: check it before listing any "no such effect" gap.** **Re-source plans that cite `effect-reference.*` to the new sections `effects.lens-light`, `effects.shape-move`, `effects.painterly`, `effects.stylize` (Camera Shake / Auto Reframe / glows / Posterize Time …), `practice.looks.vhs` (Timecode Burn-in).** **Don't cite `effect-reference.*` as a plan source** (not retrievable; cite `effects.stylize` / `effects.transitions` / a practice note). New practice notes for plans: `practice.editing.timelapse`, `practice.titles.overlays`, `practice.editing.split-screen`, `practice.titles.behind`, `practice.editing.speed-ramp`, `.stabilize`, `.flicker`.
**Effect reference ✓ (2026-10-15 23:50 EDT):** `docs/guide/effect-reference.md` names every effect / transition by folder (test-synced with the catalogue; not in the assistant's retrieval index: it cost creative coverage). **Lane: check it before listing any "no such effect" gap.** Planned (needs the GPU eval, STEADY+): a pickable "effects named in the question" step in the pick prompt (names + folders from the catalogue, no section slots); measure on the daily / creative sets before keeping it.
**FOR THE DELL LANE (2026-10-15 23:44 EDT): two gaps in the creative set are wrong.** **Camera Shake** (Video Effects ▸ Transform: Position / Rotation / Zoom Amount, Frequency, Complexity, Motion Blur, Seed) and **Auto Reframe** (Video Effects ▸ Transform: Motion Preset, Target Aspect 9:16 / 1:1 / 4:5, Reframe Offset / Zoom) exist and render. Please fix `pool/creative-gaps.md` (rows "Built-in shake / wiggle effect" and "Automatic reframing for vertical") and any `must` that says there's no built-in shake or no automatic reframing (e.g. `pool-cr1-earthquake`; the `vertical-reframe-follow` and `camera-shake-impact` families), and check the effect catalog (`crates/project/src/effect/vfx.rs`, `effect.rs`) before listing a gap. Also: the "Security-camera running clock" row: **Metadata & Timecode Burn-in** (Video Effects ▸ Video) shows a ticking Sequence / Media Timecode in a corner; only a date / time-of-day clock is missing (guide `practice.looks.vhs` updated). "Word-by-word caption pop" stays a gap (Text Animator / Typewriter are grid transitions). The guide is fixed (`practice.titles.shake`, `practice.delivery.vertical`, `effects.stylize`).
**Sub-goal split off ✓ (2026-10-15 23:42 EDT):** measured on 646 creative plan sections: the split cost coverage (305 vs 310 without; `SPLIT_SUB_GOALS = false`), the `TECHNIQUES` lookup on the whole question still pays (+12 vs none). Retrieval ideas from now on are measured on all creative batches, never one.
**Creative retrieval by guide wording ✓ (2026-10-15 23:33 EDT):** coverage@6 162 → 169 / 339 via `when:` wording of building-block sections (every batch gains); extra `TECHNIQUES` rows rejected (no gain on unseen batches). For the lane: the creative floor can go to 169.
**Lane's guide fixes (1) ✓ (2026-10-15 23:24 EDT):** `effects.stylize` + transitions list name Posterize Time, Wonder / Edge / Echo Glow, Light Leaks, Turbulent Displace, Echo, Leave Color, Whip, Push, Slide, Earthquake, Glitch, VHS Damage, Cross Zoom, Light Leak (all render); VHS / glow / shake notes corrected. (2) speed ramps (`practice.editing.speed-ramp`; smooth Time Remapping ramps are a gap: keyframes not rendered), (4) `practice.editing.stabilize`, (5) `practice.editing.flicker` ✓ 2026-10-15 23:27 EDT. (3) day-for-night variants ✓ 2026-10-15 23:29 EDT: the lane's list (1)–(5) is done.
**AI4.5c.3 creative scorer ✓ (2026-10-15 23:20 EDT):** creative answers pass only as plans (≥ 2 plan sections cited, gap said where the plan has one); `ASSIST_EVAL_SET=creative` → `target/ai4-creative-eval.json`, summary `creativeCases / creativePass`. Model run owed (GPU, STEADY+). Keyword coach pool 128 / 188 (floor 128) after wording fixes in nodes / curves / coach / colour-management / lumetri.
**AI4.5a lookup-miss passes ✓ (2026-10-15 23:10 EDT):** pool recall@5 792 → 827 / 846 by guide body wording (sections that already answered, now in the asker's words) and 6 label fixes where retrieval's first pick answers the `must` (FORK-PLAN §7 entries 22:5x–23:xx). Left: 7 picture questions (sheet-dependent; picture-aware retrieval) and ~12 where the needed words pull other questions. Model re-run owed (GPU, STEADY+).
**AI4.5c.1 first creative batch ✓ (2026-10-15 22:35 EDT):** `pool/creative-1.toml` (8 held-out, kind `creative`), scored by `creative_retrieval_covers_the_plan` (coverage@8 6 / 21, floor 6). **5c.2 per-sub-goal retrieval ✓ (2026-10-15 22:40 EDT):** `coach_assist::sub_goals` + `TECHNIQUES` in `retrieve_with_picture`, creative coverage@6 6 → 8 / 21 (floor 8), lookup −1 of 791. **Next small CPU steps:** ~~guide section on a clip's Opacity + blend modes~~ ✓ 2026-10-15 22:42 EDT (`effects.opacity`, creative coverage 9 / 22); ~~practice notes shake / clean plate~~ ✓ 2026-10-15 22:46 EDT (`practice.titles.shake`, `practice.editing.clean-plate`; coverage 11 / 24); ~~notes for double exposure, VHS / security-cam looks~~ ✓ 2026-10-15 22:50 EDT (`practice.looks.double-exposure`, `practice.looks.vhs`); still open for 5c: day-for-night variants and further notes as the creative batches need them; ~~more `creative-*` batches + training pairs~~ → Dell lane CREATIVE-POOL (§1a.91).
**Owner ~22:30 / 22:40 (§1a.90): open-ended creative questions** the guide doesn't answer directly ("remove the person and replace them with a translucent ghost figure"): AI-PLAN AI4 item 5c: `creative-*` AI-POOL batches (multi-section recipe answers, gaps admitted with workarounds, 0 invented), per-sub-goal retrieval, creative scored apart from lookup questions; practice notes for techniques recipes keep needing. CPU-only apart from the model eval: good MINIMAL / LEAN work.
**AI4.5a picture-aware retrieval ✓ (2026-10-15 21:54 EDT):** `coach_assist::retrieve_with_picture` (used by both prompt builders): when the question is about the frame (`asks_about_the_frame`: "this", "shot", "looks", "it's", … or a `picture_note` topic) and the measured sheet names clear problems (`picture_terms`: clipped / crushed / under- or overexposed / flat / cast / tint / saturation / skin off the line, worst 3), the plain retrieval's best 3 stay first and the rest come from question + those words. AI-POOL picture questions top-5 **3 → 8 / 10** (floor `PICTURE_RETRIEVAL_FLOOR` 8); with each picture case's unrelated sheet attached the other pool questions keep 766–771 of 781 (test bound: ≤ 3 % lost). Always appending the words was measured and rejected (others fell to 107–760). Provisional; model eval of it owed (GPU, STEADY+).
**AI4.5a daily set ✓ (2026-10-15 21:35 EDT):** `ASSIST_EVAL_SET=daily` runs the real-model eval on `pool/daily-sample.toml`'s 147 pool ids (owner cases stay with the keyword-coach eval), report `target/ai4-daily-eval.json`; test `the_daily_sample_resolves_to_pool_questions`. Use it for the item-5 daily check (GPU, STEADY+).
**AI4.5a fourth header pass ✓ (2026-10-15 21:19 EDT):** practice/mixed-cameras headers, then practice editing / titles / workflow; pool recall@5 776 / 834, 778 / 839 with AI-POOL.36 merged (floor 778). Remaining misses are mostly colour-page (c1–c8) and the picture questions (bare-question retrieval, for AI4 with the fact sheet).
**Owner ~21:30 (§1a.89):** AI4 item 2b added: question-aware looking (the vision model gets the question with the frame). Claude inside
the Assistant panel: not planned, don't build it.
**Owner ~20:45 (§1a.87):** AI-POOL adds three new pair kinds after the practice pages (multi-section tasks, follow-up chains,
picture-dependent pairs on AI4's fact sheet), each with a held-out slice; see AI-PLAN AI-POOL.
**Owner ~18:50 (§1a.86):** AI4 also trains and tests on AI-POOL's pools once the Dell lane finishes them (AI-PLAN AI4 item 5;
the owner reviews a sample of pairs before the tune).
**AI4.5a ✓ (2026-10-15 20:18 EDT): pool eval runner.** `real_assistant_eval_on_the_advanced_set` with `ASSIST_EVAL_SET=pool ASSIST_EVAL_SAMPLE=150` reads every `pool/*.toml` held-out question (sorted, evenly spaced sample; full set without `ASSIST_EVAL_SAMPLE`), writes `target/ai4-pool-eval.json`. **Baseline (SmolLM3-3B Q4_K_M, RX 6700 XT, pick style, no picture facts, synthetic): 103 / 150 auto-pass (68.7 %), 0 invented, 144 model-answered, 111 cite a source; first words median 0.96 s, total median 2.43 s.** Misses: 39 'no expected source cited' (mostly cited a neighbouring section), 9 gap cases without the gap note, 6 'doesn't know'. **Gap misses looked at (2026-10-15 20:23 EDT):** 8 are genuine (c3-convert answered "No conversion" correctly; the scorer only takes 'not yet' wording). Retrieval puts the right section first in 7 / 8; the gap note doesn't fire because those pages state the limit in passing, not as a marked "FilmCraft: not yet" line. **Tried and reverted:** a fallback that takes any "not … yet" sentence of the top section sharing ≥ 2 words with the question: fixes Cut page / pinch / K+L but adds 5 false gaps on advanced.toml ("no button yet", "no menu item yet", "can't carry over yet", qualifier Differences, skin-soften's "no beauty tool yet"; floor GAP_FALSE_MAX 1). Better route if retried: mark real gaps guide-side (a "**FilmCraft:** not yet …" line in its own section or a `gap:` header field), not sentence heuristics. **Gaps marked guide-side ✓ (2026-10-15 20:48 EDT):** a section's gap line ("- **FilmCraft:** no … yet") now fires only when the question shares its own clause's words (`gap_matches`: the clause up to the first `:` / `;` / `.`, deduplicated, at least 2 words or a third of a long clause; a bare "not yet (planned …)" names nothing and still fires), so gaps can sit in general sections; 11 sections now mark theirs (Edge Thin, pinch on Wayland, GSConnect, Lumetri ↔ nodes, Cut page, K+J / K+L, still rename, memories / versions, comp settings dialog, qualifier subtract / 3D, planar tracker, translated lessons). CPU-only test `gap_detection_on_the_pool`: **11 / 38 (was 2), false 5 (was 8; the left ones are practice notes whose "not yet" is part of the answer)**; advanced 7 / 7, false 0 (was 1). Not marked: limits without a "yet" (`.mogrt`, DCTL, MHL, .drp, cloud library, Fusion 3D: by design, not planned) and sections where any short clause would also match ordinary questions (starter tree, curves). Not re-run on the model (GPU eval when STEADY). **Source-citation misses by batch ✓ (2026-10-15 20:54 EDT):** of the 39, e2 / m1 / c3 had most, and nearly all were retrieval misses (expected section not in the top 5), not model picks; 12 feature sections got question-shaped `when:` wording (edit-menu clipboard / select / cleanup / sequence-settings / helpers, file-menu projects / autosaves, media generated / clips / scene-detect, projects where / opening): pool recall@5 669 → 682 / 752 (13 fixed, 0 lost); with practice-7 merged 701 / 771 (floor 701), coach 76 / 76. Not re-run on the model (GPU eval when STEADY). **Second pass ✓ (2026-10-15 20:56 EDT):** 13 more headers (essential-sound types / controls, mixing submixes, editing cutting, timeline in-out, effects apply, graphics layers, compositor make, proxies progress, getting-started install / workflow, export hevc / presets): pool recall@5 701 → 711 / 771 (10 fixed, 0 lost; floor 711). Left on purpose: questions the guide doesn't answer in its body (title font-size shortcut, .mogrt import, export for a client's Mac, cache-everything), and the c3 colour-practice misses (8; header words there risk pulling ordinary colour questions: check advanced recall when trying). **Third pass, colour pages ✓ (2026-10-15 21:05 EDT):** 7 colour headers (readme idea / layout / first-grade, primaries panel / white-balance, secondaries windows, lumetri order), each checked against its body: pool recall@5 711 → 715 / 771 before practice-8, 728 / 787 with it (floor 728; flat-why, undo-drag, wb-auto, bypass-lumetri fixed, 0 lost), advanced 87 / 87, coach 76 / 76. Still missing: ~~node-limit~~ ✓ (header wording, 786 / 846), ~~grade-per-clip~~ ✓ (787 / 846), ~~shader-off, window-position, coach-progress, remove-unused, apple-hvc1~~ ✓ 22:19–22:25 (792 / 846; floor 792), bw / window-face / split-adjacent (header wording alone didn't lift them). This is the item-5 daily check; run it after each AI4 step.
**AI-POOL done (2026-10-15 21:28 EDT, Dell lane; marker `.dell-pool-complete`).** Held-out test set in `docs/fork/coach-eval/pool/`: **846 advanced** (incl. 13 task, 4 chain, 10 picture) + **188 keyword-coach** questions in 37 batches, on top of the original 30 / advanced sets; training: **811 pool pairs** (570 feature, 100 mapping, 57 practice, 28 task, 20 picture, 13 chain, 13 follow-up, 10 gap) + **967 base pairs** (`training-pairs.jsonl`, regenerated); daily sample `pool/daily-sample.toml` (3 owner-verbatim + 147 pool ids, every batch); splits checked by `crates/engine/tests/ai_pool.rs` (6 tests: well-formed / grounded, new-kind shapes incl. the fact-sheet wording against `PictureFacts::sheet`, no test question or near-copy in any training file, coach floor 125, retrieval@5 floor 785 / 846, the daily sample). **Limit, stated:** the ~9 000-pair target isn't honestly reachable from this guide: almost every section already has 2–3 held-out questions, so most further pairs reword one; the §1a.87 kinds help only a little (kept after the overlap / intent check: tasks 28 of 36 drafted, chains 13 of 16, pictures 20 of 26; a colour chains batch kept 2 of 12 and was not committed, audio tasks 3 of 7). More pairs need more guide (new sections) or owner-written questions. **Owed:** the per-batch fresh-context reviews from practice-4 on (and color-1 / color-2) were skipped (weekly usage over its line: no subagents); the owner's 30–50 pair read before any tune (Q35). History of the lane's work follows. *(was: in progress)* layout + checks merged (AI-POOL.1): batches in `docs/fork/coach-eval/pool/` (`<batch>.toml` held-out `[[question]]` advanced format, readable by `coach_assist::parse_eval_set`, + `[[coach]]` keyword-coach format; `<batch>.jsonl` training pairs; table in `pool/README.md`), checked by `crates/engine/tests/ai_pool.rs` (overlap check on content words, floors). Done: **color-1** (color/README, color-management, curves), **color-2** (primaries, secondaries), **color-3** (lumetri, nodes), **color-4** (matching), **color-5** (finishing), **color-6** (noise), **color-7** (recipes), **color-8** (coach): the colour area is done; **editing-1** (editing, timeline), **editing-2** (trimming, edit-menu, file-menu), **audio-1** (audio, essential-sound, mixing), **compositor-1** (compositor, keying, tracking), **effects-1** (effects, shader-effects, text), **graphics-1** (graphics, packs), **export-1** (export, export-queue), **media-1** (media, projects), **proxies-1** (proxies, render-cache), **hardware-1** (hardware, cards), **linux-1** (linux), **start-1** (getting-started, pages), **glossary-1**, **practice-1** (grading), **practice-2** (node-trees), **practice-3** (looks), **practice-4** (editing), **practice-5** (audio), **practice-6** (delivery), **practice-7** (youtube), **practice-8** (titles), **practice-9** (workflow), **practice-10** (mixed-cameras): every practice page has a batch; **tasks-1** (§1a.87 multi-section tasks), **chains-1** (follow-up chains), **pictures-1**, **pictures-2** (fact sheets), **tasks-2** (feature-page jobs). Totals 844 advanced, 188 coach, 808 pool pairs (+ 967 base pairs); coach 125 / 188, retrieval@5 782 / 843. **New kinds run out fast, stated:** about a third of drafted task / chain / picture pairs reword a held-out question (most sections already have 2–3 held-out questions), so each batch keeps ~6–13 pairs. *For AI4 (main runner):* picture questions retrieve badly on the bare question (pictures-1: 1 / 5 in the top 5; the sheet carries the information), so retrieving on facts + question may help (after the main runner's AI4.5a header passes). **Limit, stated (AI-POOL "say so in HANDOFF"):** the ~9 000-pair target isn't reachable honestly: once a section has 2–3 held-out questions, most further pairs about it paraphrase one, and the overlap rule (plus judgement for semantic near-copies the word check misses) leaves them out; expect ~1 000 pool pairs plus ~970 regenerated base pairs. The advanced target (~900) is reachable with the remaining practice pages. Every feature page except shortcuts / command-reference (reference lists; a few lookup questions at most) has a batch. **Next (§1a.87):** the new kinds are in the format and checks (AI-POOL.33: `task`, `chain` with `context`, `picture` with `facts` in `PictureFacts::sheet` wording; pool/README.md). More `tasks-*` / `chains-*` batches from the feature and colour pages (chains-1 lesson: drafted chains often reword a held-out question in a later turn; run the overlap script with chain turns before committing), and `pictures-*` (fact sheets → guide steps), each with a held-out slice. **Daily sample chosen (AI-POOL.38):** `pool/daily-sample.toml` = `owner` (the 3 owner-verbatim questions of questions.toml, keyword-coach format) + `ids` (147 held-out pool questions from all 36 batches, proportional by kind and style); rerun `tools/fork/ai-pool-daily-sample.py docs/fork/coach-eval 150` after new batches (the check needs every batch represented). *For AI4 (main runner):* `ASSIST_EVAL_SAMPLE` could read this file instead of the evenly spaced pick; `owner` cases need their `facts` used as `must`. Then: fresh-context reviews owed (no subagents this week so far), and "AI-POOL done" when more new-kind batches stop paying off. Lane tooling for the next run: per-batch generator scripts are in the session scratchpad only (the `.jsonl` files are the source of truth); gate = fmt + `--test ai_pool` + clippy -p filmcraft-engine + `--lib coach_` on the Dell. No `[[coach]]` questions for NOT_INDEXED pages from effects-1 on, then editing, audio, compositor, export, media, titles, projects, linux / hardware, practice. `training-pairs.jsonl` regenerated (AI-POOL.32): 967 pairs (560 feature, 259 mapping, 148 practice), 2 held out as near-copies; rerun the script after big guide changes. Owed: a fresh-context (Opus) review sample of color-1 / color-2 (skipped: weekly usage over its line, no subagents). For the main runner (guide / retrieval, not the lane's): keyword-coach misses in `ai_pool` output, e.g. "how do I set a clip to F-Log2", "how do I do my first grade", "how many points can a curve have", "lumetri masks" (not covered), "how do I make it black and white" (→ finishing scopes), "how do I turn off one node", "how do I copy lumetri to another clip", "which clips have the same grade" (→ finishing soft clip); ~~`glossary.render-cache` stale "planned for Phase 6"~~ ✓ 2026-10-15 20:49 (points to the render cache); the keyword coach answers almost none of the AI3-PREP feature pages' questions (`timeline.md`, `trimming.md`, `edit-menu.md`, `file-menu.md`: 0 / 13 in editing-2, e.g. "add a marker", "paste insert", "turn off snapping", "remove unused clips", "smart bins" not covered; retrieval finds them); by design: *(main runner 2026-10-15 19:50: indexing `timeline.md` for the keyword coach lifts the pool coach 70 → 74 but breaks the colour eval floor ("do I grade before or after I cut the edit" lands on `timeline.in-out`), and rewording that section breaks `adv-edit-j-cut` retrieval: reverted; these pages are NOT_INDEXED on purpose (AI3-PREP.2d), retrieval covers them)* ~~`color.readme.not-yet` stale~~ ✓ 19:29;

**Phase order (FORK-PLAN §1a.6, the only valid order):** … → Y1 ✓ → 6E ✓ → U0.2 ✓ → AI1 ✓ →
X422 ✓ → **Y3** → C1 → **AI3** ✓ (§1a.73–74) → **AI4** (§1a.83, assistant sees footage; next on resume) → **AI5** (§1a.84, opt-in example sharing via GitHub) → **S1** (moved here by §1a.93) → **REF** (§1a.93, "how do I get this look?" from a reference picture) → C2 → AI2 → **A3** (§1a.76) → S2 → G3 → **UPD** (§1a.77, update check) → **AI6** (§1a.88, hands-on assistant + MCP co-editor) → 7 (+ Y4) → Y2 → C3 → C4 → G4 →
R2 → D → 8 (R1 CinemaDNG moved to the maybe list, §1a.81). Small phases are accepted in groups (§1a.41). AI1 moved right after U0.2 and the
4:2:2 export cases (X422) right after AI1 by the owner 2026-10-13 (§1a.69). **A2 and then
H1 → H2 → H3 run on the Dell lane (owner 2026-10-14, §1a.72); the main runner skips them.**
**Owner, live 2026-10-14 ~12:55 (§1a.75):** AI3 trains on **all of editing**, the Compositor (C1) included, not only grading; only features built by the tune, later phases add pages / pairs / eval questions and the tune is refreshed. **Q30 (a) accepted** (A2's numbers stand). **Q31 (a) yes:** the Dell lane may download the ITU-T H.264.1 conformance streams (Dell only, never committed) for H1.5 PAFF and H2. Q29 ok.
**Owner, live 2026-10-15 ~00:35 (§1a.78), done (AI3.5f `8cedc686`, merged):** Lessons / Ask the coach / **Assistant** are pills in
the Resolve page bar left of Home (Premiere: the header's right side; `coach::bar_buttons`); the Ask box only opens from them. AI3's
chat is `PanelKind::Assistant` (`panels/assistant.rs`): the button opens it floating (left of the coach's card column), dockable like
any panel; chat turns in `UiState::assistant` (never saved), follow-ups from the last model answer, New chat, Stop, clickable lines →
spotlight, Show me; fallback shows the keyword coach's steps. Tests: `coach_ui::assistant_panel_floats_answers_docks_and_closes`,
`pages_ui` placement checks; guide color/coach.md "The Assistant panel". **Next for the chat (main runner):** it uses the same
`coach.assist` path, so GPU inference speeds both; ~~a Settings ▸ Color row to install the model from the panel when `notReady`~~ ✓ 2026-10-15 19:58: a notReady answer links to Settings ▸ Color (`assistant.setup.<n>`, test `coach_ui::assistant_without_a_model_links_to_its_settings`).
Known flake (not from this change, fails on the base too under load): `panels_ui::scopes_of_colour_bars` in a parallel run.
**A1 (Dell lane, §1a.60) accepted by the owner 2026-10-14 ~01:20 (§1a.71)**; its tag `audio-1` is on `f8c3e788` with `x422` (2026-10-14).
**L1 / L2 part 2 accepted by the owner (live, 2026-10-14 ~01:10; was ready 2026-10-13 22:55, Dell lane; scheduled task `filmcraft-dell-lane`,
lock `.dell-lane.lock`):** the last two steps merged: **L1.5** `d0c68898` (upstream #260 off-thread
file dialogs cherry-picked, Q23; dialogs parented to the window, each kind starts in its last
folder, close waits for an open dialog) and **L2.6** `7f6d79dc` (Settings ▸ Playback ▸ Graphics
device by PCI ids, VA-API tries the chosen GPU's render node first, `graphics.listDevices`).
Checklist on the Dell (CachyOS, UHD 770, 2026-10-13): fmt ✓, clippy ✓ (workspace minus
`filmcraft-speech`, see below), engine / app / ui-egui lib ✓, every `--test ui` module one per
process ✓ (86 / 86), `native_dialogs_ui` ✓, platform lib ✓, layers ✓, wasm ✓; both steps had a
fresh-context Opus review, findings fixed. Earlier part 2 items: Flatpak build ✓ (`18d523e4`,
feature-checked in a virtual KWin; SDK kept on the Dell, Q17), L1.8 / L2.4 / L2.10 and the
text-scale hint (`2e67f732`). **Tags `linux-1b` / `linux-2b` are on `f8c3e788` with `x422` (2026-10-14).**
**Owed at acceptance:** on the PC: Graphics device Auto vs a chosen GPU on the RX 6700 XT + UHD 750
(window, monitors and decode on the same GPU; `graphics.listDevices` `inUse`), Program monitor 1:1
at fractional scale; on a real desktop: a file dialog stacked over the window (KDE portal, Wayland)
and opening in the last folder, GlobalShortcuts dialog, card insert → Copy → Eject, KDE Connect send /
receive / Multimedia control, Plasma font 11 pt → 110 %, an export notification popping.
**Seen on the Dell, not the lane's (fixed since: both examples now have `required-features = ["whisper"]`, checked 2026-10-15):** `cargo clippy --workspace --all-targets` failed in
`crates/speech/examples/candle_ops_bench.rs` since `a2cd09d4` (AI1.2): the example uses candle without
`required-features = ["whisper"]` in `crates/speech/Cargo.toml`.
**A1 accepted by the owner (live, 2026-10-14 ~01:20, §1a.71; was ready 00:55, Dell lane; AUDIO-PLAN Phase A1, §1a.59–60).** Steps merged:
A1.1 `a7b753c2` (upstream #840 stream recovery, ported to cpal 0.18), A1.3a `39f4439a` (failed reopen
retried within the budget; restart keeps the running format), A1.2 / A1.2b (play-ahead: per-block
panic recovery, `mixerStopped` → status + wall clock in both monitors, adaptive lead ≥ 4 device
buffers +50 ms per underrun episode ≤ 0.5 s, checked mix access, start-failure status), A1.3b
`a9077873` (mix rebuilt at the rate the output opened), A1.4a `c0a81628` (clock counts frames heard:
cpal timestamps), A1.4b `cdd0dc69` (sync probe `tests/ui/av_sync_ui.rs`: 0 frames off after 100
seeks, at 1× and 2×), A1.5 `c008e455` (soak harness `tools/fork/a1-audio-soak.sh`, docs/testing.md
§6). **Acceptance checklist (Dell, CachyOS, PipeWire 1.6.9):** 30-minute soak, 20 tracks with
overlapping clips, 78 seeks, 48 2× shuttles: **0 underruns** ✓, **drift ≤ 1 frame** (worst 1, 87 383
readings) ✓; **simulated device loss** (null sink removed / restored, card profile off / on, both
automated) recovered in 0.50–0.56 s each without a restart ✓, profile restored ✓; regression tests
per report: #336 overlapping-clip cut-outs and #617 Linux stutter → soak + `play_ahead` stall /
lead tests; #602 no audio → start-failure status, rate agreement, #840 recovery tests; #605 (Xonar
DX output) → sample-format conversion and running-format restart tests (the card itself isn't
here); #410 ("buggy audio", no repro steps) → covered only by the soak. **Owed on the PC at
acceptance:** the soak once on the PC's own output (`tools/fork/a1-audio-soak.sh 300`; it uses a null
sink and the first card's profile), a listen at 1× / 2× for pitch after a default-sink change, and
the real output latency (`perf.stats` `playback.audio`). Tag `audio-1`: on `f8c3e788` with `x422`.
**A2 accepted by the owner (live, 2026-10-15 ~00:15, §1a.76; tag `audio-2` after the owed review below); its Voice Isolation quality work moves into A3.** Was: **A2 ready for acceptance (Dell lane, 2026-10-14 ~08:30; §1a.72; scheduled task `filmcraft-dell-lane`,
lock `.dell-lane.lock`; worktree `.claude/worktrees/dell-lane`, branch `dell-lane` = hevc-vaapi, nothing
unmerged; tag `audio-2` rides with the next phase group).** Audit `docs/fork/PREP-A2.md` (step map §5). Merged: A2.1 `26d2e7c5` (new L2 crate
`crates/voice`: mixed-radix FFT, DFN3 STFT / ERB / features / deep filter), A2.2 `93e0eb8b` (checked
checkpoint loader, feature `dfn`), A2.3 `e9e95f2e` (the network as plain-Rust streaming kernels +
`Denoiser`; parity with DeepFilterNet's own `deep-filter` 68.2 dB; real-time factor 0.071 on one
Dell core), A2.4 `e66e056d` (the `voice_isolate` effect: audio-dsp external-effect registry +
DeNoise fallback, `filmcraft_voice::effect::install`, project def, one render arm, guide
`docs/guide/audio.md`), A2.5a `12721bed` (`audio.voiceIsolate.status / download / cancelDownload /
installFile / remove`, pinned package, lazy loading; app + CLI feature `voice-isolation` on by
default; real download checked end to end on the Dell), A2.5b `28edd384` (Settings ▸ Audio ▸ Voice
isolation row, UI test in both workflows), A2.7 `3e635592` (test set + SI-SDR harness
`voice_isolation_eval`; **on synthetic formant speech the model fails the gate (2.1 dB on clean
speech): real speech needed; **Q26 answered (b) 2026-10-14 ~06:20: LibriSpeech test-clean approved**), A2.6 `3cb31fd0` (live in the mixer: one network step per track on the
channels' mean, 0.2 s pre-roll; Dell soak 300 s: 1 Voice Isolation track 0 underruns, 4 tracks 21
in the first 2× shuttle's first 0.2 s; no offline cache needed). DeepFilterNet3 weights (pinned commit 82b0c7ad, SHA-256 in PREP-A2 §3) are on the Dell
under `~/.local/share/filmcraft-fork/models/deepfilternet3/`; the oracle is
`~/fork-oracles/target/release/deep-filter` (built from source with `RUSTC_BOOTSTRAP=1`, `time`
pinned to 0.3.36). Dell tests: `FILMCRAFT_DFN3_CKPT=<ckpt> FILMCRAFT_DFN_ORACLE=<oracle> cargo test
-p filmcraft-voice --features dfn`. A2.8a `5be92eab` (Clean Up Voice `audio.cleanupVoice` + Essential Sound ▸ Repair ▸ Voice Isolation
slot + Loudness ▸ Limit True Peak / Ceiling; guide audio.md "Cleaning up a voice by hand" = the written
manual process; loudness half of the gate passes in tests: ±0.5 LU of −14 LUFS, ≤ −1 dBTP on the
rendered clip). A2.8b `66654f4d` (the gate on LibriSpeech, 50 utterances on the Dell under
`~/.local/share/filmcraft-fork/voice-test/librispeech/`: **one-click passes all 15 cases** against
the manual process; **Voice Isolation vs DeNoise misses two of its bars** (+0.8 dB at 10 dB SNR, bar
3; clean speech 17.0 dB, bar 20), OWNER-QUESTIONS Q30 FYI, choice (a) accept with the numbers).
A2.8c `f5eafc67` (Clean Up Voice as a background job: progress, stop, applied in one undo step on poll; `wait: true` for agents). A2.9 `f9f290ac` (per-take review: recordings tagged as takes of their record point,
`audio.voiceover.takes` / `pickTake`, Takes with waveforms in Essential Sound, processing moves
with the take). **Acceptance checklist (AUDIO-PLAN A2):** port = original 68.2 dB, real-time
factor 0.071 / channel on the Dell, live in the mixer (soak: 0 underruns with 1 track) ✓;
Voice Isolation vs DeNoise on LibriSpeech: wins all 15 cases, non-steady margin +7.1 / +4.0 dB
at 0 / 5 dB SNR ✓ but **+0.8 dB at 10 dB (bar 3) and clean speech 17.0 dB (bar 20) ✗**
(OWNER-QUESTIONS Q30, choice (a) accept; (b) re-measure on your own quiet-room recordings);
one-click gate vs the written manual process: **all 15 cases pass**, −14.0 … −14.2 LUFS,
−1.5 dBTP ✓ (no case limited to "Suggest settings"); commands `audio.voiceIsolate.*`,
`audio.cleanupVoice`, `audio.voiceover.takes` / `pickTake`, all on the control channel ✓; guide
`docs/guide/audio.md` written ✓. Limits: 44.1 kHz sequences use the DeNoise fallback (resampling
around the model is a later step); the one-click path without the model (Reduce Noise) wasn't
gated. Evals: `FILMCRAFT_EVAL_SPEECH=~/.local/share/filmcraft-fork/voice-test/librispeech
FILMCRAFT_DFN3_CKPT=~/.local/share/filmcraft-fork/models/deepfilternet3/ckpt/DeepFilterNet3/checkpoints/model_120.ckpt.best
cargo test --release -p filmcraft-engine --features voice-isolation <voice_isolation_eval|cleanup_gate>
-- --ignored --nocapture` (gate ~17 min on the Dell). Reviews were self-reviews (weekly usage
above pace, no subagents): **a fresh-context review of A2.1–A2.3 and A2.8–A2.9 is owed** before
the tag. **Owed on the PC at acceptance:** live voice isolation during playback on the i5-11600K.
**AI3-PREP done (Dell lane, 2026-10-14 ~13:45; §1a.74, task §5b; marker `.dell-ai3prep-complete`)**,
for the main runner's AI3: (1) **compact headers** on all 27+ guide pages (spec docs/guide/README.md
"Section headers"; parser `coach_ask::parse_header`, `Section::header`; check
`section_headers_are_valid_unique_and_point_at_real_things` covers every page but README.md: **every
new guide section needs a header**); (2) **feature pages**: text, mixing, graphics, timeline, effects,
media, shortcuts, edit-menu, trimming, export-queue, file-menu, essential-sound, plus getting-started
X-M5 import, voice-over recording, caption tracks (~580 of ~700 engine commands named by family);
(3) **150 practice notes** in docs/guide/practice/ (10 pages; each mapped to FilmCraft with gaps said as
"not yet"; own words, no source text, plan/ai3-sources.md is maintainer-local); (4) **advanced eval**
docs/fork/coach-eval/advanced.toml (63 questions with source header ids, must / must-not facts;
`advanced_eval_set_is_well_formed`) and **885 draft training pairs**
docs/fork/coach-eval/training-pairs.jsonl (generator tools/fork/ai3-training-pairs.py, TRAINING-PAIRS.md,
`training_pairs_are_grounded`; needs a human review sample before tuning). **Keyword-index note:**
the 116-question colour eval sits at its floor (82), so the general and practice pages are in
`coach_ask::NOT_INDEXED`; AI3's retrieval should index them (and the headers). No model downloads
were made. **H1 in progress on the Dell lane** (task §6, H264-PLAN; same worktree / branch `dell-lane`):
audit **docs/fork/PREP-H1.md** (H1.0 `b731932c`, step map §4); **H1.1** `ec7ce8ed` (`Sps::check_coding_tools`
for hardware fronts, `check_supported` still refuses interlaced for software) and **H1.2** (field-aware DPB in
`crates/h264/src/dpb.rs`: H1.2a `7158790c` per-field marks / POCs, `Dpb::store` with `FieldPocs`,
`is_second_field` pairing (ask it before `idr()` / gap fill), field sliding window and MMCOs; H1.2b `c61fcb00`
`build_field_ref_lists` (to_ref gets entry + `Parity`); H1.2c `e9294236` `Poc::picture`) merged, self-reviewed
(weekly usage above pace). **H1 ready for acceptance (Dell lane, 2026-10-14 ~18:50; tag `h264-1` rides with the main runner's next phase
group).** Merged: H1.3 `b728822d` (VA-API front takes PAFF and MBAFF: `vaapi/h264.rs` field flags / per-field order
counts / second field into its first field's surface / field reference lists, `geometry()` takes interlaced H.264),
H1.4 `50f2a402` (`tests/vaapi.rs::interlaced_h264_matches_ffmpeg`, libx264 MBAFF fixtures), H1.5 `995b48ab`
(`interlaced_conformance_streams_match_reference`, ignored; ITU-T set via `FILMCRAFT_H264_CONFORMANCE_DIR`), H1.5b
`81addcfd` (`interlaced_1080i_is_bit_exact_and_faster_than_real_time`, ignored). **Review:** fresh-context Opus review
done in this run's own session (it had never seen the diff; no subagent, weekly usage above pace, Q33 note); no
new `unsafe` (ffi.rs gains two flag constants only), so no Fable review needed. No blocking findings; non-blocking: an
unpaired field (its partner lost) leaves the other field's lines from the surface's previous picture on hardware (the
software path copies the field; never a crash). **Checklist on the Dell (UHD 770, Mesa iHD):** fmt ✓, clippy
platform + h264 ✓, platform tests ✓ (24 + 1080i), layers ✓; **ITU-T interlaced set 65 bit-exact (PAFF, MBAFF, mixed;
CAVLC / CABAC; High 8x8), 3 declined (MR6/7/8_BT_B: Extended profile), 0 failed; 1080i29.97 MBAFF (tff, B-pyramid, 4
refs) bit-exact vs ffmpeg, 549.8 fps decode (18× real time).** Finding for acceptance: the hybrid decoder starts
streams that open on a non-IDR I picture in software (24 of the conformance set); with H2 merged the software path
decodes them too (bit-exact), but such files (broadcast captures cut mid-GOP) then decode on the CPU until the hybrid
moves to hardware. ~~AMD VCN interlaced~~ ✓ 2026-10-14 18:40 (RX 6700 XT, `FILMCRAFT_VAAPI_DEVICE=/dev/dri/renderD129`,
release: `interlaced_h264_matches_ffmpeg` (3 MBAFF cases) + 1080i29.97 MBAFF bit-exact vs ffmpeg at 378 fps, synthetic;
the 32 GB conformance set stays on the Dell). **Still owed on the PC at acceptance:** the in-app zero-drop playback of a 1080i file on both GPUs.
**H2 started in parallel** (sequencing choice logged here): audit
**docs/fork/PREP-H2.md** (H2.0 `e29d8814`: parsing and the field DPB are done, frame threading exists, everything
below the slice header is 8-bit 4:2:0 frame coding; step map H2.1–H2.8; conformance vectors ride on **Q31** a).
**H2.1 (sample-type groundwork) done** (2026-10-14 ~11:45, self-reviewed: weekly usage above pace, PREP-H2 §4 allows
it for H2.1): `crates/h264/src/sample.rs` (`Sample` for `u8` / `u16`, `clip1`, `chroma_row`, `code_max`: constant
255 for `u8` so 8-bit kernels clip at compile time); H2.1a `1e7c0d65` inter, H2.1b `a0ae378c` intra, H2.1c
`9ceb574c` residual kernels generic; H2.1d `833feb88` storage (`Planes` / `FrameRow` / `Frame` over `Sample` with
`bit_depth`, windows / cropping for `u16`) and deblocking (alpha / beta / tC0 × (1 << (BitDepth − 8)), clip to max,
packed `u8` vertical edges kept, plain `u16` lanes; 16-bit lanes safe to 12 bits); H2.1e `3f744cc9` slice decoding
(`PicState` / `Scratch` / `SliceDecoder` / `RefPic` generic); H2.1f `cf22ac31` decoder dispatch: a private
`Core<S>` per sample type behind `Decoder`; a slice whose SPS needs > 8 bits switches to the `u16` core (old core's
pictures flushed first; parameter sets, settings, error slot and stats carry over). Hidden test hook
`Decoder::set_wide_samples` runs **every conformance fixture (32 on the Dell) and the synth streams through the
`u16` path too: bit-exact vs ffmpeg**, plus `sample_path_switch_at_idr`. **8-bit speed unchanged** (perf_1080p
1 thread 99.5–100.7 fps vs 99.8–100.2 before, release, Dell; edge filters same speed / checksum). > 8-bit streams
are still refused (`check_coding_tools`); the `u16` core's `Picture` is reduced to 8 bits until H2.2.
**H2.2 (High 10 4:2:0) done** (2026-10-14 ~11:55, self-reviewed): H2.2a `b011e9a4` QpBdOffset (slice QP /
mb_qp_delta ranges and wrap, negative QP_Y / QP_C as `i8` in `MbState`, dequant at QP′); H2.2b `0195ae20` I_PCM at
BitDepth bits (`read_pcm`, CAVLC + CABAC); H2.2c `6d9e6f1d` `Sps::check_software_supported` (9–10-bit, equal luma /
chroma depth; `check_supported` / `check_coding_tools` stay 8-bit for the VA-API fronts), explicit WP offsets ×
(1 << (BitDepth − 8)), `Picture::bit_depth` + `Picture::high` (16-bit planes), codecs → `PixelData::Yuv16 { bits }`;
guide `hardware.md` row. **Six x264 `high10` fixtures (intra, CAVLC B, weightp + fade, qp 1, 4 slices + deblock
−2/−2, odd crop) bit-exact vs ffmpeg's yuv420p10le decode** (1 / 3 / all threads), MP4 High 10 seek test
(`crates/codecs`) sample-exact; 8-bit speed unchanged (99.7 fps). Owed / later: a `u16` plane allocator (output
planes use `Vec::with_capacity`, not the frame pool), 10-bit decode speed (not measured).
**H2.3 (High 4:2:2 intra, 8 / 10-bit) done** (2026-10-14 ~12:30, self-reviewed): H2.3a `ad61e1ed` 4:2:2 geometry
(`Planes::cheight`, `Frame::c422`, 16 chroma lines per published MB row, windows / cropping, `Picture::chroma_format_idc`,
codecs `C422`); H2.3b `f28cafa7` 8×16 chroma intra prediction (DC rules per 4×4, plane yCF = 4); H2.3c `8af81317`
chroma residual (2×4 DC: CAVLC nC = −2, CABAC cat 3 Min(i/2, 2), [[c0,c2],[c1,c5],[c3,c6],[c4,c7]] transform at
QP′c + 3; 8 AC blocks with 2×4 neighbours; 4:2:2 I_PCM); H2.3d `4e9d88df` deblocking (vertical chroma edges over 16
lines, horizontal at chroma rows 0/4/8/12 from luma edges 0..3, odd edges' bS derived under the 8×8 transform too),
4:2:2 allowed in `check_software_supported`, **4:2:2 P / B slices refused per slice until H2.4**; guide
`hardware.md` row. **Six x264 High 4:2:2 Intra fixtures (8 / 10-bit, CABAC / CAVLC, no 8x8dct, qp 1, 4 slices +
deblock −2/−2 + odd crop) bit-exact vs ffmpeg** (1 / 3 / all threads + the u16 path). deblock −2/−2 + odd crop) bit-exact vs ffmpeg** (1 / 3 / all threads + the u16 path).
**H2.4 (High 4:2:2 inter) done** (`8a9e9321`, 2026-10-14 ~12:55, self-reviewed): chroma MC at full vertical
resolution (yIntC = yAL + (mv >> 2), yFracC = (mv & 3) << 1), 8×16 chroma predictions / weighted stores, P / B
refusal lifted; slice decoding and deblocking are now **monomorphised over the chroma format** (`SliceDecoder<S,
C422>`, `deblock_mb_planes<S, C422>`, dispatched per slice / MB) so 4:2:0 carries no 4:2:2 branches. **Five x264 High
4:2:2 Long-GOP fixtures (B-pyramid + 4 refs, CAVLC, weightp + weightb on a fade, temporal direct + odd crop, qp 1;
8 / 10-bit) bit-exact vs ffmpeg**, MP4 4:2:2 10-bit seek test (`crates/codecs`) sample-exact; guide `hardware.md`.
**8-bit 4:2:0 speed is 2.3 % below pre-H2.3** (perf_1080p, 1 thread, builds with forced code alignment
`RUSTFLAGS="-C llvm-args=-align-all-functions=6 -C llvm-args=-align-all-nofallthru-blocks=5"` and their own target
dir, interleaved: 99.6 → 97.3 fps; unaligned builds vary ±1.5 % by layout). Bisect: H2.3a 0, H2.3b–d ~−1.5, H2.4
before monomorphising −2.3 (fixed). Ruled out: `MbState` / scratch sizes (probe at the old sizes: same speed),
chroma-window / add-chroma loop bounds. No `perf` on the Dell (no sudo): **profile owed at H2.7** (or on the PC).
**H2.4 never-crash rounds** `cdd9a216` (High 10 / 4:2:2 bit flips incl. parameter sets, truncation, a stream switching
bit depth and chroma format; no panics, caught or not). **H2.7a** (perf_4k_422_10): 4K 10-bit 4:2:2 **Long-GOP
43.2 fps, Intra (632 Mbit/s) 26.8 fps** with 16 threads on the Dell (1 thread: 8.5 / 3.3), fork-benchmarks.md;
**slice threads deferred (Q32, FYI)**; fps on the i5-11600K owed at acceptance.
**Q31 answered (a): ITU-T H.264.1 conformance streams may be downloaded (Dell only, never committed).**
**Conformance (H2.8a):** ITU-T H.264.1 AVCv1 + FRExt + the Hi422 / Hi10 Intra professional streams (4:4:4
skipped) **downloaded** to the Dell's `~/fork-data/h264-conformance/` (219 archives, `SHA256SUMS*`; outside git) and
extracted into `extracted/` (218 dirs; `~/fork-logs/conf-extract.sh`). Harness `tests/conformance_vectors.rs` (ignored;
`FILMCRAFT_H264_CONFORMANCE_DIR=~/fork-data/h264-conformance/extracted cargo test --release -p filmcraft-h264 --test
conformance_vectors -- --ignored --nocapture`); `f65643cb` (merged) crops references stored uncropped (CVFC1) and
accepts references that repeat pictures / mix fields per the pic_struct SEI (CVSE2 / CVSE3 / CVSEFDFT3) when both
fields of every decoded picture appear in order. **hevc-vaapi (2026-10-14 14:40): 125 pass, 92 unsupported (all
interlaced, FMO, SP/SI, monochrome), 1 skipped (alpha), 0 failed.**
**H2.5 (PAFF in software) merged 2026-10-14 ~19:20** (`d94bb1e8` and before; was branch `dell-lane-h2-5`) (local + the Dell's scratch
repo; on top of): H2.5a field-aware `Frame`; H2.5b intra field pictures (first /
second field, interleave after the second, unpaired field filled by copying); H2.5c inter fields (reference
fields, chroma offset, field scans and CABAC field contexts, field deblocking); H2.5d direct prediction in
fields (Table 8-6 colocated, Frm_To_Fld); H2.5e frames referencing field pairs (Fld_To_Frm), frame-coded
pictures stored with both field POCs, PAFF sequences accepted without the test hook (MBAFF ones still refused until
H2.6), guide `hardware.md` rows; PREP-H2 §6 "As built" (the design's chroma-offset sign was reversed, and
it missed field scans / contexts / deblocking). **Branch results on the Dell: fmt, workspace clippy, h264 tests
(incl. new synthetic field test + damaged-field-stream rounds), layers, wasm green; conformance 168 pass / 49
unsupported (42 MBAFF, 3 FMO, 2 SP/SI, 2 monochrome) / 1 skipped / 0 failed (+43 PAFF streams bit-exact);
8-bit progressive perf_1080p 1 thread −0.9 % (96.4 vs 97.3 fps, alignment-pinned A/B, inside layout noise).**
Engine test `ai_explain_tests::panic_error_and_timeout_fall_back` failed once under full-workspace load and passes
alone (timing; not touched here). Self-reviewed only (weekly usage above pace, no subagents).
**H2.6 (MBAFF) merged 2026-10-14 ~19:20** (through H2.7b `346c0cc1`; was branch `dell-lane-h2-6`) (on top of `dell-lane-h2-5`; both
on this PC and the Dell's scratch repo): H2.6a pair addressing / mb_field_decoding_flag / Table 6-4 neighbours /
field-MB reconstruction (MBAFF a const generic of `SliceDecoder`); H2.6b field-MB references, weights, MC; H2.6c
direct with MBAFF colocated pairs; H2.6d MBAFF deblocking (pair order, mixed edges line by line, field-mode top
edge), chroma DC per-half availability; H2.6e interlaced accepted by default (hook gone), guide `hardware.md` row;
PREP-H2 §6 / §7 "As built" notes. **Branch results on the Dell: fmt, workspace clippy, h264 / codecs / media tests,
layers, wasm green; ITU-T conformance (218 archives) 210 pass / 7 unsupported (2 monochrome, 3 FMO, 2 SP/SI:
outside H2) / 1 skipped / 0 failed — every PAFF and MBAFF stream bit-exact; 8-bit progressive perf_1080p 1 thread
98.4 vs 97.4 fps (base `9c7f81a5`); H2.7b (same branch): x264 1080i MBAFF bit-exact vs ffmpeg, 31.9 fps on 1 thread,
144.7 fps on 16 (fork-benchmarks.md).** Dell-only debug tools in `~/fork-logs/fc-tools/` (never committed).
**Reviewed and merged (2026-10-14 ~19:20, Dell lane):** fresh-context Opus review in the run's own session (Q33
update; no subagent): no blocking findings (H2.5's duplicated colocated comment was already fixed in H2.6; unpaired
first fields are filled on the decoder thread, which waits for that field's job: latency, not a hang; MBAFF neighbour
indices stay inside the even pair geometry, and any slip is caught per picture as gray). Rebased onto hevc-vaapi
(guide `hardware.md`: one interlaced H.264 row, VA-API first, own decoder as fallback). **Gates on the Dell after the
rebase:** fmt ✓, workspace clippy ✓, h264 / codecs / platform / media tests ✓, layers ✓, wasm ✓, **ITU-T conformance
210 pass / 7 unsupported (monochrome, FMO, SP/SI) / 1 skipped / 0 failed.**
**H2 ready for acceptance (Dell lane, 2026-10-14 ~19:55; tag `h264-2` rides with the main runner's next phase
group).** All of H2.1–H2.6, H2.7a / H2.7b and H2.8a / H2.8b are merged (last: H2.8b `0f929430`). Checklist (Dell,
i5-13500T): **ITU-T H.264.1 conformance 210 pass / 7 unsupported (2 monochrome, 3 FMO, 2 SP/SI: outside scope) / 1
skipped (alpha) / 0 failed** (every in-scope profile: Baseline / Main / Extended-as-Main / High / High 10 / Hi422 /
Hi10 Intra / Hi422 Intra, progressive, PAFF and MBAFF); the fixture matrix (x264, plus VideoToolbox streams on macOS) 57 bit-exact vs ffmpeg (incl. High
10, High 4:2:2 8 / 10-bit Intra and Long-GOP, and since H2.8b four interlaced MBAFF ones: CABAC, CAVLC bff, High 10,
High 4:2:2 10-bit); never-crash rounds (bit flips incl. parameter sets, truncation, depth / chroma switches,
damaged synthetic field / MBAFF streams, damaged x264 MBAFF streams; 1 and 4 threads): no panic, no hang; 4K 10-bit
4:2:2 25p (H2.7a): Long-GOP 43.2 fps, Intra 632 Mbit/s 26.8 fps with 16 threads (1 thread 8.5 / 3.3); 1080i MBAFF 31.9
fps on 1 thread, 144.7 on 16; guide `hardware.md` rows. **Deferred (FYI):** slice-level threads (Q32; frame
threading carries 4K 4:2:2). **Owed on the PC at acceptance:** fps on the i5-11600K (`cargo test --release -p
filmcraft-h264 --test perf -- --ignored --nocapture perf_4k_422_10 perf_1080i_mbaff`), the H2.7 profile of the
−2.3 % 8-bit 4:2:0 slowdown (no `perf` on the Dell), the 4K 4:2:2 fixtures and a 1080i file played in the app
(a real camcorder 1080i file if the owner has one).
**H3.0 audit merged** (`ed98d5c5`, docs/fork/PREP-H3.md, 2026-10-14 15:45): once H1 / H2 merge, ffmpeg is already
unnecessary for every in-scope H.264 stream; choice (c): H3.1 drops the ffmpeg pipe from the chain for H.264 only
(HEVC left to Phase 7.7), H3.2 clip badge / Decoder line, H3.3 ffmpeg-hidden + hardware-pulled harness, H3.4 playback
check + guide. H1 and H2.5 / H2.6 are merged, so H3 code can start (after H3.2a's review).
**H3.2a merged** (`e63c6410`, 2026-10-14 ~19:35, reviewed in-run, gates green on the Dell after the rebase; was branch `dell-lane-h3-2`)
(`git diff origin/hevc-vaapi...dell-lane-h3-2`; local + the Dell's scratch repo): the Project panel says when
H.264 decodes in software: High 4:2:2 gets the 4:2:2 badge (never "hardware failed", which is HEVC-only now),
High 10 4:2:0 an amber "10" badge "H.264 10-bit: software decode"; not claimed under VideoToolbox (Apple silicon may
take those profiles). `proxy_jobs::{is_h264, bit_depth, software_h264_high_bit_depth}`, `hardware_422_failed(info)`;
engine tests (hostile pixel-format labels) + UI test `media_ui::h264_software_decode_badges` (Premiere and Resolve);
es / zh-cn; guide `hardware.md` options table (edit the original / proxy / intra export). Dell gates: fmt, workspace
clippy, engine proxy_jobs + coach, ui media_ui + coach + i18n, layers, wasm green. Joins the Q33 review queue (a
low-risk UI diff: a Sonnet review is enough). **H3.2b** merged (below).
**H3.1 + H3.3 + H3.4a merged** (`66ef1c63` / `34a843f7` / `9fdbc1b3`, 2026-10-14 ~19:00, in-run fresh-context
review of `crates/platform` (no new `unsafe`; no blocking findings: the chain's closures, the pipe-name check and the
guide's "H.264 never uses ffmpeg" hold; H.265 export is the only other ffmpeg user); was branch `dell-lane-h3-1`):
**H3.1** H.264 never decodes through the ffmpeg pipe (`vaapi_chain` in `crates/platform/src/lib.rs`: zero-copy →
read-back → FilmCraft's own decoder; HEVC keeps the pipe until 7.7; `hardware_decoder_for` follows it), **Q34** (FYI,
choice c); **H3.3** `crates/platform/tests/ffmpeg_free.rs` (ignored): the product's chain in child processes with
ffmpeg hidden, hardware / Hardware decoding Off / VA driver pulled × 7 fixtures (H.264 progressive, High 10, 4:2:2
10-bit, MBAFF, MBAFF 4:2:2 10-bit; HEVC Main, Main 10): **all pass on the Dell's UHD 770, every picture identical to
the software decoder, none through the pipe**; **H3.4a** guide `hardware.md` § "H.264 without ffmpeg". Base had moved
by docs only, so the branch's Dell gates stand (fmt, clippy, platform tests incl. VA-API hardware, coach, layers).
**H3.2b merged** (`8253a5a5`, 2026-10-14 ~19:15, self-reviewed: weekly usage above pace): File ▸ Get Media File
Properties for ▸ Selection… has a **Decoder** line (the decoder that decoded the file last, e.g. "VA-API H.264 (in
process)" / "FilmCraft H.264"; follows a hybrid's mid-stream fallback; "chosen when the clip first plays" before the
first frame; files only). `GopCache::decoder_name` (a leaf lock, never waits on a decode; intra decoders recorded when
made, outside the race window `intra_frames_decode_in_parallel_once_each` measures), `MediaSource::decoder_name`
(default `None`; mp4 / mkv / mxf / mpeg / apv and the unreadable-slate wrapper forward it), `decoder` in
`file.mediaProperties`; codecs + engine + UI tests (`media_ui::media_properties_decoder_line`, Premiere and Resolve);
es / zh-cn; guide `hardware.md`. Dell gates: fmt, workspace clippy, codecs gop / media / engine project_tools +
media_pool + coach / ui media_ui + menus_ui + coach + i18n, layers, wasm green (re-run after the rebase onto AI3.6a).
`intra_frames_decode_in_parallel_once_each` failed once under full load in the first gate (6 / 6 alone; then the
record moved out of its window). **H3.4b done (2026-10-14 ~19:30, no code):** the release app in a virtual KWin on the Dell (`kwin_wayland --virtual`,
1920×1080), ffmpeg hidden (`FILMCRAFT_FFMPEG=/nonexistent`, PATH without it; log: "ffmpeg not usable"), `--empty`,
control channel: four 10 s 1080p25 x264 clips (High, High 10, High 4:2:2 10-bit, MBAFF tff), each its own sequence,
9 s of playback each. **Played 220–224 frames, 0 dropped**, except **1 dropped on the first software-decoded play of a
session** (High 10 or 4:2:2, whichever plays first; 0 on every later play: warm-up, not codec-specific; noted, not
fixed). High and MBAFF: `hardware.frames` 236, `backends` only "VA-API H.264 (in process)"; High 10 and 4:2:2 (proxies
off): `softwareFrames` 237 / 239, no backend; with Automatic proxies on, the 4:2:2 clip played from its proxy (HEVC on
VA-API). **No stream went through the ffmpeg pipe.** Properties dialog (screenshots): "Decoder: FilmCraft H.264" /
"Decoder: VA-API H.264 (in process)". Scripts / logs on the Dell: `~/fork-logs/h3-4b-{run,run2,drive}.*`.
**H3 ready for acceptance (Dell lane, 2026-10-14 ~19:35; H264-PLAN H3; tag `h264-3` rides with the main runner's next
phase group).** Merged: H3.0 audit `ed98d5c5`, H3.1 `66ef1c63` (H.264 never through the ffmpeg pipe), H3.2a `e63c6410`
(software-decode badges), H3.2b `8253a5a5` (Decoder line), H3.3 `34a843f7` (ffmpeg-free harness: 7 fixtures × 3
setups pass on the UHD 770), H3.4a `9fdbc1b3` (guide "H.264 without ffmpeg"), H3.4b above. **Owed on the PC at
acceptance:** `cargo test --release -p filmcraft-platform --test ffmpeg_free -- --ignored --nocapture` on the RX 6700 XT;
the same in-app playback (ffmpeg hidden) on both GPUs; H1's 1080i zero-drop playback; H2.7's profile. Dell leftovers: A/B worktree `~/Projects/fc-base` (at
`9c7f81a5`) + `~/Projects/fc-perf-*` targets (≈0.5 GB, reusable for perf A/B).

## Status

- **Branch** `hevc-vaapi` (never touch `master`). Tags `phase-1` … `phase-6`, `grade-0` …
  `grade-2c`, `upstream-sync-1`, the yt-1 group (`resolve-ui-1`, `resolve-ui-2`, `phase-6-7b`,
  `coach-1`, `yt-1`) and the **6E group on `d866cb4c` (2026-10-13): `phase-6e`, `linux-1a`,
  `linux-2a`**, **`upstream-sync-2` on `e090c0c9` (2026-10-13, U0.2 + U0.2.3b)** and `ai-1` on
  `bcb5f713` (2026-10-14) and the **X422 group on `f8c3e788` (2026-10-14): `x422`, `audio-1`, `linux-1b`,
  `linux-2b`** and **`yt-3` on `db7c1d0d` (2026-10-14)** and **`comp-1` on `236c1c0c` (2026-10-14)**. Builds in `~/Apps/filmcraft-builds/`; `latest` → `comp-1`; launcher
  `~/.local/share/applications/filmcraft-fork.desktop`. Public (§1a.33): dawgon2/filmcraft-cachyos
  branch `fork` snapshot `ca059e2e` (of `comp-1`, parent `9abdf285` = `fork-yt-3`), tag `fork-comp-1`,
  release `fork-comp-1` (scrubbed Dell build; NOTES.md); earlier tags `fork-yt-3`, `fork-x422`, `fork-audio-1`,
  `fork-linux-1b`, `fork-linux-2b`.
- **U0.2 as shipped:** one VA-API stack: upstream's H.264 / HEVC front ends on the fork's
  `va::Display` with `va_accel::ZeroCopyAccel` (zero-copy, hand-off incl. scaled, read-back mode);
  fork `va_hevc.rs`, `crates/hevc/src/accel.rs` and upstream's `vaapi/va.rs` deleted. Displays are
  kept for the process's life and `filmcraft_platform::quiesce()` runs before exit (a CLI crash at
  exit, fixed with a regression test). Exports (U0.2.3b, synthetic, full length): 4K 32.5–35.1,
  4K graded 30.5–31.1 (Auto: Intel decodes, scaled hand-off, Q20), 6.2K 25.5, 6.2K graded 19.6 (Q21).
- **6E as shipped:** two-GPU HEVC export (`crates/export/src/lanes/`: chunker, scheduler,
  coordinator with the **slow-lane rule** `retire_slow_lanes`, spill files; `hev1` when the
  drivers' headers differ), Export engines setting, encoder speed preset, 4:2:2 10-bit master on
  Intel. Synthetic: 6.2K 25.8 fps dual vs 19.4 single, 4K 33.9 vs 24.6, graded 4K 24.2 vs 22.3
  (`docs/fork-benchmarks.md` "Phase 6E"). Diagnostic env vars: `FILMCRAFT_EXPORT_TRACE=1`,
  `FILMCRAFT_EXPORT_GPU_PROBE=1` (per-frame GPU time per device), `FILMCRAFT_BENCH_ENGINES=dual|single`,
  `FILMCRAFT_BENCH_KEEP=<dir>`.
- **Schema v18** (C1 comps), prefs version 3. New user-visible strings need es + zh-cn rows. New guide pages go
  in `GUIDE_PAGES` (`crates/engine/src/coach_ask.rs`) and their `{ui:}` ids must be known to the
  `coach_ui` walk; dynamic automation ids (`format!`) must be listed in
  `crates/ui-egui/src/coach_ids.rs::DYNAMIC_ID_PATTERNS` (a test fails otherwise). Coach eval floor 94.
- **RS1** feature `fork-resolve-default` is on in ci: new UI tests that assume Premiere set
  `prefs.workflow = Premiere`.
- **Desktop integration is off under `--control`** unless `FILMCRAFT_DESKTOP_INTEGRATION=1`
  (MPRIS, taskbar progress, notifications, inhibit).
- **Public-snapshot hygiene:** `publish-snapshot.sh` refuses the owner's user name, e-mail name or a
  LAN address it can't rewrite (it rewrites the home path, not a `/media/…` or `/run/media/…` mount
  path, and not a name inside prose): use `user` in test fixtures and never write the owner's user
  name in docs.
- **Owed (not blocking):**
  - *U0.2:* ~~Opus review of U0.2.3b d and U0.2.4 / .5~~ ✓ 2026-10-15: fixed (1) an MP4 clean aperture was cut in full-size
    coordinates from a scaled hand-off picture (wrong part / aspect): such sources hand off unscaled (`GopCache::crops`, test
    `a_clean_aperture_keeps_the_hand_off_unscaled`); (3) `with_handoff_scale` restores on unwind (drop guard, test
    `a_panic_inside_restores_the_scale`). ~~(2) comp items in `handoff_scales`~~ ✓ (media a comp's Media In reads keeps full size, a comp reading a track keeps the whole sequence at full size; test `media_a_comp_reads_keeps_full_size`). **Left:**
    (4) `quiesce()` vs live decode / encode threads: ✓ in part 2026-10-15 (each picture submit — decode, video processing,
    encode — holds `Busy`, so exit waits for one in flight and refuses new ones; test `crates/platform/tests/va_exit.rs`,
    diagnostic `va::work_refused_while_exiting`); ✓ 2026-10-15 04:10: surface work outside a submit (sync, image read / write,
    DMA-BUF export, new / spare surfaces, coded-buffer maps) holds it too (test `crates/platform/tests/va_exit_sync.rs`, failed before);
    (5) ~~`va.rs` Send / Sync comment~~ corrected 2026-10-15; ~~drivers other than Mesa / iHD shared unchecked~~ ✓ 2026-10-15 05:12 EDT:
    `Display::open_raw` refuses them (`driver_is_thread_safe`: vendor string starts "Mesa Gallium driver" / "Intel iHD driver"),
    software decode instead, `FILMCRAFT_VAAPI_ANY_DRIVER=1` overrides (Q36; per-display serialisation for NVIDIA = Phase 8);
    ~~(6) nit: `KEPT` cap 64 unreachable today~~ ✓ 2026-10-15 04:33 EDT (named `KEPT_MAX`, a display past it is logged as not kept);
    ~~`teardown_ui` flake~~ ✓ 2026-10-15 (reproduced with `teardown_ui timeline text title trim`
    at 6 threads: other tests' ~100 frame / background threads starve the closed app's last frame; the test now
    checks that no queued frame job runs after the close and that everything exits within 2 min, via
    `FrameServer::live_workers`; a missing stop still fails it). **Left:** render-cache cancel latency inside one frame (CLOUD-QUEUE item 10, cloud
    diagnosis); the 2 min bound is only a never-stops check for it. Thread names in its failure list are
    inherited: unnamed threads started from a `filmcraft-background` / `filmcraft-frames-N` thread (rayon pool,
    decoder threads) carry that name on Linux, so ~38 "filmcraft-backg" is not one worker per session; ~~hand-off scale only for MP4 / MOV sources~~ (MKV and MXF implement
    `set_handoff_scale` now, test `crates/codecs/tests/handoff_scale.rs`); U0.2.3 owed (ii) re-grey test needs failure injection, ~~(v)
    `va_dual_export::two_gpus_keep_the_colours_of_one` with the Intel device forced~~ run 2026-10-15
    (`FILMCRAFT_VAAPI_DEVICE=/dev/dri/renderD128`): colours kept (worst mean chroma shift 0.06, bound 1.0), but its PSNR bound
    fails: 2.15 dB (bound 0.5), because the second lane is then AMD, whose encoder gives lower chroma PSNR than Intel's at the
    same settings; with AMD primary (the default) the second lane is the better encoder. Not a colour bug; if Intel-primary
    dual exports matter, compare against the weaker GPU's single export in that test.
  - *6E:* Intel lane barely helps graded exports (retired); `Rgba16Float` effect stage on
    integrated GPUs would let it; a hung lane can still be the slow-lane comparison until the
    watchdog; (`perf.stats` lanes now report `retired` apart from `failed`, 2026-10-13);
    4:2:2 master is CPU-converted (2.7 fps; GPU → Y210 is worth a step); ~7 GB RSS for a 6-frame
    6.2K dual export in `hevc_presets`.
  - *L1 / L2 part 1, on the owner's desktop* (visual / System Settings / sudo; NOTES "Please
    check"): KWin look + icon, Program monitor 1:1 at 125 / 150 %, accent / light-dark / Animation
    speed live, window size restore, 10 min without lock, Dolphin thumbnails / double-click /
    paste, nested non-KDE compositor, `makepkg -si`. Verified here: native Wayland, MPRIS + Pause,
    portal Inhibit on play, taskbar progress + notification, journal priorities, single instance,
    X11 run.
  - *Y1:* idle zero-copy slots not trimmed / counted; AMD open-GOP-seek parity covers the I
    picture only; the fork HEVC pool may share the iHD context-lifetime hazard; Dell run of
    `tests/vaapi.rs` (U0.2.3 has a Dell run anyway).
  - *Earlier:* ~~6.7b TNR keyframe markers / label width~~ ✓ 2026-10-15 (lane diamonds: click seeks, drag = `tnr.moveKeyframe`); RS3 cramped Color page, Markers vs Index,
    missing panels, Fairlight strip, home-screen screenshot compare; Phase 6 frame-server proxy
    hint, ~~`cache.claimFolder`~~ ✓ 2026-10-15, decode-to-screen fps with hardware 4:2:2, software 4:2:2 memory cap
    (ask first), TNR / cache / proxy 6.2K bench cases.
- **DC1 done (2026-10-13):** `fork/dell-ci/` installed on the Dell (user timer, every 2 min, only
  while the owner is logged in: Linger=no). Use: `git push ssh://dell~/Projects/filmcraft-main
  HEAD:refs/heads/dell-ci/<kind>-<id>` (kinds `ci`, `test-<crate>`, `intel` = `fork/dell-ci/intel.list`,
  `soak-<name>` = `fork/dell-ci/soaks/<name>.sh`), keep working, later
  `ssh dell 'cat ~/filmcraft-ci/results/<kind>-<id>.txt'` (3 lines; `.log` next to it, read only on FAIL).
  Self-tests: soak-selftest-fail → FAIL with its line, test-filmcraft-time → PASS. Gentle mode per
  §1a.56 (games: GPU jobs wait; MPRIS playing: 8 build jobs, soaks wait; < 3 GB free: no new job).
  `intel-1` (U0.2.3 owed iii): PASS in 3m47s.
- Cloud: CLOUD-QUEUE has C1-PREP, AI1-PREP, R1-PREP (docs only). Merge `origin/cloud-work` first
  if it has new commits.

## Exact next step (2026-10-16 15:30 EDT, owner §1a.99): finish the S1 step in progress → tune + search by meaning → UPD → XPS → pause

**New order (owner, live, §1a.99, replaces "then S1 item 5d" below):** finish the S1 step you are on (one green commit), finish the §1a.97 tune + eval (LoRA `assist-v1` is training in the background) and §1a.98 search by meaning (model on disk since 14:17, Q51: run the keep / drop eval), then **UPD** (§1a.77; test against mocked GitHub replies, Q47 hold), then **XPS** (the owner's Dell XPS 16: 155H Arc iGPU + RTX 4050; FORK-PLAN §1a.99 items a–f), then create `.fork-paused` and stop until the owner says the laptop is ready. The rest of S1, REF, C2 … come after the pause.

**Standing (§1a.100):** every user-visible tool / feature you commit gets well-written guide text **and** a row in `docs/fork/coach-eval/pool/FEATURE-QUEUE.md` in the same commit; the Dell lane builds training / held-out sets from the queue (backfill rows for S1.1–S1.5, AI4, AI5, search by meaning are seeded). Rows the lane sets to `guide wanted` come before your next feature.

### Was: §1a.98 search by meaning + §1a.97 assistant tune (AI4.5b-late), then S1 item 5d (2026-10-16 14:12 EDT)

**Corrected order (§1a.97 / §1a.98 say the tune and search by meaning come before S1 continues; the 13:xx runs missed it).**
- **Q45 search by meaning:** encoder `filmcraft_llm::embed` (BERT, candle-checked), engine `coach_meaning.rs`
  (`SearchMode` Words / Meaning / Mixed, `coach_ask::retrieve_in`, cached section vectors), eval
  `ai_pool::search_by_meaning_coverage` (ignored; needs the model). **Model not on disk: Q51** (the download was refused
  by the session's safety check). Default stays Words until the eval says keep.
- **Tune (§1a.97):** item 1 ✓ (tinylm SmolLM3 + converter + parity 5 / 5, `parity_lm.py`, `llm_greedy`). Item 2 ✓
  (`assist_traindata` example → `~/Apps/filmcraft-train/runs/assist-data-v1`, 750 / 88). Item 3: LoRA run
  `~/Apps/filmcraft-train/runs/assist-v1` (`train.log`, `best-lora.pt`) → merged `models/tuned/smollm3-assist-v1`.
  Item 4: `llm_quantize <tuned> <tuned>/smol3-assist-v1-q4k.gguf --type q4k`, copy `tokenizer.json`, a `local!` catalogue
  entry (sha256 + size, models.rs like the explainer tunes), then the eval for both models:
  `FILMCRAFT_LLM_DIR=~/.local/share/filmcraft-fork/models ASSIST_EVAL_SET=daily [ASSIST_EVAL_MODEL=<id>] fc-cargo.sh test
  --release -p filmcraft-engine --features llm --lib real_assistant_eval -- --ignored --nocapture` and the AI3 advanced set
  (no `ASSIST_EVAL_SET`); keep only if better on both with 0 invented and creative not falling; else drop the entry, log why.
- Then S1 item 5d (below): **done 2026-10-16 15:30 except trimming**: stills, user effects (installed on import, never
  overwritten), size estimate in the status line, Package as One File… tile item, Import Package takes `.fcpackage` /
  `manifest.json`, `"trim": true` (stream copy, `filmcraft_isobmff::trim_mp4`) ✓ 16:10. 
- **Tune + search evals done 2026-10-16 17:10 (progress log):** tune v1 rejected (one-section answers), Mixed not
  default (AI3 −1, daily −2); defaults unchanged. v2 plan in the log. Eval recipe: `real_assistant_eval` with
  `ASSIST_EVAL_SET` / `ASSIST_EVAL_SAMPLE` / `ASSIST_EVAL_MODEL` / `FILMCRAFT_ASSIST_SEARCH`, ~1 s per case on the GPU.
- **S1 acceptance next (2026-10-16 17:20):** S1's code items are done (5d incl. trim; newer-project import warns). Left:
  (1) the real-share acceptance on sshfs (Q50 update in OWNER-QUESTIONS: `sshfs -o reconnect,ServerAliveInterval=5,
  ServerAliveCountMax=2 dell:~/fc-share ~/fc-share`; copy a 6.2K synthetic clip + a project there; drive the
  app with `--control` (or engine-level tests with the real mount): play, edit, save on the share; kill the `sshfs` process
  mid-playback and mid-save → UI stays responsive (frames keep advancing), clips go offline, the local backup opens;
  remount → relink by itself; 6.2K playback from the local copy matches local playback); record results (synthetic);
  (2) `cargo xtask ci`; (3) tag `storage-1`, build per §1a.9; (4) publish per §1a.101 (held ai-3/4/5 + storage-1, delete
  `.fork-publish-hold`, Q47 answered). Optional S1 owed items (not acceptance gates): CLOUD-QUEUE 33 / 34, home-screen trim
  choice.
- **§1a.102 order now (replaces §1a.99's):** tune eval + search-by-meaning eval (model on disk, Q51 answered) → **the rest
  of S1** to acceptance (home-screen trim choice, the S1 owed items in the status below, `cargo xtask ci`, tag `storage-1`,
  build; the real-share check Q50 stays owed) → **publish** held `ai-3/4/5` + `storage-1` (§1a.101, delete
  `.fork-publish-hold`, Q47 answered) → **UPD** (§1a.77, live check against that release, tag `update-1`) → **XPS** (a–f
  without the laptop) → pause (`.fork-paused` with the line `dell-lane: feature-pool`). §1a.100: every feature commit
  gets guide text + a FEATURE-QUEUE row; the Dell lane marked "search by meaning: guide wanted".

### Was: S1 (STORAGE-PLAN Phase S1), item 5d, then the rest listed below

**AI5:** accepted, tagged `ai-5` (19655fb4), build `~/Apps/filmcraft-builds/ai-5` (`latest`), NOT published (held with
`ai-3` / `ai-4`, `.fork-publish-hold`, Q47).

**Standing runner step (§1a.92):** each run `python3 -I tools/fork/ai-inbox-import.py` (provisional Q48).

Order after S1 (§1a.93): REF → C2 → AI2 → A3 → S2 → G3 → UPD → AI6 → 7 … (check §1a.6 and later §1a entries first).

**S1 status (everything below is tested on a simulated share: a fake mount-table line over a local folder,
`netfs::test_mounts::add_share` in engine tests, `project_views::use_mount_table` in UI tests). Guide:
`docs/guide/network-storage.md` (all sections written so far).**
- **Item 1 ✓** `crates/engine/src/netfs.rs` (`classify`, mount table cached 2 s), `media.storage`, Project Settings ▸
  General storage lines, NET / NET ✓ badge in the Project panel.
- **Item 2 ✓ (core)** `crates/engine/src/net_cache.rs`: `NetCache` store (versioned keys, index, size limit, resume
  from `.part`, restat), `MediaPool::open_file` reads complete copies (last copy only when the share is unreachable),
  `Session::start_net_copy` / `poll_net_copy` worker, UI `tick_net_copies`, settings `mediaCache.networkCopies` /
  `networkCopiesGb` (default auto / 100 GB, provisional **Q49**). Owed: free-disk floor (needs a statvfs in
  `crates/platform`, FFI → Opus review), `media.storage` copy state per item.
- **Item 3 ✓ (core)** `crates/media/src/deadline.rs` (`with_deadline`, `TimeoutReader`), `netfs::share_call` (share
  down 30 s after a timeout), FsServices read_file / file_size / reader / file_stamp limited on shares, TimedOut →
  Offline, `media_watch::follow_presence` (offline / back online by itself), audio peaks open on their worker. Owed:
  source-audio start (`source_playback.rs:132`) and Link Media preview (`media_dialogs.rs:136`) still open on the UI
  thread (≤ 8 s on a dead share); a source whose `TimeoutReader` died while the share's stat still answers stays dead
  until an offline → online cycle; one worker per `TimeoutReader` instead of a thread per read; limits for `list_dir`,
  `file_loader`, `read_range`, `write_file`; macOS / Windows detection (CLOUD-QUEUE 31).
- **Item 4 ✓** safe saves on shares (`write_project` in `commands.rs`: local copy first, read-back, clear error;
  share projects auto-save locally).
- **Item 5 ✓ (core)** `crates/engine/src/package.rs` (manifest, SHA-256, verify), `projects.package` (Project Manager
  collect + `manifest` flag, `dryRun` estimate), `projects.importPackage` (copy, verify the copy, relink under the
  sender's package folder), home screen **Package** / **Import Package** / tile **Package…**. **Next 5d:** stills
  gallery images, voice-over takes, title fonts that aren't bundled, user WGSL effects, caption files (whatever isn't a
  media item today; check `project_manager.rs` `used_ranges` and the project model for their paths), trim to used
  ranges without re-encoding (keyframe cut where the container allows, else whole file; never a silent re-encode),
  ~~`.fcpackage`~~ ✓ (`format: "file"`, `package::zip_folder` / `unzip_package`), estimate + one-file choice on the
  home screen (Import Package picks folders only today).
- **Acceptance owed:** a real share (**Q50**: sshfs or Samba needs sudo) with network pulled mid-playback / mid-save;
  `cargo xtask ci`; tag `storage-1`; build per §1a.9; publish per §1a.33 unless held.

**Still open from AI4 / AI5 (not blocking S1):** picture tune v2 (open-footage download, §1a.96 (c)), assistant tune (Q46),
semantic search (Q45), `adv-lumetri-vs-nodes`; AI5: NFC normalisation of names (no crate in the tree), macOS host name.

### AI5 detail (done)

Code: `crates/engine/src/coach_share.rs` (+ tests), `crates/engine/src/fork_inbox.rs` (feature `fork-inbox`, FORK-ONLY),
`crates/ui-egui/src/panels/share_example.rs`, `tools/fork/ai-inbox-import.py`.

### AI4 detail (done; kept for reference)

#### Was: exact next step (2026-10-16 02:17 EDT): AI4 (AI-PLAN Phase AI4), Dell lane on CREATIVE-POOL

**Where AI4 item 2 (picture model) stands:** Qwen3-VL 4B runs entirely on FilmCraft's own code
(CPU): `vision.rs` encoder (oracle parity), `mrope.rs`, `forward.rs` Qwen3 + `forward_multimodal`,
`convert.rs` in-app BF16 → GGUF (Q8_0 default, Q4_K mix option), `vl::VisionLanguage`, and the
Assistant wiring (`coach.assist` `context: "auto"` + a picture question → `Picture (seen):` line;
`PictureLooker`, look cache, guide `color.coach`). Real demo-frame look: accurate, **28 s on the
CPU** (load + ~256 picture tokens). Env-gated tests: `crates/llm/tests/qwen3vl_{vision,text}.rs`
(`FILMCRAFT_QWEN3VL_DIR=~/.local/share/filmcraft-fork/models/qwen3-vl-4b-instruct`, `--release
--features infer -- --test-threads=1`), engine `real_vision_model_describes_the_demo_frame`
(`FILMCRAFT_DATA_DIR=~/.local/share/filmcraft-fork`, `--features llm -- --ignored`).

**Next, in order (any mode, §1a.94: GPU / design work is done regardless of the usage mode; Opus builders, no Fable this week, §1a.95):**
1. ~~GPU for the look~~ ✓ 2026-10-16 (live): text model on the GPU (`GpuForward` Qwen3 + `forward_multimodal`, Q8_0 batched matmul) and picture encoder on the GPU (`gpu_vision.rs`, f32, parity ≤ 7e-5 on AMD and Intel); both wired in `vl.rs` for discrete GPUs (`FILMCRAFT_LLM_GPU` policy), CPU fallback on any GPU failure, failed GPU objects dropped off-thread; model kept loaded 2 min after a look. 15 fixture looks: 336 s (CPU) → 94 s → **33 s** after the matvec rewrite (≈ 1.5 s per look once loaded), 15 / 15 right, nothing invented. Encoder 512² 0.22 s (CPU 5.75 s); prompt 0.26 s (CPU 5.1 s). ~~Matvec~~ ✓ (4 rows per 64-thread workgroup, vec4 input, ≈ 340 of 384 GB/s; upload staging freed per matrix: the Q8_0 model had pushed weights out of VRAM): Qwen3-VL Q8_0 71 → 17 ms / token, Q4_K mix 53 → 12; Intel RKL 1754 → 160 ms / token (**re-check whether integrated GPUs now beat the CPU for the explainer / picture model before changing the discrete-only policy**). Left (optional): fuse the ~650 small kernels per token (≈ 3 ms, 20–25 %), one shared `GpuContext` for encoder + text model (two wgpu devices today) and attention tuning in `gpu_vision.wgsl` (60 % of a block at 3 072 patches). Intel integrated: both run but slower than the CPU, so not used.
2. Item 2 acceptance: CLOUD-QUEUE 24 (fixture frames for the `needs_clip` questions) → score looks. **Started 2026-10-16 02:27 EDT:** `coach-eval/looks.toml` (15 fixtures) + `tests/look_fixtures.rs`; real run ✓ 2026-10-16 02:32 EDT (`FILMCRAFT_DATA_DIR=~/.local/share/filmcraft-fork fc-cargo.sh test -p filmcraft-engine --release --features llm --test look_fixtures -- --ignored --nocapture`): **14 / 15 right, nothing invented** (02:39 EDT, 15 fixtures, 336 s on the CPU, synthetic); `REAL_FLOOR` 14 + no inventions asserted; more fixtures = CPU work (cloud or MINIMAL).
3. ~~Install path in the UI~~ ✓ 2026-10-16 02:53 EDT: `filmcraft_llm::models::VISION_MODEL` (5 files pinned to Qwen/Qwen3-VL-4B-Instruct rev ebb281ec…, SHA-256 + size), `download_vision`; engine `ai.vision.status|download|remove` (shares the one download slot; remove also deletes the converted `*.gguf`, keeps other files); `real_looker` needs the pinned files installed; Settings ▸ Color ▸ AI explanations ▸ **Picture model** row (`settings.color.ai.vision.download|remove|cancel|progress`); guide `color.coach` install steps. Not yet looked at in a screenshot.
4. ~~Item 2b's per-shot cache~~ ✓ 2026-10-16 04:26 EDT (provisional design: reuse, not a generic description): looks are remembered per shot (`grade_tools::current_shot_key`: clip + its state + media + colour settings, any frame in it; `AssistJobs::shot_looks`, 8 shots); a question that points at the shot (`refers_to_the_shot`: shot / frame / footage / look …) but isn't a measured-fact topic gets that shot's last look as `Picture (seen):` without a new look; how-to questions with "this" don't. Test in `the_vision_model_looks_at_the_frame_with_the_question`. Guide `color.coach`.
4b. **Daily GPU eval ✓ 2026-10-16 04:32 EDT** (`ASSIST_EVAL_SET=daily`, Pick style, RX 6700 XT, 231 s, synthetic): **72 / 147, 0 invented**; lookups 69 / 86, creative 3 / 61 (57 fail "cites fewer than two plan sections"). CPU probe on all 608 creative held-out: ≥ 2 plan sections in retrieval top 6 for only 138 (top 10: 188, top 16: 202); the pick prompt's relevance filter keeps 101. Nearest creative *training* questions' sources added after the top 3: creative 138 → 165–167, but lookup@6 821 → 774–787 (gate by Jaccard ≥ 0.15: 156 / 805; ≥ 0.2: 141 / 814): **rejected** (a trade, not a gain). Semantic retrieval = new download → OWNER-QUESTIONS **Q45** (provisional: word matching only). Creative gains now come from the tune (item 5 (b)) and guide wording.
4c. **AI4 item 3 synthetic frames ✓ 2026-10-16 04:42 EDT** (§1a.96 (c)): `crates/engine/examples/vision_traindata.rs` draws 7 scene kinds with known light (lamp interior, daylight, sunset, overcast, night street, interview, beach) in linear light, applies a camera error (stops, warm / cool, tint, saturation), records display or log (F-Log2 ~17 %, all 10 `LogCurve`s), labels one question topic per frame (light / exposure / white balance / skin / sky / log) from the known scene, and drops a frame whose words disagree with `PictureFacts` (638 of 3 000). Run: `fc-cargo.sh run --release -p filmcraft-engine --example vision_traindata -- --out-dir ~/Apps/filmcraft-train/runs/vision-data-v1 --frames 3000` → 2 133 train / 229 val, 647 MB, prompt = the app's `look_prompt`. Review sample (40) in `docs/fork/coach-eval/VISION-READ.md` (Q44). **Next for item 3:** (i) the open footage incl. log (download approved in §1a.96 (c); do the download from a live session or with the owner's go in chat, licences in `VISION-SOURCES.md`, ≤ 20 GB, `~/.local/share/filmcraft-fork/training-footage/`); (ii) inputs ✓ 2026-10-16 04:46 EDT: `vl::VisionLanguage::training_inputs` (the same prompt code as `ask`: ids with −1 picture rows, M-RoPE positions, answer start, picture rows + 3 DeepStack taps) and `crates/engine/examples/vision_traindump.rs` (`FILMCRAFT_DATA_DIR=~/.local/share/filmcraft-fork fc-cargo.sh run --release -p filmcraft-engine --features llm --example vision_traindump -- --run-dir ~/Apps/filmcraft-train/runs/vision-data-v1` → `inputs-{train,val}.{jsonl,bin}`, bf16, ≈ 3.8 MB a frame, ≈ 0.2 s a frame on the GPU); trainer ✓ 2026-10-16 04:58 EDT: `tools/explainer-train/qwen3vl.py` (text model, q / k norm, interleaved M-RoPE as `mrope::angles`, DeepStack adds, bf16) + `train_vl.py` (LoRA r 16 on q/k/v/o/gate/up/down, loss on the answer, val-loss early stop, writes merged tensors + index + symlinks to the release; `--check n` greedy answers). **Parity:** `train_vl.py --check 3` vs `vision_traindump --ask 3`: first answer word for word the app's, the others the same opening then bf16-vs-Q8_0 drift. Labels no longer name the log curve (not visible in pixels; would teach inventing). Was planned: Qwen3 text in `tools/explainer-train` style (q / k norm, interleaved M-RoPE, DeepStack adds after layers 0–2), picture rows + taps precomputed by our GPU encoder (f16, ≈ 2–5 MB a frame) so Python never runs the encoder; LoRA on the text model only, bf16 base ≈ 8 GB of the 12 GB; (iii) score base vs tuned on held-out frames, keep only if better: tooling ✓ 2026-10-16 05:08 EDT: `vision_traindump --ask 229 [--model-dir <tuned>]` scores answers against the val labels by topic (+ invented log-curve names; converts the tuned text model to GGUF first if missing), and `FILMCRAFT_LOOK_DIR=<tuned> … --test look_fixtures -- --ignored` runs the 15 out-of-distribution look fixtures on a tuned copy (`ai_explain::looker_at`). Keep the tune only if val improves AND looks stay ≥ the base's 15 / 15 with 0 invented. **Tune v1 started 2026-10-16 05:08 EDT** (`~/Apps/filmcraft-train/runs/vision-v1/train.log`, out `~/.local/share/filmcraft-fork/models/tuned/qwen3-vl-4b-look-v1`).
4c'. **Tune v1 REJECTED 2026-10-16 06:56 EDT (synthetic):** LoRA r 16, 1 epoch (266 steps, 1.6 h on the RX 6700 XT), val loss 4.91 → 0.054. In-distribution val frames 80 → **109 / 120** (light 12 → 27 / 27, sky 5 → 14 / 14), but the 15 out-of-distribution look fixtures fell **15 → 13 / 15** (floor 14; **8 / 15 with 6 inventions** once the fixtures' `must_not` lists also name the template scenes, 2026-10-16 06:59 EDT; base stays 15 / 15), and by eye every answer is now one of the seven synthetic scene templates (a tungsten room → "An interview against a plain grey backdrop …", false log / exposure claims the keyword scorer doesn't catch). Kept the untuned model (keep-only-if-better rule); nothing in the app changed. Tuned copy left at `~/.local/share/filmcraft-fork/models/tuned/qwen3-vl-4b-look-v1` (≈ 12 GB, delete when not needed), adapter `~/Apps/filmcraft-train/runs/vision-v1/best-lora.pt`. **Lesson / next for v2:** templated synthetic labels collapse the model onto the templates; v2 needs (1) the downloaded open footage (real scenes, incl. log; §1a.96 (c), needs the go in chat) as the main data, (2) synthetic frames as a minority with varied label wording and no scene sentence the picture can't support, (3) a far shorter tune (≈ 50 steps, lower lr) with the look fixtures checked at each validation step, (4) a scorer that also flags scene claims absent from the frame (fixtures: `must_not` words for the template scenes). Until then `ai-4` ships with the untuned picture model (15 / 15).
4e. **AI3 85-case regression check 2026-10-16 07:20 EDT (GPU, Pick, synthetic):** was 76 / 87 (AI3 acceptance 85 / 87): the creative `TECHNIQUES` lookup fired on everyday words ("phone footage", "green screen", "laptop", "someone grading") and offered tracking / screen-replacement steps. Triggers tightened (no "phone" / "laptop" / "someone"; "green / blue screen" isn't a screen) → 80 / 87, creative coverage 555 → 557 / 1550. Four cases answered rightly from sections their lists didn't name (J/L-cut note, look node, match recipe, skin recipe; answers read, must-facts present): accepted → **84 / 87, 0 invented**. **2026-10-16 07:26 EDT: `adv-gamut` fixed** (the gamut note's `when:` names clipped / purple lights; GPU re-check passes) → **85 / 87** (= AI3 acceptance). **Owed:** `adv-lumetri-vs-nodes` (answers from `color.lumetri.order`, no trade-offs); `adv-mixed-light` failed before too.
4f. **Full held-out pool eval 2026-10-16 08:09 EDT (GPU, Pick, 36 min, synthetic): 664 / 1454** — lookups **622 / 846 (74 %)**, creative **42 / 608 (7 %)**; 3 "invented" cases were false positives from the guide's own JSON example (`[{"clips": […]}]` parsed as a citation in `color.matching.same-grade`): reworded, re-check 0 invented. Fails: 537 creative plans citing < 2 sections (retrieval cap, Q45), 179 lookups without an expected source.

**AI4 acceptance status (2026-10-16 08:09 EDT):** full pool scored ✓ (above); picture fixtures 15 / 15, 0 invented ✓ (stricter `must_not`); AI3 set 85 / 87, no regression ✓; look ≈ 1.5 s once the model is loaded on the RX 6700 XT (15 fixture looks 33 s incl. load), cached looks free ✓ (borderline: measure first-look-per-shot exactly at the acceptance run); corrupt / missing model → error, no crash ✓ (test); item 3 tune done and rejected by the keep-only-if-better rule ✓ (v2 needs the open-footage download, §1a.96 (c)); item 5 (b) untuned (Q46 download). **Next run: decide the `ai-4` acceptance** (ci, tag, build; `ai-3` publish hold may cover `ai-4` too: check `.fork-publish-hold`), then AI5.
4d. **Item 5 (b) assistant tune: owed a download** (SmolLM3-3B full weights, ≈ 6.2 GB, Apache-2.0; only the Q4_K GGUF is on disk) → OWNER-QUESTIONS **Q46** (with the provisional Pick-style training design). Not blocking: `ai-4` may ship untuned per item 5 (b).
5. (from the GPU work, 2026-10-16 04:12 EDT) Re-measure integrated GPUs now that the matvec is ~11× faster on Intel (1754 → 160 ms / token): explainer models and the picture model, Intel RKL / UHD 770 (Dell) vs the CPU; change the discrete-only policy (`infer::load_gpu`, `vl::load_gpu_vision`) only where the GPU wins. Then optional: one shared `GpuContext` for encoder + text model, fused small kernels (~3 ms / token).
Note on item 4: with a look now ≈ 1.5 s on a discrete GPU, the generic per-shot cache mainly helps CPU-only machines; design it as "reuse the last look of this frame for a follow-up that isn't a new picture question" before building.
Open owner question: **Q42** (CPU torch venv for the exact reference check; provisional: none).
MINIMAL-sized meanwhile: guide notes the lane asks for (HANDOFF CREATIVE-POOL line), CLOUD-QUEUE
16 with the separate-index idea (findings in the item), reviews of today's AI4.2 diffs (Opus when
the mode allows: `vision.rs`, `convert.rs`, `forward_multimodal`, the assist wiring).

### Detail of the 2026-10-16 00:15 – 02:17 run

- **CPU steps that are ready (MINIMAL is fine):** guide / practice notes for whatever the lane's
  "practice notes wanted" list in the CREATIVE-POOL line asks for (check the effect catalogue and
  `docs/guide/effect-reference.md` before writing any "not yet"); when a guide change moves
  `CREATIVE_COVERAGE_FLOOR` in `ai_pool.rs`, set it to the measured count in the same commit with
  the reason; retrieval ideas are measured on all creative batches, never one.
- **AI4.2 code started (2026-10-16 00:20 EDT):** `crates/llm/src/safetensors.rs` reads and checks safetensors headers / the sharded index (the downloaded model: 713 tensors, 315 vision). BF16 / F32 tensor reads ✓ (2026-10-16 00:20 EDT, `read_f32` with an element cap). Preprocessing ✓ (2026-10-16 00:22 EDT, `vision_prep::target_size` / `patches`; resize itself still to choose: bicubic like the reference, or FilmCraft's Lanczos, decide with the parity check). Next: the SigLIP-style encoder design (STEADY+): patch embed (Conv3d as matmul, temporal patch 2 → duplicate the still frame), learned position table interpolated to the grid, 24 blocks with 2-D rotary attention, the 2×2 merger and DeepStack taps; parity oracle (2026-10-16 00:23 EDT): **candle-transformers 0.9.2 (already the `llm` crate's dev-dependency, MIT / Apache-2.0) has `models::qwen3_vl`**, no new download needed; but its `vision` module is private (only `Qwen3VLModel::forward` → logits is public, loading `model.visual.*` from the same safetensors). Options: copy its `vision.rs` (+ `conv3d_temporal_2.rs`) into a test-only oracle module with attribution for per-block parity, or compare end-to-end logits. Neither checks the processor's patch order (candle takes patches as given): that needs one comparison with the reference processor's output (owner question if it means installing Python packages).
- **GPU steps owed (STEADY+):** the daily eval (`ASSIST_EVAL_SET=daily`), the creative eval
  (`ASSIST_EVAL_SET=creative`, scored as plans), `context: "auto"` on the 85-case set, then AI4 item 2
  (Qwen3-VL 4B, **downloaded 2026-10-16 00:17 EDT** to `~/.local/share/filmcraft-fork/models/qwen3-vl-4b-instruct/`, SHA-256-verified. Vision: 24 layers, hidden 1024, 16 heads, MLP 4096, gelu-tanh, patch 16, temporal patch 2, 2×2 spatial merge → 2560, learned position table 2304, DeepStack taps at layers 5 / 11 / 17; preprocessing mean / std 0.5, 65 536 – 16.7 M pixels. Text: 36 layers, hidden 2560, 32 q / 8 kv heads, head 128, MLP 9728, interleaved M-RoPE sections [24, 20, 20], tied embeddings, vocab 151 936) and the planned pickable "effects named in the
  question" step in the pick prompt.
- **Oracle ✓ (2026-10-16 00:39 EDT):** `crates/llm/tests/qwen3vl_vision.rs` + `tests/qwen3vl_oracle/` (candle 0.9.2 copy, attribution) loads `model.visual.*` via our `safetensors::read_f32` (F32 VarBuilder) and encodes a 64×64 gradient: 4 tokens × 2560, 3 DeepStack taps, finite, sum 59.68; our `vision_config::parse` matches candle's. Run with `FILMCRAFT_QWEN3VL_DIR=~/.local/share/filmcraft-fork/models/qwen3-vl-4b-instruct` (`--release`). **Own CPU encoder ✓ (2026-10-16 00:45 EDT):** `filmcraft_llm::vision::VisionEncoder::{load, encode}` (shape-checked loading, attention one query row at a time, rows parallel with `infer`) matches the oracle: max |diff| / max |oracle| 1.7e-4 tokens, ≤ 1.1e-3 taps; with the oracle's tanh GELU in the merger it is 3e-6 everywhere, so the whole gap is candle using tanh GELU where the reference model uses the exact (erf) one (ours follows the reference). (a) ✓ 2026-10-16 00:49 EDT: `vision_prep::resize_bicubic` (a = -0.5, widened when shrinking; provisional vs Lanczos until the reference-processor check) and `VisionEncoder::encode_image(rgb, w, h)`; (b) ✓ timed: **1024 × 768 → 768 tokens in 145 s on the CPU** (12 threads, plain f32 loops; `FILMCRAFT_QWEN3VL_TIME=1` in `tests/qwen3vl_vision.rs`, synthetic): far too slow for use, so **next = GPU kernels for the encoder (STEADY+; reuse `gpu_matmul.wgsl` f32 paths where possible; parity against `VisionEncoder::encode`)**, CPU interim ✓ 2026-10-16 00:51 EDT: 16-row tiles in the matmul and attention (weights / keys read once per tile) → **37 s** for the same picture, parity unchanged; run-time SIMD ✓ 2026-10-16 00:55 EDT (`pulp` `WithSimd` tiles, FMA dot; `FILMCRAFT_LLM_SIMD=baseline` honoured) → **25.6 s**; GPU is the real fix; (c) the text side: **design note `docs/fork/DESIGN-AI4-TEXT.md` (2026-10-16 00:56 EDT)**: candle 0.9.2's Qwen3-VL text side is NOT an oracle (no M-RoPE; causal mask only built when seqlen ≤ 1, so prefill is unmasked); use candle's dense `qwen3` on the renamed `model.language_model.*` weights for text-only parity, unit tests for M-RoPE (confirm the interleave rule against the reference first), behavioural checks for pictures; steps 1–7 there (weights: BF16 → cached GGUF via `llm_quantize`, recommended). Steps 1, 3, 4 are MINIMAL-sized; **step 4 ✓ 2026-10-16 00:59 EDT** (`filmcraft_llm::mrope::{positions, angles}`, rules confirmed from the published transformers source, 4 tests); **step 3 ✓ 2026-10-16 01:01 EDT**: `forward.rs` runs GGUF arch `qwen3` on the CPU (`Config::{q_dim, qkv_bias, qk_norm}`: heads wider than embd / heads via `attention.key_length`, per-head q / k RMSNorm before the rotary embedding, no qkv bias), matching candle's `quantized_qwen3` token by token on a tiny model; **the GPU forward refuses q / k-norm models for now** (`GpuForward::with_context` error → CPU), GPU support = a q / k-norm pass + q width ≠ embd in `gpu_forward.rs` (STEADY+, shader). **steps 1 + 2 + 5 ✓ 2026-10-16 01:05 EDT**: `llm_quantize` converts Qwen3 / Qwen3-VL (nested `text_config`, sharded index, `model.language_model.*`, q / k norms, `key_length` metadata; picture encoder left in the safetensors): `~/.local/share/filmcraft-fork/models/qwen3-vl-4b-instruct/text-q8_0.gguf` (4 274 472 576 B, sha256 91541bf4…, 26 s; Q8_0 chosen provisionally for parity, Q4_K_M later for size). `tests/qwen3vl_text.rs` (env-gated): our forward on it vs candle `quantized_qwen3`: same next token ("Paris"), max |diff| 0.39 on logits up to 40.5. GGUF per-tensor cap raised 2^28 → 2^29 (the 151 936 × 2560 embedding). **Steps 6 + 7 ✓ 2026-10-16 01:21 EDT: FilmCraft's own Qwen3-VL answers questions about pictures (CPU).** `Forward::forward_multimodal(toks: &[MmToken], positions, sections, Some(MmPicture{tokens, deepstack}))`: picture tokens take the encoder's rows, per-token M-RoPE angles from `mrope.rs`, DeepStack taps added after layers 0–2, all inputs checked before anything is cached; text-only calls continue the conversation. `tests/qwen3vl_text.rs::describes_a_synthetic_picture` (env-gated, `--test-threads=1`, each test ~10 GB): red / blue / green square on white → "Red" / "Blue" / "Green"; 64 picture tokens, encode 1.4 s + answer 2.2 s each (synthetic). **`filmcraft_llm::vl::VisionLanguage` ✓ 2026-10-16 01:36 EDT** (`load(dir, gguf)`, `ask(rgb, w, h, max_pixels, question, max_new, cancel)`: sanitized question, Qwen3-VL chat layout, greedy, stops at `<|im_end|>`; `VisionOutput` now carries the merged grid `rows × cols`; `encode_image_within` caps pixels: 2^18 → 64–112 tokens, 3.5 s square / 5.8 s 448 × 256 on the CPU). **In-app converter ✓ 2026-10-16 01:44 EDT** (`filmcraft_llm::convert::qwen3vl_text_to_gguf(dir, out, cancel)`: pure Rust, one tensor at a time, Q8_0 matrices / F32 vectors, `.part` then rename; the real release → `text-own-q8_0.gguf` 4 274 472 576 B in 24 s, same size and same results as the candle-made file; provisional choice: convert on first use rather than download a pre-converted file (no new download, everything local); Q4_K: ✓ 2026-10-16 02:17 EDT `Quant::Q4KMix` (Q4_K, Q8_0 for the more-bits tensors + output head): `text-own-q4k.gguf` 2 910 566 048 B in 98 s, all real checks pass (colours, "Paris", 1.1 % from candle); the app still converts to Q8_0 (provisional: tighter, 24 s); switch `VISION_TEXT_GGUF` + quant in `ai_explain.rs` for 16 GB machines if memory bites). **Next (STEADY+ preferred):** (i) wire it as a picture-capable assistant model (first use: convert with progress + cancel, then `VisionLanguage`); frame grab ✓ 2026-10-16 01:47 EDT (`grade_tools::program_frame_rgb(s, max_pixels)`: Program frame under the playhead rendered small, display RGB 0..=1, proxies allowed, None while loading; test `the_program_frame_for_the_vision_model_is_small_display_rgb`); **(i) wired ✓ 2026-10-16 02:00 EDT**: `coach.assist` with `context: "auto"` and a picture question (the measured facts' topics: `picture_note`) renders the frame (`LOOK_PIXELS` 2^18) when a looker exists; the assistant thread asks `PictureLooker::look` with `look_prompt(question)` (item 2b) and puts the reply in the prompt as `Picture (seen):` (free and pick styles); failure / panic → no line. Real looker (`ai_explain::real_looker`, feature `llm`): `<models>/qwen3-vl-4b-instruct/`, converts `text-own-q8_0.gguf` on first use, loads per look, drops after. Real engine test (ignored, `FILMCRAFT_DATA_DIR=~/.local/share/filmcraft-fork`): demo frame, "is the sky too blue?" → "The sky is not blue; it transitions from deep purple at the top to orange near the horizon … sunset or sunrise … exterior …" in 28 s (CPU). Guide `color.coach` paragraph. **Not yet:** per-shot cache of the generic description, a model download / install entry (Settings) and an install check in the UI, GPU (the 28 s), AI-POOL `needs_clip` scoring of looks (item 2b acceptance) (catalogue entry pointing at the cached GGUF + safetensors encoder, `infer.rs` prompt with picture spans, the `coach.assist` picture input AI4 item 2 asks for: what frame / still is sent, at what size); (ii) GPU: vision encoder f32 matmul + q / k norm and q width ≠ embd in `gpu_forward.rs`; (iii) ~~Q4_K_M conversion~~ ✓ 2026-10-16 01:25 EDT (`text-q4k.gguf`, 2 703 039 680 B, sha256 009a041d…, Q4_K + Q6_K mix with a Q6_K output head copy as for Qwen2; colours right, "Paris", logits within 2.2 % of candle; select with `FILMCRAFT_QWEN3VL_GGUF=text-q4k.gguf`): use it as the default once wired; (iv) exact reference check if Q42 is answered (a). patch-order check against the reference processor still owed: **script ready** (`tools/qwen3vl-ref/make_ref.py`), needs a CPU torch ≥ 2.6 + Pillow venv = OWNER-QUESTIONS **Q42** (download; provisional: none). If answered (a): make the venv in `~/Apps/filmcraft-builds/qwen3vl-ref-venv`, run the script, add an env-gated test comparing `vision_prep::patches` + `VisionEncoder::encode` to its files (2026-10-16 00:58 EDT). Was: a test-only oracle module copying candle 0.9.2's `qwen3_vl/vision.rs` + `conv3d_temporal_2.rs` (MIT / Apache-2.0, attribution header) that loads `model.visual.*` from the downloaded safetensors, then our own CPU encoder in `crates/llm` (patch embed, interpolated positions, 24 blocks with 2-D rotary attention, merger, DeepStack taps) block by block against it; GPU kernels after.
- Run ends only for the reasons in FORK-PLAN §1a.92.

### Earlier (done): §1a.79 order: (1) H group acceptance (owed PC checks → tags h264-1/2/3 → build → publish), (2) AI3 to acceptance (GPU inference, STEADY+), (3) `.fork-paused`

**2026-10-15 05:50 (scheduled run):** H group in progress: `ffmpeg_free` on the RX 6700 XT and H2 perf on the i5-11600K
running; then in-app 1080i / 4K 4:2:2 playback on both GPUs (ffmpeg hidden), tags, build, publish. AI3 notes below stay valid. **Done 05:55:** H3 ffmpeg_free on AMD ✓, H2 perf ✓, in-app 1080i zero drops on AMD + Intel ✓, 4K 4:2:2 H.264 plays at
9–24 / 25 fps (recorded; proxies recommended; burstiness → Open problems). Done since: A2 review + fixes `a8c708b9`, ci (AMD VCN fault, Q37; Intel + Dell green), tags
`h264-1/2/3` + `audio-2`. **(1) done** (build h264-3, `latest` → h264-3, public release `fork-h264-3`). **Now (2): AI3 to acceptance** — **GPU inference done** (lanes A, B, D merged; `f710a39d`): answer median 2.4 s, first words median 0.90 s (Q38),
83 / 85 auto-pass, 0 invented; guide names every command (`94d91587`); Dell CPU time recorded. **Open accept item:** must-fact
completeness 57 / 85 full (need ≥ 73): **lane E running since ~11:40** (Opus worktree, `target/agent-ai3e`: recipe-step
expansion, key facts first, synonyms for 4 misses, padding filter). Then: independent Sonnet re-read → ci (AMD VCN fault
Q37: set `FILMCRAFT_VAAPI_DEVICE=/dev/dri/renderD128` or reboot first) → tag `ai-3` → build → publish → `.fork-paused`
(§1a.79 (3)). If this run died: review + merge their branches if sound, else note them (GPU inference: see "AI3.3b GPU step 1" below for steps (b)–(e); Opus lanes per §1a.79 notes).

**2026-10-15 03:58 (scheduled run, LEAN):** the owed C1 GPU-comp review and U0.2 review are done (fixes + tests, leftovers
listed under Status › Owed). Token-light leftovers are now thin. **Next = GPU (wgpu) inference for AI3.3b**: step 1 (Q4_K matvec) ✓ 05:45 in MINIMAL, see "AI3.3b GPU step 1" below for
steps (a)–(e); the bigger ones (attention, forward path) best once the mode is
STEADY or better (Opus / Fable designer + builder per §1a.44); CLOUD-QUEUE item 14 (AI3-GPU-PREP) asks the cloud for the
design prep `docs/fork/PREP-AI3-GPU.md`: read it first if it has landed. In LEAN / MINIMAL meanwhile: the in-app AMD + Intel
look at a comp reading an F-Log2 clip (C1 owed); ~~C2's design note~~ ✓ 05:21 (`docs/fork/DESIGN-C2.md`) (`vaSyncSurface` outside submits ✓ 2026-10-15 04:10). Comp mask Bézier handles ✓ 2026-10-15 04:27 EDT; Ctrl-click add / delete points on comp masks ✓ 2026-10-15 04:32 EDT.

**Order fix 2026-10-14 16:50 (scheduled run):** this section said "C2 next", but FORK-PLAN §1a.74 puts
**AI3 between C1 and C2** (… C1 → AI3 → C2 → AI2 …). AI3 is the current phase; the C2 plan below waits.
AI3-PREP (items 0a–0d, 6) is done on the Dell (see above). **Main-runner step map** (AI-PLAN items 1–6):
- **AI3.2a ✓** (2026-10-14): `coach_ask::retrieve(question, k)` (≤ `MAX_RETRIEVE` 12): a second BM25 index
  over **every** guide page (NOT_INDEXED practice / general pages included; README.md left out) that also
  searches each section's compact header (`does:` ×2, `when` / `resolve` / `premiere`); the coach's own index
  is unchanged (colour eval floor untouched). `retrieval_finds_the_advanced_sets_sources`: **recall@5 60 / 63**
  on advanced.toml (floor 60; misses `adv-grain-where`, `adv-shot-match-limits`, `adv-black-point`),
  `retrieval_survives_hostile_questions`.
- **AI3.2b ✓** (2026-10-14): `coach_assist::assistant_prompt(question, &AssistContext, budget_chars)` → `AssistPrompt { system, user, sources }` (6 sections, best 2 with full text when they fit, others header + steps; budget in chars, default 12 000, max 64 000; markers sanitised; 3 tests). Token-exact budgeting waits for AI3.3 (the model's tokenizer).
- **AI3.2c ✓** (2026-10-14): heading-word boost in `retrieve` (`ASSIST_HEADING_MATCH` 0.5); `adv-grain-where` also accepts `color.nodes.starter-tree` (it states grain goes on the last node); `practice.grading.black-white-points` header `when:` names blacks / whites. **recall@5 62 / 63** (floor 62; `adv-black-point` "how far down should my blacks go?" still misses, left honest).
- **AI3.4a ✓** (2026-10-14, done before AI3.3 because it needs no model): `coach_ask::ground(answer, &sources, &Checks) -> Grounded` checks a model answer line by line (a line naming an unknown control / a command-namespace id that isn't registered is dropped, ids listed; `[id]` citations of sections not sent are taken out as `invented_citations`; `General advice:` lines marked; ≤ 80 lines / 16 000 chars); 2 tests. Sources list in the UI ✓ (AI3.5c). **"I don't know" ✓ (2026-10-14 18:58 EDT):** no section found → `AssistFallback::NothingFound` (model not asked); the model saying the sections don't cover it → `AssistAnswer::dont_know` / JSON `dontKnow` (lines kept, keyword coach's answer alongside, UI note `coach.ask.assistDontKnow`); tests `the_assistant_says_when_it_does_not_know` + the box test.
- **AI3.3a ✓** (2026-10-14): **SmolLM3-3B runs on our runtime.** `Family::SmolLm3` (GGUF arch `smollm3`;
  `Config::no_rope_step` 4: layers 3, 7, 11 … skip the rotary embedding, as llama.cpp; interleaved rope like
  `llama`; tied output; end-of-text `<|end_of_text|>`), `chat_prompt` follows the published template in
  `/no_think` mode (metadata header, **no `<|im_end|>` after the system block when there are no tools** —
  that is what the template writes —, empty think block; "Today Date" left out), catalogue entry
  `smollm3-3b-q4km` pinned to `ggml-org/SmolLM3-3B-GGUF@4965cb60` Q4_K_M (1 915 305 312 B, sha256 verified)
  + base-repo tokenizer `@a07cc9a0`. Test `smollm3_skips_the_rotary_embedding_on_its_nope_layers`. **Real
  model** (`tests/model.rs` with `FILMCRAFT_LLM_DIR=target/ai3-models/root`, files in
  `target/ai3-models/smollm3-3b/`, never committed): coherent answer, verify 1.3 s, load 1.1 s, **~5.9 tok/s**
  generation, prefill 981 tokens in 24.4 s (~40 tok/s), first words 1.8 s with the system prompt cached
  (release build, this PC, synthetic). Guide `color/matching.md` names the model.
- **AI3.3c ✓** (2026-10-14, ahead of 3b): `coach_assist::answer(backend, model, question, ctx, checks) ->
  AssistAnswer` (blocking; run off the UI thread): prompt → `ExplainBackend::generate` (400 tokens, 90 s) →
  `ground`; the keyword coach answers with `AssistFallback::{NotReady, Error, Ungrounded}` otherwise. Test with
  a scripted backend.
- **AI3.3d ✓** (2026-10-14): commands **`coach.assist {question, page?, selection?, gradeFacts?, model?}`** →
  `{id, model}` and **`coach.assistResult {id, wait?}`** → `{state: pending|done|unknown, source: model|coach,
  sources, lines[{text, general, ui, commands}], cited, removedLines, removedIds, inventedCitations, fallback,
  coach}`. `Session::assist: coach_assist::AssistJobs` (background thread `filmcraft-assist` under
  `catch_unwind`, one model answer at a time, ≤ `MAX_PENDING` 4 waiting, 32 kept; command ids snapshotted for
  the worker; no backend (no `llm` feature / wasm) → the coach answers at once). Model: `ASSIST_MODEL`
  `smollm3-3b-q4km` or `model` param (an "Assistant model" pref comes with the UI). Test
  `coach_assist_commands_answer_in_the_background`. **Streaming ✓** (2026-10-14 evening): `answer_streaming`, pending results carry `partial` (unchecked text so
  far; a stop → coach fallback; an evicted question stops the model). **Forget ✓:** `coach.assistForget {id}` stops / drops an answer. **Not yet:** a real-model end-to-end run of
  `coach.assist` (do it with AI3.3b's smaller budget).
- **AI3.4b ✓** (2026-10-14): `coach_ask::ground_with_text(answer, sources, sent_text, checks)`: in a line with
  an action word (click / press / open / select / find / use …) every `**bold**` / "quoted" label, and every
  shortcut (`Ctrl+N`) anywhere, must appear in the guide text sent (`AssistPrompt::sent_text`), else the line
  goes; `answer` uses it. System prompt: "not in FilmCraft yet → say so, no steps"; name controls only as the
  sections write them. Test from the real hallucination. Ignored real-model test
  `real_assistant_answers_advanced_questions` (`ASSIST_BUDGET`, `FILMCRAFT_LLM_DIR`).
- **Real-model findings (SmolLM3-3B Q4_K_M, budget 3 500 chars ≈ 1 000 tokens, 400 max new tokens,
  2026-10-14, synthetic):** 48–70 s per answer (target < 10 s); answers are long (300–400 tokens); **no
  citations** in 2 of 3; on the gap question "how do I add a parallel node like in Resolve?" it **invents the
  feature** ("Parallel Mixer", `Ctrl+N`, a "+" button, File ▸ New Project ▸ "Node Tree") even with the rule;
  the label check removes 4 lines, the rest (unlabelled steps) survives. **So:** (1) gap questions need a
  deterministic route before the model — if the best sections say "not yet" / "not available" for the
  question's topic, answer from the coach's `not-yet` route or state it up front in the user message;
  (2) shorter answers (max ~150 tokens, "at most 6 steps") and streaming; (3) the eval (AI3.6) will likely
  need the tune on training-pairs.jsonl (human review sample first → OWNER-QUESTIONS when it starts);
  (4) a Fable / Opus design pass on (1)+(3) when the mode allows (MINIMAL today).
- **AI3.4c ✓** (2026-10-14): gap route. `assistant_prompt` finds a gap in the **best** section (`gap_line`: a
  "**FilmCraft:** … no / not … yet" first clause, or "Planned: …") → `AssistPrompt::gap`, an "Important, what
  FilmCraft doesn't have yet [id]: …" note before the question; `answer` caps it at `GAP_ANSWER_TOKENS` 150
  and opens with the guide's gap line + citation when the model didn't say it. On advanced.toml: **6 / 7 gap
  questions found, 1 other hit** (`adv-bleach-bypass`, a real partial gap); top-2 / top-3 gave harmful false
  hits (adjustment layer, reference look). Tests `gap_questions_are_told_…`, `gap_detection_matches_…`.
- **AI3.3b part 1 ✓** (2026-10-14): shorter by default: `DEFAULT_BUDGET_CHARS` 4 500 (≈ 1 100 tokens),
  `FULL_TEXT` 1, `MAX_ANSWER_TOKENS` 200, system prompt "at most 6 steps, no headings". Estimate on this PC:
  ~25 s prefill + ~33 s answer: still far from the acceptance (first words < 1 s, typical answer < 10 s).
  **The numbers say CPU is too slow for 3B** (AI-PLAN item 3: "GPU inference (wgpu) only if CPU is too slow
  for a 3–4B model: a separate, bigger step, decide on the numbers"): runner's call, **next big step = wgpu
  inference for the Q4_K / Q6_K matmuls** (RX 6700 XT / Intel; CPU stays the fallback), a Fable designer +
  builder per §1a.44 once the mode allows (STEADY or better); in MINIMAL keep to AI3.5 UI / streaming work.
  Also cheap first: re-measure with the new defaults (`real_assistant_answers_advanced_questions`).
- **AI3.5c ✓** (2026-10-14 evening): **chat UI, first part.** The Ask the coach box has a
  **Also ask the assistant (slow: runs on this computer)** checkbox (`coach.ask.assistant`, off by
  default, provisional until GPU inference): the keyword coach answers at once, then `coach.assist`
  runs; the box polls `coach.assistResult` each frame (repaint 250 ms) and shows the partial text
  ("Not checked against the guide yet.") with **Stop** (`coach.ask.assistStop` → `coach.assistForget`),
  then the grounded lines (`coach.ask.assistLine.{n}`, "General advice" dimmer), **Sources** (cited, else
  given) and lines left out; a fallback says why in one line. Closing / unticking / a new question stops
  it. Test `coach_ask_box_shows_the_assistant_answer_or_why_not` (scripted backend); es / zh-cn strings;
  guide `color/coach.md` › "Also ask the assistant". Side fix: the new section flipped a near-tie in the
  coach eval (`owner-1-para-d` at the floor 94), fixed guide-side ("Start here when grading any log clip"
  in `recipes.md` › 1). **Still owed for AI3.5:** follow-up questions (conversation context),
  "Show me" → lesson from an assistant answer, an "Assistant model" choice. **AI3.5d ✓:** the checkbox is the
  preference `color.coach.assistant` (default off, lenient: a bad value is off; also Settings ▸ Color ▸ Grading Coach).
  **AI3.5e ✓ follow-ups:** `coach.assist` takes `previousQuestion` / `previousAnswer` (each ≤ `MAX_PREVIOUS_CHARS`
  800, sanitised; "Earlier in this conversation" block before the sections; retrieval reads both questions);
  the box remembers the last checked model exchange (`CoachAsk::last_exchange`) until **New topic**
  (`coach.ask.newTopic`), close or untick. Test `a_follow_up_carries_the_earlier_exchange`. **AI3.5 left:**
  "Show me" ✓ (2026-10-14 19:06 EDT, `coach_ask::lesson_for`, `coach.ask.assistLesson`), an "Assistant model"
  choice (with more models), a real-model end-to-end run of the box (with GPU inference).
- **AI3.6a ✓ (2026-10-14 19:11 EDT): eval scorer.** `coach_assist::{parse_eval_set, score_case, summarize}`: per case
  `model_answered` (not the coach / "I don't know"), `cites_source` (a cited id among the case's `sources`), `invented`
  (removed lines + invented citations: any one fails), `states_gap` for `gap` cases; `auto_pass` is an upper bound
  (the `must` facts come back in `review` for a reader). Test `eval_scores_sources_inventions_and_gaps`; ignored
  runner `real_assistant_eval_on_the_advanced_set` (`ASSIST_EVAL_LIMIT`, writes `target/ai3-eval.json`; ~1 min/case
  on CPU, run on an idle machine or the Dell as a soak). **AI3.6 left:** the `must`-fact check (reader or a judge
  pass), the run itself (best after GPU inference), tune only if the 85 % floor is missed.
  **Baseline run 2026-10-14 20:18 EDT (SmolLM3-3B Q4_K_M, CPU, default budget, synthetic): 7 / 85 auto-pass
  (8 %)**, 19 cite an expected source, 60 had lines / citations removed (real inventions, e.g. "use a Key-Mixer node"
  on the parallel-node gap), 20–57 s per answer, 63 min total. Floor 85 % → far off: **OWNER-QUESTIONS Q35** (tune on
  training-pairs.jsonl needs the owner's sample review; provisional: assistant stays off by default, next = GPU
  inference, meanwhile try (b) extractive answers: the model picks / orders / shortens guide steps only). Report
  `target/ai3-eval.json` (not committed; carries `removedIds` / `inventedCitations` from the next run on).
- **AI3.6d ✓ (2026-10-14 20:39 EDT): pick-the-steps style** (Q35 (b)): `AssistStyle::Pick` / `coach.assist {style: "pick"}`, default
  still `free`; the model only names step keys, the answer is guide text word for word (no inventions by design, ~40
  tokens out instead of 200). **Full eval ({D}, synthetic): 69 / 85 (81 %), 0 invented** (free: 7 / 85) →
  pick is now `DEFAULT_STYLE` of `coach.assist` / the box (Q35 update; `style: "free"` kept). Next cheap steps for
  the 85 % floor (report `target/ai3-eval.json`, not committed): the 4 gap questions that don't state the gap
  (in pick mode the gap line is only prepended when the reply picks nothing or the first line lacks it: check
  `adv-outside-node` etc.), the 10 without an expected source (retrieval / which sections fit the budget), the 4
  "NONE". Then GPU inference (speed) as before.
  **AI3.6e ✓ ({D}):** scorer accepts "no … yet"; bullet lists split into steps; **fresh run 76 / 85 (89.4 %), 0
  invented** (synthetic) → floor met on the automatic checks, no tune needed (Q35 update). Left for AI3.6: the `must`
  facts read (a reader / judge pass over `target/ai3-eval.json`, not committed). **Next big step: GPU (wgpu) inference
  for AI3.3b** (Fable / Opus designer once the mode is STEADY or better). **`must`-fact read ✓ (2026-10-14 22:30 EDT,
  [`coach-eval/AI3-MUST-READ.md`](coach-eval/AI3-MUST-READ.md)): 53 / 85 (62 %) state every `must` fact, 73 / 85 (86 %)
  counting partial answers**, 3 auto-passes miss the point; ~15 answers padded with off-topic picks (the model picks
  runs of keys from the top sections). A prompt line against padding was tried (73 / 85, padding unchanged) and
  reverted; the eval runner now defaults to `DEFAULT_STYLE` (pick). **Padding filter ✓ (2026-10-15):**
  `PICK_MIN_RELEVANCE` 0.5 in `parse_picks` (each `PickStep` carries its section's score / best); replayed on the
  22:14 answers: 39 / 322 lines dropped, no auto-pass lost (0.6 lost two). `adv-export-size` retrieval fixed guide-side (header `when:`). In MINIMAL meanwhile: the remaining misses (+ ~~`adv-edit-tighten-cut`~~ ✓ 2026-10-15: synonym `late` → trim roll ripple: editing.trimming rank 3 → 1, recall@1 70 / 85, floor raised; `adv-qualifier-chatter` looked at 2026-10-15: the answer sits in `color/secondaries.md` › Tips, the guide's only `### Tips` block, which pick mode doesn't offer (numbered steps only, ≤ `PICK_STEPS` 8, already full there); the `practice.grading.qualify-skin` pitfall bullet was offered and not picked: a model limit, left as is) (~~adv-film-look~~ ✓ 2026-10-15: a **quoted** header `when:` example question whose words are all in the question ranks its section ×1.5 (`ASSIST_WHEN_MATCH`; unquoted `when:` text is a description and isn't boosted: "exporting HDR", "interviews and documentaries" stole rank 1 when tried); `practice.looks.film-curve` rank 2 → 1, recall@1 65 → 66 / 85 (new floor `RETRIEVAL_FLOOR_FIRST`), recall@5 85 / 85; pick prompt now offers its toe / shoulder step first; not re-run on the model; adv-lut-wrong-camera looked at 2026-10-15: its answer (normalise-log A4 / A6 pitfall) was offered as rank 1 and the model replied NONE, a model limit; ~~adv-aces~~ ✓ 2026-10-15: new guide section `practice.delivery.aces` (gap in the first clause, so the gap note fires; quoted `when:`), added to the case's sources, ~~adv-grain-where~~ ✓ 2026-10-15 (`practice.grading.grain-finish` `does:` now says "last node of the tree, after the look": not retrieved → rank 1; recall@1 67 / 85, floor raised; the model had picked the glossary although starter-tree B6 was offered), ~~adv-check-scene~~ ✓ 2026-10-15 (`numbered_list` bug: a wrapped line "100. **Check scene** …" in color/matching.md was taken as the list, so pick mode offered one prose line instead of steps 1–3; now a list interrupting a paragraph must start at 1, as CommonMark; test `a_number_at_the_start_of_a_wrapped_line_is_not_a_step`),
  adv-edit-proxy-export (looked at 2026-10-15: `proxies.what` A1 states "an export always reads the original files", offered first; the model replied NONE — same pattern as adv-lut-wrong-camera: **yes/no questions get NONE although the first step answers them**; idea for the next model run: a PICK_SYSTEM line "a step that answers a yes / no question counts", measure on those two + a few others; glossary › Proxy had a stale "planned for Phase 6" line, fixed), ~~adv-audio-voice-fast~~ ✓ 2026-10-15 (synonyms `cleanup` → clean up, `quickest` → fast quick: audio.voice-isolation / audio.cleanup-voice rank 1 / 2, were 2 / absent; recall@1 69 / 85, floor raised; coach eval 94 / 124 unchanged), ~~adv-gfx-lower-third-series~~ ✓ 2026-10-15 (quoted `when:` "the same lower third in every episode" on graphics.templates: rank 2 → 1, recall@1 71 / 85, floor raised), ~~adv-media-dual-system-sound~~ (2026-10-15: its answer section `practice.editing.sync-sound` ranks 1 and is correct; added to the case's sources; the model still skipped its Merge Clips / Synchronize step A1 and padded with multicam).
- **Model re-check ✓ (2026-10-15 ~01:05 EDT, SmolLM3-3B Q4_K_M, CPU, pick style, synthetic):** the 8 cases fixed on 2026-10-15
  pass 7 / 8 (adv-check-scene answered right from `color.recipes.scene`, now in its sources); the yes / no NONE pattern is fixed by a
  `PICK_SYSTEM` line ("a step that answers a yes / no question by saying what FilmCraft does counts as an answer"): a second run
  of lut-wrong-camera, proxy-export, check-scene, film-look, aces and the gap cases outside-node / parallel-nodes: **7 / 7, 0
  invented**. A full 85-case re-run with the new line is worth doing at the next idle hour (~60 min CPU, few tokens; reports
  `target/ai3-eval.json`, last full run 76 / 85).
  **Full re-run ✓ (2026-10-15 01:19–02:00 EDT, pick style, SmolLM3-3B Q4_K_M, CPU, synthetic): 83 / 85 auto-pass
  (97.6 %), 0 invented, 85 / 85 model-answered**, median 25.6 s per answer (max 90.7 s); misses `adv-mixed-light` and
  `adv-interview-match` (no expected source cited). Report `target/ai3-eval.json` (not committed; previous 76 / 85 run in
  `ai3-eval-prev.json`). Misses looked at (2026-10-15 02:20): `adv-interview-match` answered from `color.recipes.match` (rank 1, a correct how-to) → added to the case's sources, and that recipe's wipe step now says "match skin first"; `adv-mixed-light` retrieves its section at rank 1, the model picked day-for-night bullets instead: a model limit, left honest. **Smaller pick prompt ✓ (2026-10-15 02:15–02:43):** `pick_prompt` leaves out sections below `PICK_MIN_RELEVANCE` (their picks were dropped anyway): 3 682 → 3 064 chars on average (test `pick_prompts_leave_out_sections_whose_picks_would_be_dropped`); full re-run **84 / 85 (98.8 %), 0 invented, median 21.5 s (was 25.6), max 32.4 s (was 90.7), 28 min total (was 40)**, only `adv-mixed-light` misses (synthetic). Still far from < 10 s: GPU inference stays the next big step. The `must`-fact read (AI3-MUST-READ.md) was on the 76 / 85 run; re-reading it is optional.
- **AI3.3b GPU step 1 ✓ (2026-10-15 05:45 EDT, MINIMAL, done in the main run):** `filmcraft-llm` feature `gpu`
  (`crates/llm/src/gpu.rs`, `gpu_matvec.wgsl`): `GpuContext::{new, upload_q4k, upload_q6k, matvec}` → `GpuMatrix`, Q4_K and Q6_K × f32 matvec (Q6_K ✓ 05:55, 212 B
  blocks, d as f32), parity with `dequantize_q4k` / `dequantize_q6k` on AMD + Intel. Design choices made without the PREP note (provisional, cheap to change): a `gpu` feature on
  `filmcraft-llm` rather than a new crate; activations stay f32 on the GPU (no Q8_K); blocks 152 B (scales unpacked).
  **Next GPU steps (small, in order):** ~~(a) Q6_K matvec~~ ✓; (b) time both on a
  SmolLM3-sized matrix (11 008 × 2 048) vs `kquant::matvec_q4k`; per-call buffers → persistent x / y buffers; (c) RMSNorm,
  rope (NoPE layers), SiLU-gate, attention over a GPU KV cache; (d) a `forward.rs` GPU path behind the feature with a
  logits parity test on the real model (`FILMCRAFT_LLM_DIR`); (e) tiled matmul for prefill. CPU stays the fallback.
- **AI3.3b (open):** speed (GPU inference, above; prefill profiling). **Prefill checked 2026-10-14 18:20:** already batched
  (`forward.rs` `PREFILL_CHUNK` 64 → `Weight::matmul` → `quant::rows_at`: rayon tasks of 32 weight rows, AVX2 `dot4_v3`
  over 4 activation rows, each weight row read once per chunk); 981 tokens in 24 s ≈ 240 GFLOP/s effective, so no cheap
  CPU fix left (llama.cpp-class tiling might give ≤ 2–3×, still far from < 1 s): **GPU inference stays the next step**,
  with a Fable / Opus designer once the mode is STEADY or better. In MINIMAL meanwhile: ~~`compositor.md` guide gaps (§1a.75)~~ ✓ 2026-10-14 18:21 (every `comp.*` command documented;
  advanced.toml + 6 compositing questions ✓ (recall@5 68 / 69, floor 68);
  + 8 editing / trimming / audio / export ✓ (misses adv-black-point,
  adv-edit-three-point; audio → sound synonym added; + 8 titles / media / playback / project questions: 80 / 85, floor 80; misses adv-media-send-to-colourist, adv-play-heavy-timeline, adv-proj-move-computer; AI3.2e synonyms 2026-10-14 18:35–18:45: **85 / 85, floor 85**), or the cheap real-model re-measure (`real_assistant_answers_advanced_questions`, needs an idle machine). Acceptance wants first words < 1 s and a typical answer < 10 s: a 3 000-token
  prompt would take ~75 s of prefill at 40 tok/s, so (1) cache the fixed system prompt's KV (already
  cached across calls) and keep the per-question part small (≈ 800 tokens: headers + steps, full text only
  for the top 1; `DEFAULT_BUDGET_CHARS` down to ~3 500), (2) profile the prefill (matmul batch path in
  `weights.rs`; is Q4_K prefill using the batched kernel?), (3) then wire `assistant_prompt` → `Model` in
  `crates/engine` behind the `llm` feature (a `coach.assist` command returning the grounded answer). (Was AI3.2b:) prompt assembly in `crates/engine` (no model yet): `assistant_prompt(question, grade facts,
  on-screen context) -> Prompt` sending header + numbered steps of the top-k first, the full body only for the
  top 1–2, under a token budget (count with `crates/llm` tokenizer when a model is loaded, else chars / 4);
  tests: budget respected, every section id cited, hostile input. Then look at the 3 misses (synonyms or
  header `when:` words, guide-side fixes only if the text really lacks the answer).
- AI3.3 model: SmolLM3-3B (approved §1a.74) on the AI1 runtime (`crates/llm`: gguf / kquant / forward);
  download needs no further yes for SmolLM3; check the architecture (SmolLM3 = Llama-style with NoPE layers)
  against `forward.rs`; tokens/s on this PC. AI3.4 grounding checker (every `{ui:}` / command in an answer
  checked against the live app, unknown ones removed; "general advice" marking). AI3.5 chat UI (follow-ups,
  streaming, Show me → lesson; model-free coach as fallback). AI3.6 eval of model + retrieval on advanced.toml
  (≥ 85 %, no invented controls); tune only if the floor is missed (training-pairs.jsonl needs a human review
  sample first: ask via OWNER-QUESTIONS). AI3.1 / §1a.75: finish `compositor.md` and fill feature-page gaps.
- MINIMAL mode: do it yourself in small steps; with STEADY+ a builder for AI3.3 (decoder state → Fable per §1a.44).

## After AI3: C2 (COMPOSITOR-PLAN, parallel / layer nodes in the grader), step 1

**comp-1 accepted 2026-10-14** (C1: comp clips, node canvas + inspector, Compositor panel / Compositing workspace /
Fusion page, GPU comps via `PlanLayer.comp` + `crates/gpu/src/comp.rs`, Media In tracks as `Pass::Layers`, coach
routes (item 6), `comp.explain` + comp lessons (item 7); bench comp-1080 / comp-2160 half 192 / 0 drops, 30-min soak
clean; all synthetic). The C1 working notes are archived at the end of `docs/fork/PROGRESS-LOG-ARCHIVE.md` ("HANDOFF
notes for C1"). **Cloud work merged 2026-10-14 ~16:45** (USHADER-HOSTILE, R1-PREP, USERFX-FOLDER, GUIDE-SHADER-COOKBOOK; guide recipes render on AMD + Intel); full `cargo xtask ci` owed at the next boundary. TEARDOWN-FLAKE stays open (cloud diagnosis in CLOUD-LOG: render-cache `CacheJob` holds the pool past `CLOSE_WAIT`). **Owed from C1 (not blocking):** ~~an Opus review of `crates/gpu/src/comp.rs` + the `render_plan` comp
hook and `plan::track_layers`~~ ✓ 2026-10-15 03:33 (bug fixed: a later comp's uploads evicted an earlier comp's result from the 24-entry upload cache → it drew transparent; `refresh_comp_output` re-registers every result before the draw, test `a_busy_later_comp_does_not_evict_an_earlier_result`, failed before. **Left from the review:** ~~(2) comp VRAM~~ ✓ 2026-10-15 (`COMP_VRAM_BUDGET` now covers every held slot set, sets a frame no longer uses are dropped, all comp textures go after `COMP_IDLE_FRAMES` 120 comp-free draws; test `comps_share_one_budget_and_their_textures_are_let_go`); ~~(3) effect-pool churn from the nested slot draws~~ ✓ 2026-10-15 (one pool frame per top-level `render_plan`, `fx_frame_open`; test `comp_and_frame_effects_keep_their_pooled_textures`, failed before); ~~(4) `Pass::Image` of another size~~ ✓ 2026-10-15 (such a comp runs on the CPU, test in `a_comp_too_wide_for_a_texture_falls_back_to_the_cpu`); (5) `track_layers` nested `scale` > 1: left as is (2026-10-15: it only changes graphics / captions, which then rasterise at the comp's size, sharper than the CPU's upscale; media frames are already capped at full size; sizes stay within the texture limit)); ~~Item-source Media In still CPU pictures~~ ✓ 2026-10-15 02:56 (media items / subclips: one plan layer via `plan::media_frame_layer`, fitted as `fit`; log / HDR Y'CbCr gets the input transform as layer colour ops; RGB frames needing management, nested sequences and comps stay CPU; tests `an_item_source_compiles_to_a_plan_layer`, `a_media_frame_layer_fits_the_canvas`; owed: an AMD + Intel look at a comp reading an F-Log2 clip in the app); Mask shapes on the monitor have no Bézier handles / add-point; ~~`backdrop` / `post` reallocate when comp and frame sizes alternate~~ ✓ 2026-10-15 (it was `output` + `backdrop`, 2 new targets per frame; comp slot draws now swap in their own `comp_output` / `comp_backdrop`; test `a_comp_of_another_size_leaves_the_frames_targets_alone`, AMD + Intel); ~~the `settings_tests::hardware_decoding_setting_drives_the_decoder_switch`
parallel flake (seen once)~~ ✓ 2026-10-15 (`set_prefs` re-applied the process-wide decoder switch on every `prefs.set`, so another test's unrelated setting turned it back on; now only when Hardware decoding / Memory change; deterministic check in the test); the compositing coach-eval misses (`comp-*` in `docs/fork/coach-eval/questions.toml`): 4 fixed 2026-10-15 (COMP-EVAL), `comp-what-is` left. **Next in MINIMAL** (2026-10-15 03:25): token-light leftovers are thin; GPU (wgpu) inference for AI3.3b wants STEADY (Fable / Opus designer); TEARDOWN-FLAKE needs instrumented 8-copy repro runs of the ui test binary (CLOUD-LOG); ~~mask Bézier handles~~ ✓ 2026-10-15 04:27 EDT (comp mask overlay: smooth points show handles, drag bends (mirrored, Alt breaks), Alt-click smooth ⇄ corner; shared `MaskVertex::set_tangent` / `MaskPath::toggle_smooth`; test in `comp_ui.rs`); Ctrl-click add / delete points ✓ 2026-10-15 04:32 EDT, shared `Mask::add_vertex` / `remove_vertex`) (Item-source Media In ✓ 02:56).

**C2 plan (COMPOSITOR-PLAN §C2):** 1 grade model → DAG (`Grade` keeps node ids, gains inputs per node; serial grades
load unchanged: schema bump + migration test; reuse `crates/project/src/dag.rs` `DagNode` / `validate` / `Order`),
2 node types (Parallel mixer, Layer mixer, Splitter / Combiner, key mixer, key inputs / outputs, Outside node),
3 evaluation (CPU reference graph first, then GPU passes reusing C1's scheduler: `render::comp`'s `Program` / slots
and `gpu::comp::run_comp` are the model; per-node `ColorOp` chains as today), 4 UI (the Color page node strip
becomes `widgets/node_canvas.rs`; add Parallel / Layer / Outside from the menu; Resolve keymap shortcuts from the
public manual), 5 the node explainer (G2.10–11) explains branches, 6 Coach lessons for each branch type (the lesson
engine + walk tests as `first-comp` / `key-comp` do: `crates/ui-egui/tests/ui/coach_ui.rs` `walk` with
`Do::WorkspaceNamed` / `{comp}`). **Design note written 2026-10-15 05:21 EDT: `docs/fork/DESIGN-C2.md`** (edges + node `kind` on `Grade`, serial = implicit
chain and compiles exactly as today, virtual source id 0, schema v19, `FxOp::GradeGraph` program mirroring C1's, CPU oracle,
lanes); ADR 0004 / 0007 point to it. Start at its §5 lane 1 (model + schema v19). In MINIMAL mode do it yourself; with STEADY+ a Fable / Opus designer for the note and
builders per lane. **Accept:** each new lesson completes end to end; skin + background on parallel nodes, a look on a
layer node and an outside node render identically on CPU and GPU, round-trip save / load, and graded 6.2K plays in
the quick pass without regression. Tag `comp-2`.

## Environment and gotchas

- **Disk rule (FORK-PLAN §1a.12):** keep ≥ 20 GB free on `/home`; `df -h /home` at session start
  and after each build; clean `target/debug/incremental` / `target/debug` / stale build dirs in
  the repo when needed; never delete `target/fixtures`, `target/fork-bench`, `target/bench`,
  `~/Apps/filmcraft-builds` (keep `target/release`: the MCP config runs
  `target/release/filmcraft-cli`); can't stay above 20 GB → reason in `.fork-plan-blocked`, stop.
  2026-10-08 after cleanup: 182 GB free (`target/debug` deleted; `target/` 31 GB).

- **AI explainer models (G2.11, owner-approved downloads 2026-10-11):** GGUF candidates +
  tokenizers in `~/.local/share/filmcraft-fork/models/llm/` (qwen2.5-0.5b q4_k_m, qwen2.5-1.5b
  q4_k_m, smollm2-360m q8_0, smollm2-1.7b q4_k_m; SHA-256 checked); full-precision bases for
  training in `~/.local/share/filmcraft-fork/models/llm-base/{SmolLM2-360M-Instruct,
  Qwen2.5-0.5B-Instruct}/` (pinned revision in `REVISION`). Trainer venv
  `~/Apps/filmcraft-train/.venv` (Python 3.12, torch 2.5.1+rocm6.2, transformers, peft; 18 GB);
  run it with `HSA_OVERRIDE_GFX_VERSION=10.3.0` (RX 6700 XT = gfx1031; ~6 TFLOPS bf16, 12 GB).
  Tuned models are published as releases on the public dawgon2/filmcraft-cachyos only after the
  owner says "publish" (OWNER-QUESTIONS Q7).
- **Rust** 1.99 via rustup in `~/.cargo` (not pacman): `export PATH="$HOME/.cargo/bin:$PATH"`.
  `bindgen-cli` 0.73.2 is installed there (user-level) for regenerating `va_sys.rs`.
- **Bench `--only`:** the export section is gated by `wants("export")` (substring of the case
  name in the filter, not the other way round): use `--only export` (all three cases).
- **Shell is zsh**: unmatched globs are errors (quote `--include='*.rs'`); `set -- $var` does
  not word-split (use `bash -c '…'`); no `bc` (use python3). `pgrep -f` / `pkill -f` match
  their own command line (`pkill -f gates-2.2b` killed the tool's shell): use `pgrep -x cargo`,
  `pkill -x <name>`; killing cargo leaves orphaned `rustc` processes (`pkill -x rustc`).
- **Foreground commands time out at 10 min.** Run long work detached
  (`(cmd > log 2>&1; echo "exit $?" >> log) &; disown`) and wait with
  `timeout 590 bash -c 'until grep -q "^exit" log; do sleep 15; done'`. Never chain `sleep`s.
- **Dell lane (FORK-PLAN §1a.32):** `ssh dell` (fish shell: send `bash -s` scripts) is a second
  machine with an Intel UHD 770, set up and green (full build 392 s, tests 380 s). Use it for
  Intel-path tests (6.1b 4:2:2 parity first), long tests / soaks and weaker-PC checks; sync
  with `git push ssh://dell~/Projects/filmcraft-main hevc-vaapi`.
- **GPUs:** AMD RX 6700 XT = `/dev/dri/renderD129`, Vulkan GPU0; Intel UHD 750 = `renderD128`,
  Vulkan GPU1. Force one for wgpu tests: `MESA_VK_DEVICE_SELECT=1002:73df` (AMD) or `8086:4c8a`
  (Intel) plus `MESA_VK_DEVICE_SELECT_FORCE_DEFAULT_DEVICE=1`. Run GPU parity tests on both.
- **GPU stall with the video engine flat out (session 4):** decoding pictures back to back on
  VCN made compositing wait 40–100 ms per frame (`device.poll`); spread decoding did not.
  Reverse playback paces VA submissions (`cancel::with_pace`). Not caused by DMA-BUF import,
  surface churn or VA queue depth (all measured). Watch for it wherever a burst of decoding
  happens while playing (proxies, cache rendering in Phase 6).
- **Driver quirks found:** Mesa's VA-API wants HEVC scaling lists in raster order of each 4×4 / 8×8
  matrix (not the coded diagonal order the VA header suggests); RADV/Intel interpolate 1:1
  sample positions slightly off texel centres (fixed by snapping in `composite.wgsl`); ffmpeg's
  MOV muxer writes `hvcC numTemporalLayers = 6` for single-layer x265 streams (use the SPS);
  libva reuses surface ids after destroy (key caches by our own memory id).
- **Bench:** `--only "jkl J 1x"` runs one JKL case; `L 4x warm` / `J 2x warm` play the speed
  elsewhere first (remembered step); output has `steps` and cadence (`gap_ms`).
- **Env vars:** `FILMCRAFT_VAAPI_DEVICE` (render node), `FILMCRAFT_FFMPEG`,
  `FILMCRAFT_NO_GPU_IMPORT` (app/bench: no Vulkan DMA-BUF import → read-back path),
  `FILMCRAFT_VRAM_CACHE_MB` (decode-cache VRAM budget), `FILMCRAFT_SOAK_MINUTES` (bench soak),
  `FILMCRAFT_BENCH_TRACE=1` (bench lists frames that finished after they were due),
  `FILMCRAFT_BENCH_KEEP=<dir>` (bench keeps each export's output), `FILMCRAFT_NO_VA_ENCODE`
  (export through ffmpeg), `FILMCRAFT_FULL_SIZE_GRADE` (grade converted media at full size),
  `FILMCRAFT_XM5_DIR` (where the xm5 clips live; real clips can be linked there by name),
  `FILMCRAFT_NO_ZERO_COPY` (export reads frames back instead of rendering into encoder surfaces),
  `FILMCRAFT_EXPORT_TRACE` (per-batch export stage times on stderr).
- **Worktree lanes (session 10):** leaf builders in `.claude/worktrees/agent-*` with
  `CARGO_TARGET_DIR=target/agent-<name>` (~5 GB each); remove the worktree after merging.
- **Disk:** see the disk rule above; a debug build of the workspace regrows `target/debug` to ~60 GB. Fixtures: `target/fixtures/xm5/` (12 GB synthetic), `target/fixtures/hevc/`
  (conformance, `.hevc` + ffmpeg `.yuv`), `target/fixtures/platform/`, `target/fixtures/hevc4k/`.
- **Bench:** `target/release/examples/bench --section xm5 [--only CASE] [--quick] [--json f]`;
  cases ceilings, decode, play-full, play-graded, jkl, scrub, multicut, export, 422-play, and only
  when named: soak, open. Output line includes VA surfaces, cache VRAM and GOP seeks / decoded /
  skipped / evicted. Full suite ≈ 75 min (graded 4K export alone ≈ 40 min). Saved binaries in
  `target/fork-bench/bench-phase{0,1,2}`, results in `target/bench/xm5-phase*.json`.
- **Benchmark pitfalls hit:** run on an idle machine (an x265 encode, a build or orphaned
  `rustc`s inflate everything; check `uptime` and `ps`); never run benchmarks during the soak
  (they share the decode engine); `--quick` multicut covers only 3 cuts near GOP starts, the full
  60 s run covers 20 cuts deeper into the clips (much worse); bench plays used to leak a
  `FrameServer` each (fixed) — results of a case can depend on the cases before it in one process;
  the synthetic clips are x265 `ultrafast` open-GOP (CRA + RASL), keyint 30, ~49 % disposable.
- **CI:** `cargo xtask ci` ≈ 22 min since session 13 (nextest, `[profile.ci]`, one UI test binary `crates/ui-egui/tests/ui/main.rs`: a new UI test file needs a `mod` line there; prints per-stage times); run idle — `mixer_tests::perf_24_tracks_3_effects_realtime_factor`
  needs ≥ 2.5× real time on one core (4.7× idle). `cargo test --workspace --release --no-fail-fast`
  shows every failure.
- **App smoke test:** `filmcraft --data-dir <scratch> --control <port> --empty`, JSON lines
  `{"id":1,"method":"engine.execute","params":{"command":"file.import","params":{"paths":[…]}}}`,
  `file.newSequenceFromClip` `{"items":[id]}`, `ui.playback` `{"action":"play"}`, `perf.stats`,
  `ui.screenshot` `{"path":…}`, `app.quit`. Playback resolution: `ui.set {"playbackRes":"Full"}`
  (serde wants variant names: `{"program":{"res":"Full"}}` works, `"full"` is refused). JKL:
  `engine.execute` `playback.forward` / `playback.reverse` (each call doubles the speed while
  playing), `playback.stop`; shown / dropped in `ui.inspect` → `playback`. Script used in
  session 4: a Python JSON-lines client (see FORK-PLAN §7, Phase 3.3).
- **Silent audio tests:** cpal's PulseAudio client ignores `PULSE_SINK` and sets no
  `application.process.id`; a long test once played on the owner's speakers. Find the stream by
  `application.name` (check it first) before moving it to a null sink, or mute it another way.
- **Permission classifier:** the agent cannot set up self-restarting automation (`/loop`, timers
  that start `claude`); the owner does that (the runner script).

## Dead ends (with measurements, all synthetic)

- JKL (Phase 3.2): more spare VA surfaces (48 instead of 8) against the reverse GPU stall: no
  change; limiting VA submissions in flight (1, 2, 3, 64): 12–44 drops of 300, no fix (pacing
  the rate did it); a skimming tolerance of a quarter step (instead of half): grid frames deep
  in a GOP cost every reference before them and 4× climbed to step 24–30.

- ffmpeg pipe tuning (Phase 1): a 1 MiB pipe buffer did not help (22.2 vs 21.8 fps); ffmpeg's own
  download + write is the cap.
- Vulkan Video as primary: 5 % slower than VA-API in ffmpeg (40.9 vs 43.2 fps), frames identical.
- Multicut / cuts (Phase 2–3): requesting the first frame after the next cut early (20–90 frames
  ahead, with or without the priority gate), a deeper prefetch window (20–30 frames), a per-clip
  floor of 16 frames, waiting for every picture inside the priority gate (−6 % throughput):
  all made cut drops worse or did nothing. What helped: the open-GOP seek fix (correctness) and
  skipping non-reference pictures before a clip's in-point (~98 → 25–42 drops of 360).
- VRAM: Auto budget of half the VRAM ran a 12 GB card out of memory with four 6.2K clips; a
  quarter is the default. The monitor frame cache charging GPU frames their full size left room
  for ~9 plans (fewer than the prefetch window); it charges 16 MB per GPU frame.

## Open problems

- **Damaged video can wedge AMD's VCN (2026-10-15, OWNER-QUESTIONS Q39):** `vaapi::damaged_samples_and_parameter_sets_never_crash_or_hang` caused a VCN page fault at 14:20 (and very likely Q37's at 06:40); hardware decode stays slow until a reboot. The hostile VA-API tests now skip AMD (`FILMCRAFT_VAAPI_HOSTILE_AMD=1` runs them). Later: stricter bitstream checks before VA submit, or software decode after damage; find the triggering pattern and report it to Mesa.
- **Assistant padding (AI3):** ~35 / 85 answers still carry a line or two from a neighbouring topic, ~16 longer than needed, 3 lines cut at a table; misses `adv-check-scene`, partial `adv-adjustment-layer`, `adv-saturation-luma`, `adv-gfx-lower-third-series` (`coach-eval/AI3-MUST-READ.md`).
- **AMD VCN stuck (2026-10-15 06:40, Q37):** `Fence fallback timer expired on ring vcn_dec_0` since ci's VA-API tests;
  VCN page fault at 07:28. Until the owner reboots, run VA-API checks on Intel / the Dell. Open question: can our hostile
  hardware-input tests (`damaged_samples_and_parameter_sets_never_crash_or_hang`) trigger this driver fault? After the
  reboot: run the AMD `vaapi` binary alone and watch `journalctl -k`.
- **A2 review leftovers (non-blocking):** ~~(2) `installFile` on the command thread~~ ✓ 2026-10-15 19:49 (kept there, documented: bounded work; a zip is refused by size before it is hashed, test in `hostile_files_are_refused_before_anything_is_written`);
  ~~(3) `Denoiser::reset` allocates~~ ✓ 2026-10-15 19:45 (`Features::reset` refills in place, test `features_reset_restores_the_start_in_place`); ~~(7) `apply:false` / `wait:true` on the command thread~~ ✓ 2026-10-15 19:52 (documented as control-channel / CLI / MCP only; the UI button sends neither); ~~(10) `takes_of` scans every item each frame~~ ✓ 2026-10-15 19:53 (cached per clip, revision and project; test `voice_over_takes_in_both_workflows` checks a pick and its undo show at once). **A2 review leftovers all done.**
- 4K 10-bit 4:2:2 H.264 in-app playback is bursty (2026-10-15, synthetic): decoder alone 36.5 fps (12 threads), app shows
  9–24 / 25 fps, p95 decode jobs 500–600 ms (frame-threading latency vs the frame workers' deadlines?). Proxies cover it today.

- 6.2K cuts every few seconds drop frames on original media (decoder-bound) → Phase 6.
- 4× on this synthetic stream (51 % reference pictures, open GOP) runs at the decoder's limit
  even at step 12–15: steady skipping at ~8–10 pictures/s, not more.
- 4:2:2 HEVC does not decode at all (no decoder on Linux) → Phase 6.1.
- Graded 4K export 27.5 fps (just under real time): one VCN shared by the 6.2K decode and the
  encode; a possible 6.2K optimisation phase later (FORK-PLAN §1a.16, only with the owner).
- First play after loading drops frames for ~0.5–1 s (cold start) → Phase 7.
- Fujifilm's F-Log2 LUT changes gamut on the log signal; built-in is linear light (FORK-PLAN §7).
- Grade node ids are per grade (`Grade::next_id`); the current node is clip-scoped
  (`EditorState.grade_node: Option<GradeSel>`).
- Linux default keymap has shortcut conflicts (Ctrl+9, Ctrl+Shift+M, Ctrl+T) — not fixed.
