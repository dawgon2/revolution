# Cloud queue

Jobs for the cloud routine (branch `cloud-work`, only while the PC is down; FORK-PLAN §1a.23).
CPU-verifiable, chosen not to overlap with what the PC run is doing. Take them top to bottom; note
each in `docs/fork/CLOUD-LOG.md` with what is UNVERIFIED ON GPU.

**PC is doing now (2026-10-21 03:45, scheduled runs): AI2 (AI-PLAN; tuned explainer `smol360-v3` training on the GPU until ~06:00, then scoring sets 1–5, full ci, tag `ai-2`, publish 0.2.1), then A3 (AUDIO-PLAN mixer basics). Don't touch `crates/render/src/grade_explain/`, `crates/engine/src/comp_explain*`, `crates/llm`, `tools/explainer-train/` or `crates/ui-egui/src/ai_explain*`.**

Jobs (refreshed 2026-10-21; C2-GUIDE dropped: C2's parallel / layer / outside sections already exist in `node-trees.md`), take these first:
- **REF-ASK-DROP (`crates/ui-egui/src/panels/coach.rs` Ask box only):** drop or paste a picture (PNG / JPEG from the
  clipboard or a file) onto the Ask box: run `stills.import`, set it as the compared still (`compare.set`), and prefill
  "How do I get this look?". Done = UI test in `crates/ui-egui/tests/ui/coach_ui.rs` (drop via `OsFile` like
  `stills_ui.rs`), guide line in `docs/guide/color/reference-look.md` (keep `ai_pool` floors: run
  `cargo test -p filmcraft-engine --test ai_pool`), es + zh-cn strings. GPU owed: none.
- **REF-EVAL (engine tests only):** 6 more generated reference looks in `crates/engine/src/coach_reference_tests.rs`
  (bleach bypass, day for night, pastel, high-contrast B&W, golden hour, neon night) each asserting the expected
  `LookTag`s and a grounded recipe (`assert_grounded`); tune nothing in `look_diff.rs` without a note in CLOUD-LOG.
  GPU owed: none.

`crates/hevc`, `crates/codecs` (hybrid / hw factory), `crates/export/src/lanes/`, or the Dell lane's
Linux desktop files (`apps/filmcraft/src/{desktop,mpris,cards,kdeconnect,portal*}.rs`). Earlier rules:
**Don't touch** `crates/ui-egui/src/{app.rs,menus.rs,theme.rs,workspaces*,i18n/mod.rs}`,
new `projects*` files, the autosave / project file code, `crates/platform`, `crates/hevc`, the
grade model / compiler, `crates/llm` or `.cargo/config.toml`. New user-visible strings need es +
zh-cn entries. The fork build defaults to the **Resolve** workflow (feature
`fork-resolve-default`): a new UI test about Premiere behaviour must set
`prefs.workflow = Premiere`. **Guide pages are read by the Grading Coach at compile time:** a new
page must also be added to `GUIDE_PAGES` in `crates/engine/src/coach_ask.rs`, and its `{ui:}` ids
must be known to the `coach_ui` walk tests.

Done earlier (merged and checked on the PC): GUIDE-1…6, GUIDE-CHECK (covered by
`coach_ask_tests::every_indexed_section_has_provenance`), REVIEW-1, I18N-1, EXPORT-UI-1, KEYMAP-1.

1. **~~GUIDE-7~~ DONE 2026-10-13 (PC run): `docs/guide/editing.md` written, in `GUIDE_PAGES`.** Was: **GUIDE-7, `docs/guide/editing.md` (docs only):** cutting on the timeline, JKL shuttle (each
   press doubles the speed), trimming, multicam; link to `proxies.md` / `render-cache.md` (the PC
   writes those) instead of explaining them. Every command id must exist (grep `crates/engine/src`).
   Done = page written with provenance tags, in `GUIDE_PAGES`, README status updated, `coach_ask`
   tests green. GPU owed: none.
2. **~~GUIDE-8~~ DONE 2026-10-13 (PC run):** scopes section of `color/finishing.md`; eval question `uncovered-scopes` now covered.
3. **~~Y1-PREP~~ DONE 2026-10-12 (side lane, `docs/fork/PREP-Y1-6E-U02.md` §1.1: no parser field missing; don't redo).** Was: **Y1-PREP, `crates/h264` only (CPU):** check that the parser exposes every SPS / PPS /
   slice-header field a VA-API H.264 decode needs (`VAPictureParameterBufferH264`,
   `VASliceParameterBufferH264`, `VAIQMatrixBufferH264`: names from the public libva header, no
   code copied), for Constrained Baseline / Main / High, 8-bit 4:2:0 progressive. Add any missing
   fields with unit tests on hand-built or fixture bitstreams (fixtures generated with ffmpeg into
   `target/fixtures/`, never committed). Write the field map to `docs/fork/Y1-FIELDS.md`. Done =
   map + tests, `cargo test -p filmcraft-h264` green. GPU owed: none (Y1 itself is PC work).
4. **~~I18N-2~~ DONE 2026-10-13 (PC run, nothing to add):** every proxy / render-cache string is a
   `tl!` / `tlf!` literal, and `i18n::tests::{spanish_translates_every_tl_literal,
   chinese_translates_every_menu_label_and_tl_literal}` already fail on any gap; don't redo.

Removed 2026-10-12: COACH-WALK-1, G2C-CLIP, RS1-HEADLESS (an owner side lane fixes them on the PC,
FORK-PLAN §1a.40 (c)). Also don't touch the TNR / grade compiler code (step 6.7b is next on the PC).

5. **~~A1-PREP~~ DONE 2026-10-13 (PC run): `docs/fork/PREP-A1.md`; don't redo.** Was: **A1-PREP, docs only (CPU):** read AUDIO-PLAN.md A1 (Linux playback reliability) and audit the
   playback path it names (`crates/audio`, the cpal output in the app, underrun handling, device
   switching); write `docs/fork/PREP-A1.md`: what exists, what A1's steps need per file, risks,
   suggested test list. No code changes. GPU owed: none.
6. **~~Y3-PREP~~ DONE 2026-10-13 (PC run): `docs/fork/PREP-Y3.md`; don't redo.** Was: **Y3-PREP, docs only (CPU):** same for YOUTUBE-PLAN Y3 (GPU keys, attach-to-track / tracked
   blur, screen replacement, WGSL custom shaders, look packs): what exists (keyers, tracker,
   effect registry), the gaps, and a lane split; write `docs/fork/PREP-Y3.md`. No code changes.
   GPU owed: none.
7. **~~C1-PREP~~ SUPERSEDED (cloud 2026-10-14): `docs/fork/DESIGN-C1.md` covers it; don't do.** Was: **C1-PREP, docs only (CPU):** read COMPOSITOR-PLAN.md C1 and C2 and audit what they build on
   (the grade node model and compiler, `effects` / FxOps, GPU compositing in the renderer, keyers,
   masks, text / title rendering, the ADR list); write `docs/fork/PREP-C1.md`: what exists, gaps
   per C1 / C2 step with the files they touch, risks (serde of the DAG, cycles, GPU memory),
   suggested tests, a lane split. No code changes. GPU owed: none.
8. **~~AI1-PREP~~ DONE 2026-10-13 (PC run): `docs/fork/PREP-AI1.md`; don't redo.** Was: *read AI-PLAN.md AI1 and audit `crates/llm` and every place that
   depends on the x86-64-v3 build flag (`.cargo/config.toml`, FORK-PLAN Q8); write
   `docs/fork/PREP-AI1.md`: the current forward pass and quantisation, what a run-time CPU dispatch
   (`is_x86_feature_detected!`, scalar fallback) needs per file, how to test parity between the
   paths, risks. No code changes. GPU owed: none.*
9. **~~R1-PREP~~ DONE (cloud `7fcae72f`, docs/fork/PREP-R1.md, merged and tested on the PC 2026-10-14); don't redo.** Was: **R1-PREP, docs only (CPU):** read RAW-PLAN.md R1 (CinemaDNG, pure Rust) and audit what exists
   for TIFF / DNG parsing, lossless JPEG, demosaic and the input-transform path; write
   `docs/fork/PREP-R1.md` (exists / gaps per step / fixtures that ffmpeg or a public-domain sample
   could give / risks / tests). No code changes. GPU owed: none.

10. **TEARDOWN-FLAKE (diagnosed by the cloud 2026-10-14, see CLOUD-LOG "TEARDOWN-FLAKE": the holder is the render-cache `CacheJob`; its "exact next step" is the plan. **Test part done on the PC 2026-10-15 (`c39b3289`)**: frames are checked deterministically (no queued job after the close, `FrameServer::live_workers`), everything must exit within 2 min. **Still open:** the render-cache job can take seconds to see the cancel inside one frame under load (the cancel flag already reaches `render_batch` / `encode_batch` per frame): time each `CacheJob::step` phase after `stop` and make the long piece check the flag; then the 2 min bound can come back down. A full 6-thread run of the ui binary is OOM-killed in the 20 GB build slice on the PC (18 GB peak), so verify with the filter `teardown_ui timeline text title trim` at 6 threads, which reproduced the old failure), `crates/ui-egui` test only (CPU):** `tests/ui/teardown_ui.rs`
   `dropping_the_app_stops_its_workers_and_frees_its_media` fails under the full parallel
   `cargo xtask ci` run (8 media references left after its 3 s settle) but passes alone. Find what
   still holds the media pool after the app drops under load (frame workers, render-cache job,
   thumbnail jobs) and make closing wait for / cancel it deterministically, or make the test
   measure the right thing; never just raise the timeout. Done = the test passes in 3 full
   parallel runs of the ui test binary with `RUST_TEST_THREADS=12`. GPU owed: none.
11. **~~MKV-HANDOFF~~ DONE (cloud `fbf60f1d`, merged and tested on the PC 2026-10-14; the MKV Intel export check is still owed by the PC).** Was: **MKV-HANDOFF, `crates/codecs` (CPU):** `MediaSource::set_handoff_scale` is implemented for
   MP4 / MOV only (`mp4.rs`, U0.2.3b d). Give `mkv.rs` (and any other source whose decoder comes from
   `make_video_decoder`) the same: store the scale, wrap its decoder creation in
   `hw::with_handoff_scale`. Done = a unit test per source that the scale reaches
   `hw::handoff_scale()` inside decoder creation (a test factory can record it). GPU owed: one
   4K-from-6.2K MKV export on the PC with Intel decoding.
12. **~~USHADER-HOSTILE~~ DONE (cloud `682d7c16`, merged and tested on the PC 2026-10-14); don't redo.** Was: **USHADER-HOSTILE, `crates/usershader` tests only (CPU):** a deterministic generator test
   (no new dependency: a small xorshift) that feeds `parse` 2 000 mutated effect files (random
   annotation lines, truncated / repeated `// @param`, huge numbers, NaN / inf defaults, non-ASCII
   names, `fx_` names, nested comments, 16+ params, empty file) and asserts it never panics, every
   error has a line inside the file or none, and every accepted effect validates with `validate`.
   Done = the test passes in < 10 s. GPU owed: none.
13. **~~USERFX-FOLDER~~ DONE (cloud `e82421cd`, merged and tested on the PC 2026-10-14); don't redo.** Was: **USERFX-FOLDER, `crates/engine` test only (CPU):** extend `user_effects_tests.rs` for the
   Effects folder edge cases: a symlink loop / a directory named `x.wgsl`, a non-UTF-8 file, an empty
   file, 300 files (cap `MAX_FILES` 256, the rest reported in the log), two files whose stems give
   the same id (`My Glow.wgsl` / `my-glow.wgsl`), upper-case `.WGSL`. Fix what panics or misreports
   (with the test). Done = tests green. GPU owed: none.
14. **~~GUIDE-SHADER-COOKBOOK~~ DONE (cloud `d2cb0c2c`; recipes also render on AMD + Intel in `gpu` `bundled_examples_render`, merged and tested on the PC 2026-10-14); don't redo.** Was: **GUIDE-SHADER-COOKBOOK, docs only (CPU):** add a "Recipes" section to
   `docs/guide/shader-effects.md` with 3 short original example effects users can copy (vignette
   with a softness control, a duotone using two colour params, a time-driven pulse), each checked by
   a new engine test that parses and validates it with `filmcraft_usershader::validate`; compact
   header per `docs/guide/README.md`; keep the coach eval at its floor (`coach_ask_tests`; reword
   rather than lower it). GPU owed: render each recipe once on AMD and Intel.

15. **~~C1-GUIDE~~ DONE 2026-10-14 (PC run, `91879dca`: partial page, 10b sections come with 10b); don't redo.** Was: **C1-GUIDE, docs only (CPU; added 2026-10-14):** draft `docs/guide/compositor.md` (C1 step 11): what a comp
    is (a project item, a node graph; comp clips on the timeline), every node kind and what it does (DESIGN-C1 §2,
    `crates/project/src/comp.rs`), the `comp.*` commands as they exist now (`crates/engine/src/comp.rs`; leave the
    Compositor panel / Fusion page parts as "coming in C1 step 10" with no UI labels invented), the two acceptance
    comps as recipes (comp A: Media In → Key → Merge over a background → Color → Out; comp B: Text → keyframed
    Transform → Merge), limits ([Fork]: transparent on a cycle, Layer sources only below the comp clip, 256 nodes).
    Provenance tags per docs/guide/README.md ([Resolve-style] for the Fusion-like flow, from the public manual only;
    [Fork] for limits), compact section headers (`coach_ask::parse_header` spec), add it to `GUIDE_PAGES` in
    `crates/engine/src/coach_ask.rs` and run `cargo test -p filmcraft-engine coach` (the header / ui-id checks).
    **Done** = page + GUIDE_PAGES entry, engine coach tests green. PC owes nothing (no GPU).
16. **~~COMP-REVIEW~~ DONE 2026-10-15 (PC run, Opus review: 4 fixes `aa5a39f4` … `b0230ca6`, 1 left as is; see HANDOFF); don't redo.** Was: **COMP-REVIEW, read-only review (CPU; added 2026-10-14):** the review owed from C1: read
   `crates/gpu/src/comp.rs`, the comp hook in `render_plan` and `plan::track_layers` + `plan::media_frame_layer` (Item-source Media In, 2026-10-15) for the
   never-crash rules (indexing, checked math, capped allocations from hostile comp sizes / node
   counts), slot / texture lifetime, and hostile DAGs (cycles, missing inputs, 0×0 or huge comp sizes).
   **Write findings only** to CLOUD-LOG (file:line, failure scenario, suggested fix); no code
   changes (C2 is editing next to it). Done = the findings list. GPU owed: none.
17. **~~COMP-EVAL~~ DONE 2026-10-15 (PC run: 4 of the 5 `comp-*` misses fixed guide-side, coach eval 98 / 124, floor 98; `comp-what-is` still loses to "Making a comp" on its heading word, left honest); don't redo.** Was: **COMP-EVAL, guide + eval (CPU; added 2026-10-14):** the 6 compositing coach-eval misses
   (`comp-*` in `docs/fork/coach-eval/questions.toml`): run the eval the way `coach_ask_tests` does,
   find which `docs/guide/compositor.md` sections are missing the answer, add them (plain language,
   provenance tags, real labels / ids), until each `comp-*` question is answered from the guide.
   Done = the eval floor test passes with the misses gone. GPU owed: none.
18. **~~PEAKS-CANCEL~~ DONE 2026-10-14 (PC run: `TlState::cancel_peak_jobs`, set in `FilmcraftApp::drop` and on project change; `sequence_peaks` checks it per chunk; test `a_cancelled_sequence_mix_returns_without_peaks`); don't redo.** Was: **PEAKS-CANCEL, `crates/ui-egui/src/panels/timeline.rs` only (CPU; added 2026-10-14):** give
   `request_sequence_peaks`' detached thread a cancel flag (an `Arc<AtomicBool>` the app's `Drop`
   sets, checked between clips / blocks) so it drops its `PoolProvider` quickly at close; test that a
   cancelled job returns without a result. Done = test + `cargo test -p filmcraft-ui-egui --lib`
   timeline tests. GPU owed: none.
10. **~~C2-PREP~~ SUPERSEDED 2026-10-15 (PC run: `docs/fork/DESIGN-C2.md` covers it; don't do).** Was: **C2-PREP, docs only (CPU):** read COMPOSITOR-PLAN.md C2 and GRADE-PLAN's node model, audit
    the grade model / compiler and C1's comp DAG (`docs/fork/DESIGN-C1.md`, `crates/engine` comp
    code) and write `docs/fork/PREP-C2.md`: the exact types, migrations (serial grades unchanged),
    commands, UI ids, tests and coach lessons C2 needs, in step order, with risks. Done = the file,
    nothing else changed. GPU owed: none.
10b. **C2-MODEL (CPU, only once HANDOFF says C2 has started; added 2026-10-15):** DESIGN-C2.md §5 lane 1 in
    `crates/project`: `Grade.edges` / `Grade.output`, `GradeNode.kind`, virtual source id 0 (a stored node id 0
    renumbered at load with its edges), `DagNode` for grade nodes, `Grade::graph_order()` on `dag::validate`, schema
    v19 + migration test (serial fixtures load and compile bit-identically); the ~69 `Grade { … }` / `GradeNode { … }`
    literals in other crates take `..Default::default()` / `GradeNode::new`. Done = `cargo test -p filmcraft-project
    -p filmcraft-render` green, workspace builds. GPU owed: none (serial path unchanged; the PC re-runs grade parity).
11. **AI2-PREP, docs only (CPU):** read AI-PLAN.md AI2, audit `tools/explainer-train/` (generator,
    fact checker, held-out sets) and write `docs/fork/PREP-AI2.md`: which comp-graph / branch-node
    cases to add, how the fact checker must grow, the two new held-out sets' shape, and what the PC
    must run (training on the RX 6700 XT). Done = the file. GPU owed: the training itself (AI2).
12. **~~GUIDE-9~~ DONE 2026-10-15 (PC run: `getting-started.install`); don't redo.** Was: **GUIDE-9, `docs/guide/getting-started.md` install section (docs only):** README marks
    "install to write": building from source (craft-fonts optional), the phase builds / launcher
    in plain words, first launch (Project Manager, Workflow switch). No personal paths or names.
    Done = section written with provenance tags, README status updated, `cargo test -p
    filmcraft-engine --lib coach` green (the eval floors must not drop). GPU owed: none.
13. **~~AI3-EVAL-2~~ DONE 2026-10-14 (PC runs: +22 compositing / editing / audio / export / titles / media / playback / project questions, 85 total; retrieval recall@5 85 / 85, floor 85); don't redo.** Was: **AI3-EVAL-2, questions only:** §1a.75 says AI3 covers all editing incl. the Compositor; add
    ~20 questions about editing / compositor / export to `docs/fork/coach-eval/advanced.toml` in
    its existing format (expected section ids from the guide pages; mark gaps as gaps), without
    changing any code or floor. Done = `cargo test -p filmcraft-engine --lib coach` green; list
    in CLOUD-LOG which new questions retrieval misses. GPU owed: none.
14. **~~AI3-GPU-PREP~~ SUPERSEDED 2026-10-15 08:45: the PC builds GPU inference directly (two Opus lanes); don't do it.** Was: **AI3-GPU-PREP, docs + CPU reference only (added 2026-10-15; PC started the code 2026-10-15 05:45: `crates/llm/src/gpu.rs`
    feature `gpu`, Q4_K matvec + parity tests; the note should build on that, not redo it):** the next big AI3 step is GPU (wgpu) inference for
    SmolLM3-3B Q4_K_M (AI-PLAN item 3; HANDOFF "AI3.3b": CPU prefill ~40 tok/s, generation ~5.9 tok/s, acceptance wants
    first words < 1 s, a typical answer < 10 s). Audit `crates/llm` (gguf, kquant block layouts Q4_K / Q6_K, `forward.rs`
    `PREFILL_CHUNK`, `Weight::matmul`, `quant::rows_at`, KV cache) and `crates/gpu`'s compute-pipeline helpers, and write
    `docs/fork/PREP-AI3-GPU.md`: how Q4_K / Q6_K blocks go into storage buffers unchanged (no dequantised copy: 1.9 GB
    model, 12 GB AMD / shared Intel), the WGSL kernels needed (matvec for generation, tiled matmul for prefill, RMSNorm,
    rope with SmolLM3's NoPE layers, attention over a GPU KV cache, SiLU-gate), which crate owns them (layering: `crates/llm`
    must not depend on egui; a new `llm-gpu` crate or a `gpu` feature), the CPU ↔ GPU parity tests (logits on a few prompts
    within a tolerance, per-kernel tests against `kquant` dequant), the fallback when no adapter / too little memory, and
    a step order. Optional CPU-only code: none (design only). Done = the file. GPU owed: everything that runs kernels.
15. **~~A2-REVIEW-LEFTOVERS~~ DONE 2026-10-15 19:53 (PC run, all four); don't redo.** Was: **A2-REVIEW-LEFTOVERS (added 2026-10-15, CPU only):** the Opus review of A2 (progress log 2026-10-15 ~08:35) left four
    non-blocking items: ~~(2) `installFile` on the command thread~~ DONE 2026-10-15 (PC run: documented reason + size check before hashing); ~~(3)
    `Denoiser::reset` allocates~~ DONE 2026-10-15 (PC run: `Features::reset` refills in place); ~~(7) cleanup `apply:false` / `wait:true`~~ DONE 2026-10-15 (PC run: documented as control-channel only); ~~(10) `takes_of` per-frame scan~~ DONE 2026-10-15 (PC run: cached per clip + revision). **Whole item DONE; don't redo.** Done = each fixed with a test (or a documented reason), voice + engine voice
    tests green. GPU checks owed: none.
16. **COACH-FEATURE-PAGES (added 2026-10-15, `crates/engine` coach + `docs/guide/` only, CPU):** the Dell lane's AI-POOL
    batches found the keyword coach answers almost none of the pool's coach questions on feature pages (`timeline.md`,
    trimming / edit-menu / file-menu: 0 of 13); indexing `timeline.md` whole was tried and reverted (colour eval 100 → 99:
    "do I grade before or after I cut" lands on `timeline.in-out`; rewording broke `adv-edit-j-cut` retrieval@5). Find a
    way that keeps both: e.g. index only those pages' sections with a `does:` header, or a page-family penalty for
    colour-worded questions. Done = pool coach floor up, colour eval still 100 / 100, advanced retrieval@5 floors kept,
    `coach_ask_tests` + `ai_pool` green. GPU checks owed: none (the 85-case model eval is re-run by the PC when STEADY). **Measured 2026-10-16 02:09 EDT (PC run, reverted):** indexing timeline / edit-menu / trimming / file-menu with a score factor on their sections (0.55 / 0.7 / 0.85; also without timeline.md) lifts the pool coach to 131–136 / 188 but the colour eval always drops 100 → 99: `uncovered-edit-then-grade` ("do I grade before or after I cut the edit", expected: no answer) gets answered (conf 0.280, `color/nodes.md › The Nodes panel`) because the extra pages change the index's word statistics (df / lengths), whatever the factor. So the fix has to keep the colour index's statistics: e.g. a second `Index` over the feature pages with its own stats, consulted only for questions whose colour-index ceiling-normalised confidence is low **and** whose feature-index match is strong, with that question as the guard case.

19. **(Taken by the Dell lane 2026-10-15 22:58, §1a.91 CREATIVE-POOL: skip.)** **CREATIVE-2 (added 2026-10-15, `docs/fork/coach-eval/pool/` + `docs/guide/practice/` only, CPU):** AI-PLAN AI4
    item 5c.1: a second creative batch `pool/creative-2.toml` (8–12 held-out, kind `creative`, ≥ 2 sources each,
    fully / partly / not possible, gaps said plainly with a workaround; examples left in AI-PLAN: security camera /
    VHS / old film, whip-pan transition, music ducking without keyframes, …) plus `creative-2.jsonl` training pairs
    that don't reword the held-out ones; where an answer keeps needing a technique the guide lacks, a practice note
    (options + trade-offs; check `retrieval_finds_the_advanced_sets_sources` recall@1 stays ≥ 73). Done = `cargo test
    -p filmcraft-engine --test ai_pool` and `--lib coach` green, `creative_retrieval_covers_the_plan` reported in
    CLOUD-LOG. GPU checks owed by the PC: none (the daily model eval covers it later).
20. **~~AI4-VISION-SIMD~~ DONE 2026-10-16 00:55 EDT (PC run, 37 → 25.6 s); don't redo.** Was: **AI4-VISION-SIMD (added 2026-10-16 00:53 EDT, `crates/llm/src/vision.rs` only, CPU):** the vision encoder's f32 `dot` (used by `Linear::forward` and `attention`, 16-row tiles) is portable code; give it run-time AVX2 + FMA dispatch the way `quant.rs` / `kquant.rs` do (`pulp::x86::V3::try_new()` honouring `quant::simd_level()` / `FILMCRAFT_LLM_SIMD=baseline`, `vectorize` around a struct implementing `pulp::WithSimd`, `#[inline(always)]` kernels; a closure passed to `Arch::dispatch` was tried and is NOT inlined: no gain). Done = unit tests: SIMD dot equals the portable one within 1e-5 relative on random lengths 0…300 incl. non-multiples of 8 / 16, and `Linear::forward` / `attention` give the same output with `set_simd_level(Baseline)` and Auto; clippy default + `gpu`. PC owes: `FILMCRAFT_QWEN3VL_TIME=1` timing of `tests/qwen3vl_vision.rs` (37 s before) and the oracle parity numbers unchanged.
21. **~~AI4-TEXT-PREP~~ DONE 2026-10-16 00:56 EDT (PC run: `docs/fork/DESIGN-AI4-TEXT.md`); don't redo.** Was: **AI4-TEXT-PREP (added 2026-10-16 00:53 EDT, docs + test-only code, CPU):** design note `docs/fork/DESIGN-AI4-TEXT.md` for running Qwen3-VL 4B's text model on our runtime: what differs from our Qwen / SmolLM3 forward pass (interleaved M-RoPE sections [24, 20, 20] and how picture tokens get their (t, h, w) positions; DeepStack: the 3 tap outputs added to the hidden state at the picture tokens after layers 0 / 1 / 2; q / k norm; tied embeddings; BF16 safetensors → our quantised formats or f32), read from candle-transformers 0.9.2 `models/qwen3_vl/{mod,text}.rs` (MIT / Apache-2.0, in the cargo registry) and the model's published config; plus the end-to-end oracle plan (candle `Qwen3VLModel::forward` logits on a fixed prompt + picture). No product code. PC owes: nothing GPU; the PC reviews the note before building.
22. **~~AI4-Q4K-CONVERT~~ DONE 2026-10-16 02:17 EDT (PC run: Q4_K weight quantizer byte-identical to candle; Q4_K mix with Q8_0 where Q4_K_M uses Q6_K; no Q6_K port, candle's needs an unsafe helper); don't redo.** Was: **AI4-Q4K-CONVERT (added 2026-10-16 02:02 EDT, `crates/llm` only, CPU):** `convert.rs` writes Q8_0 only (4.27 GB for Qwen3-VL's text model). Port ggml's Q4_K / Q6_K **weight** quantizers (candle-core 0.9.2 `quantized/k_quants.rs` `quantize_row_q4_K` / `q6_K` with `make_qkx2_quants` etc., MIT OR Apache-2.0, attribution header + ATTRIBUTION.md row like `kquant.rs`) into `kquant.rs`, and give `qwen3vl_text_to_gguf` a Q4_K_M mix like `llm_quantize --type q4k` (Q6_K for `attn_v` / `ffn_down` in the "more bits" blocks and for an output head copy of the tied embedding). Done = our block bytes equal candle's `QTensor::quantize` bytes on random matrices (candle is a dev-dependency), the tiny-checkpoint test runs the Q4_K_M file like the same weights written directly, clippy default + `gpu`. PC owes: real conversion (~2.7 GB) and `qwen3vl_text` tests on it.
23. **~~AI4-LOOK-CACHE~~ DONE 2026-10-16 02:11 EDT (PC run); don't redo.** Was: **AI4-LOOK-CACHE (added 2026-10-16 02:02 EDT, `crates/engine/src/coach_assist.rs` only, CPU):** the vision model's look (`PictureLooker::look`, ~28 s on the CPU) runs again for the same frame and question. Cache the last few replies in `AssistJobs` keyed by the picture's content hash + question (as `picture_cache` keys the fact sheet); a test with a counting fake looker (same frame + question → one look; another frame or question → a new look; a failed look isn't cached). PC owes: nothing GPU.
24. **AI4-2B-FIXTURES: STARTED 2026-10-16 02:27 EDT (PC run): `docs/fork/coach-eval/looks.toml` (15 fixtures: shade blue cast, tungsten, blown sky, banded sky, underexposed, noisy shadows, green LUT, laptop screen glow, flat B/W, sky + water, sunset, washed out, grey clouds, neon sign, flat log; real Qwen3-VL 14 / 15, nothing invented) + `crates/engine/tests/look_fixtures.rs` (fixture checks incl. no `must_not` word the question itself uses, whole-word scoring, pixel-statistics fake looker 7 / 9 floor, ignored real-model run). Still open: fixtures for the other ~59 `needs_clip` questions where a flat-shape frame can set the situation up (skip ones that need motion or real faces; say which in the file). Was: (added 2026-10-16 02:02 EDT, `crates/engine` tests + `docs/fork/coach-eval/` only, CPU):** AI4 item 2b's acceptance ("does the look mention what the question is about; nothing invented on frames with known content") has no frames. For the AI-POOL `needs_clip = true` questions (~35, colour-* / practice-* / pictures-2 / compositor-1), write synthetic fixture recipes (generator scene + a grade or effect that creates the situation: green cast on skin tones, clipped sky, tungsten vs daylight, underexposure …) with the facts a right look must name and the ones it must not claim, plus a harness test that renders each with `filmcraft-cli`-style engine calls and runs a `PictureLooker` over it (a fake looker in the test; the real one on the PC). Done = fixtures + harness + fake-looker test green. PC owes: the real Qwen3-VL run and its score.
25. **AI4-SCENES-2 (added 2026-10-16 05:12 EDT, `crates/engine/examples/vision_traindata.rs` only, CPU):** more synthetic scene
    kinds for the picture-model tune, each with known light and labels checked against `PictureFacts` like the
    existing seven: mixed light (window daylight + tungsten lamp, the two sides different colours), a face under a
    green fluorescent tube, a backlit face against a clipped bright window, snow, a neon-lit interior, a candle-lit
    close-up. Labels in the same plain style (no log-curve names, nothing a picture can't show). Done = the
    generator runs (`--frames 3000`), `data-stats.json` shows every new scene with < 40 % dropped, and
    `docs/fork/coach-eval/VISION-READ.md` gets 2 sample rows per new scene. **PC owes:** re-dump + re-tune +
    base-vs-tuned score (GPU).
26. **~~AI5-PREP~~ SUPERSEDED 2026-10-16 10:50 EDT (PC run built AI5 steps 1–5 directly; don't do).** Was: **AI5-PREP (added 2026-10-16 05:12 EDT, docs only, CPU):** read AI-PLAN.md Phase AI5 (share an Assistant example; items 1–8,
    incl. the fork-only local training inbox, FORK-PLAN §1a.92) and audit what exists (`coach_assist.rs`
    `AssistAnswer` / `seen`, the Assistant panel and Ask box in `crates/ui-egui`, any GitHub-issue or Save
    code). Write `docs/fork/DESIGN-AI5.md`: the data a shared example carries, what the user ticks, how it is shown
    before sending, the issue body format, the Save file format, the inbox layout, privacy checks (no paths,
    names, media), and the first 6 code steps with their tests. **PC owes:** nothing (design read by the next run).
27. **S1-PREP (added 2026-10-16 10:50 EDT, docs only, CPU):** S1 (STORAGE-PLAN Phase S1) comes right after AI5. Read
    STORAGE-PLAN.md S1 items 1–5 and audit what exists: `projects.export` / import (`crates/engine/src/projects.rs`,
    `project_manager.rs`), the offline badge and relink-by-identity (`media_pool.rs`, `media_watch.rs`), the save path
    (`crates/format/src/atomic.rs`, `autosave.rs`), the render-cache / proxy claim-folder rules. Write
    `docs/fork/DESIGN-S1.md`: what is reused, what is new, the package manifest format (relative paths, sizes, SHA-256,
    left-out list), the order of the first 6 code steps with their tests. **PC owes:** nothing (the next run reads it).
28. **~~S1-NETFS~~ DONE 2026-10-16 12:20 EDT (PC run: `crates/engine/src/netfs.rs`, `media.storage`, Project Settings line); don't redo.** Was: **S1-NETFS (added 2026-10-16 10:50 EDT, `crates/engine` (or `crates/platform` if it fits better) + tests only, CPU):**
    S1 item 1: a pure function that classifies the filesystem a path lives on from `/proc/self/mountinfo` text
    (longest mount-point prefix wins; `cifs` / `smb3`, `nfs` / `nfs4`, `fuse.kio-fuse`, `fuse.sshfs`, `fuse.rclone`,
    `9p` → network; escaped spaces `\040` in mount points; bind mounts; a hostile / truncated file gives "unknown",
    never a panic), plus the Linux caller that reads the real file (other OSes: "unknown"). Done = unit tests on
    fixture mountinfo texts (incl. a path under `/run/user/1000/kio-fuse-xyz/smb/…` and a path with spaces), clippy,
    `cargo xtask wasm` still green (no `/proc` read on wasm). **PC owes:** a check on a real SMB mount if one exists.
29. **~~S1-MANIFEST~~ DONE 2026-10-16 13:10 EDT (PC run: `crates/engine/src/package.rs`); don't redo.** Was: **S1-MANIFEST (added 2026-10-16 12:20 EDT, `crates/project` (or `crates/format`) + tests only, CPU):** S1 item 5's
    package manifest as pure types: `PackageManifest` (serde JSON: format version, FilmCraft version, project file, entries
    with relative path / size / SHA-256 / role (project, media, still, voice-over, font, shader, captions, proxy), and a
    `left_out` list with reasons), `verify(dir) -> Report` (missing / wrong size / wrong hash / extra files, paths that
    escape the package via `..` or absolute paths refused), bounded reads, streaming hash. Pure Rust SHA-256 only if the
    tree already has one (check `Cargo.lock`; otherwise write it under `crates/bitstream`-style rules with NIST vectors).
    Done = unit tests incl. hostile manifests (huge counts, `../` paths, bad hex), clippy, `cargo xtask wasm`. **PC owes:**
    nothing.
30. **~~S1-GUIDE-PACKAGES~~ DONE 2026-10-16 (PC runs: `network-storage.package`, `.package-trim`); don't redo.** Was: **S1-GUIDE-PACKAGES (added 2026-10-16 13:10 EDT, docs only, CPU):** draft `docs/guide/network-storage.md` (or a new
    `packages.md`, listed in README and `coach_ask.rs` PAGES) sections for S1 item 5 from STORAGE-PLAN's spec and
    `crates/engine/src/package.rs` (manifest, verify report): send a project (full / trimmed, folder / `.fcpackage`), what's
    in it and left out, import + verify; mark steps whose commands don't exist yet as "(coming)". Done = coach_ask header
    tests green. **PC owes:** final wording once `projects.package` lands.
31. **S1-NETFS-OTHER-OS (added 2026-10-16 13:10 EDT, `crates/engine/src/netfs.rs` + tests, CPU):** `classify` answers
    Unknown off Linux. Add pure parsers for macOS (`statfs` `f_fstypename`: smbfs / nfs / afpfs / webdav) and Windows
    (`GetDriveTypeW` DRIVE_REMOTE, UNC `\\server\share` paths) behind `cfg`, with the type mapping as a pure function
    tested on all hosts; any FFI goes in `crates/platform` only (AGENTS §0.3). Done = unit tests, clippy, `cargo xtask
    wasm`. **PC owes:** nothing (no macOS / Windows machine).
32. **Q45-BACKGROUND (added 2026-10-16 15:35 EDT, `crates/engine/src/coach_meaning.rs` + `coach_ask.rs` only, CPU):** search by
    meaning (`coach_meaning::meaning()`) loads the embedding model and embeds every guide section on the first caller's
    thread (minutes without the vector cache). Make it a background build: `available()` / `rank()` never block (they answer
    `None` / word search until the build is done), the build runs once on a named worker thread under `catch_unwind`, a
    failure is remembered (no retry storm). Done = a test with a fake slow loader (inject a loader fn, e.g. `#[cfg(test)]`
    seam) shows `rank` returns `None` at once while loading and `Some` after; Words mode untouched (`ai_pool` green);
    clippy with and without `--features llm`, `cargo xtask wasm`. PC owes: the real-model eval (`search_by_meaning_coverage`)
    once the model is on disk (Q51).
33. **S1-UI-THREAD-OPENS (added 2026-10-16 15:35 EDT, `crates/ui-egui` (`source_playback.rs` ~132, `media_dialogs.rs` ~136) only,
    CPU):** S1 item 3 owed: source-audio start and the Link Media preview still open files on the UI thread (up to 8 s on a dead
    share). Move both to the existing background-open pattern (`audio peaks open on their worker`, see `media_watch` /
    `netfs::share_call`). Done = a UI test with a simulated share (`netfs::test_mounts::add_share`, `project_views::use_mount_table`)
    whose reader blocks: the frame loop keeps running (frames advance) while the open waits; UI lib + touched panels' tests green.
    PC owes: nothing GPU.
34. **S1-FREE-DISK-FLOOR (added 2026-10-16 15:35 EDT, `crates/platform` (statvfs, the one crate allowed `unsafe`) + `crates/engine/src/net_cache.rs`, CPU):**
    Q49's 20 GB free-disk floor for network copies: a safe `free_bytes(path) -> Option<u64>` in `crates/platform` (Linux `statvfs`;
    other OS `None` for now), and `NetCache` skipping a copy that would leave the cache's disk under 20 GB free (a toast once per
    session). Done = unit test on the temp dir (`free_bytes` is `Some` and sane), a copier test with an injected free-space fn,
    ADR 0001 note for the new FFI call. PC owes: an Opus review of the FFI (`unsafe`) before merging.
35. **UPD-NO-DOWNGRADE (added 2026-10-16 20:00 EDT, `crates/engine/src/update.rs` + `update_tests.rs` only, CPU):** Opus
    review #7: `newer()` orders releases by `published_at`, so an old tag re-published later (draft → published) would be
    offered as an "update". Fix: when the running tag is in the list, never offer a release whose tag was first seen
    *before* it in a stable order (e.g. compare GitHub's `created_at` of the tag / release id, which only grows), and
    when the running tag isn't listed (page 1 only) fetch nothing more but keep the build-time rule. Done = tests for
    re-published old tag (not offered), normal newer (offered), held build (unchanged), plus the existing 25 update
    tests green. No GPU checks owed.
