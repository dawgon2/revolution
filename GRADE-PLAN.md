# GRADE-PLAN.md: a Resolve-style colour page for beginners and intermediates

Written 2026-10-11 (session 8, with the owner). **Approved by the owner 2026-10-11**, including
the phase order in §4 and the Resolve workflow (G1.8) and shot comparison / Shot Match (G2.8–9)
features. Recorded in FORK-PLAN §1a.6 and §1a.17; the runner
(`~/Apps/filmcraft-builds/run-fork-plan.sh`) picks these phases up like any other.

Goal: an editor who knows the basics of DaVinci Resolve's Color page can grade X-M5 F-Log2
footage in FilmCraft without missing anything they use day to day. Advanced and pro features are
explicitly deferred (§6). Every FORK-PLAN rule still applies: never crash, exact `Tick` time,
everything is a command, layering, `cargo xtask ci` green per phase, a build per phase (§1a.9).

---

## 1. What already exists (audited 2026-10-11, don't rebuild it)

| Need | Where it already is | Gap |
|---|---|---|
| Working space DWG / DI, F-Log2 / F-Gamut input, Rec.709 output | `crates/color` (`log.rs`, `spaces.rs`, `grade.rs`, `transform.rs`) | none |
| Highlight roll-off, gamut mapping | `transform.rs` BT.2390 tone map, `GamutCompress`, `DesaturateIntoGamut` | a per-grade "highlight" control (§3 G2.5) |
| Lift / gamma / gain / offset maths | `ColorOp::Lggo` (`crates/color/src/ops.rs:259`), GPU branch (`crates/gpu/src/colorops.rs:93`), parity test | **no effect or UI uses it** |
| Shadows / mids / highlights wheels | `ColorOp::Wheels`, Lumetri | no Offset wheel; luma not separated |
| Curves: RGB, luma, hue vs sat / hue / luma, luma vs sat, sat vs sat | `ColorOp::Curve`, `HslCurves` | none |
| HSL qualifier (key, soften, blur, denoise, show mask) | `ColorOp::HslKey` / `HslCorrect`, Lumetri HSL Secondary | invert, sat max, picker/eyedropper |
| Windows: ellipse, polygon, Bézier; feather, invert, combine | `crates/project/src/mask.rs`, `FxOp::Masked` | linear (gradient) window |
| Window tracking | `crates/render/src/track.rs` (Shi-Tomasi + pyramidal LK + RANSAC), background job `crates/engine/src/masks.rs` | hook to grade-node windows |
| Scopes: waveform, parade, histogram, vectorscopes | `crates/scopes`, GPU sampling `crates/gpu/src/scopes.rs` | none |
| Reference / comparison view | Program monitor Comparison mode, Reference Monitor panel | stills gallery, wipe / split slider |
| Copy / paste attributes | `crates/engine/src/clip_ops.rs:1024` | grade-specific copy / paste, append |
| Ordered effect stack | `TrackItem.effects: Vec<EffectInstance>` | no reorder command |
| Colour tests | `colorops_tests.rs`, `lumetri_chain_tests.rs` (oracle), `crates/golden` | node-grader oracle (§5) |
| Colour wheel / curve widgets | private fns in `crates/ui-egui/src/panels/lumetri.rs` (`curve_editor` :448, `color_wheel` :558) | extract to reusable widgets |

**Truly missing:** the grade / node model and its UI, an Offset wheel wired to `Lggo`, noise
reduction (none at all), film-like grain (today's "Noise" effect isn't luma-aware), stills
gallery and wipe, linear window, qualifier extras.

## 2. Design decisions (write them up as ADR 0004 in G0)

1. **A grade is a separate per-clip object, not an `EffectInstance`.** `TrackItem.grade:
   Option<Grade>` (`#[serde(default)]`, so old projects load unchanged). Effect params are a flat
   keyframable map, which doesn't fit a node list.
2. **Serial nodes now, a graph later.** `Grade { nodes: Vec<GradeNode>, bypass }`, where each
   `GradeNode` has a stable `id`, a label, `enabled`, and one corrector: primaries (LGGO + offset,
   contrast / pivot, saturation, temperature / tint), curves, an optional qualifier, windows
   (`Vec<Mask>`, reusing `mask.rs`) and an optional LUT. Ids (not indices) everywhere, so parallel
   / layer nodes (§6) become a DAG with input lists later without a project-format break.
3. **Where it runs:** input transform → **grade nodes (DI working space)** → effect stack
   (Lumetri included) → output transform. ADR 0003 already reserves this spot for the node grader.
4. **No new grading maths.** Each node compiles to the existing `ColorOp`s / `FxOp`s (windows and
   qualifier through `FxOp::Masked` / `HslKey`) and runs in the existing fused GPU pass. Only grain
   and noise reduction add new GPU kernels.
5. **Everything is a command** (`grade.*`: addNode, insertNode, removeNode, moveNode, toggleNode,
   setParam, resetNode, copy, paste, append, stills.grab / apply / delete, compare.set), each
   undoable through `Session::edit`, and every widget gets an automation id.
6. **Keyframes:** node params reuse the existing keyframable `Param` type.

## 3. Phases

Each phase ends like a FORK-PLAN phase: acceptance checks, `cargo xtask ci`, tag `grade-N`, build
in `~/Apps/filmcraft-builds/grade-N/` with NOTES.md, `latest` symlink, HANDOFF rewrite.
Estimates are agent working hours, assuming the pace of Phases 2–5.

### Phase G0: foundations (≈ 4–8 h)
1. ADR 0004 (the decisions in §2).
2. `Grade` / `GradeNode` types in `crates/project`, serde round-trip and old-project load tests.
3. Compile a grade to `ColorOp` / `FxOp` in `crates/render`; wire it between input transform and
   the effect stack; CPU and GPU paths; an identity grade is bit-exact.
4. Extract `color_wheel` and `curve_editor` from `lumetri.rs` into reusable widgets (`widgets.rs`
   or `widgets/color.rs`); Lumetri keeps using them, and its UI tests stay green.
5. `effects.move` (reorder) command, which was missing.

**Accept:** a project with a one-node grade saves, loads and renders identically on CPU and GPU;
an old project loads unchanged; play-graded 6.2K quick pass shows no regression.

### Phase G1: "Lite", the everyday grader (≈ 20–35 h)
1. **Node stack:** add / insert / delete / reorder / enable / bypass-all / label nodes; a
   horizontal node strip in a new Color workspace (simple painter widget, no graph library).
2. **Primaries panel:** Lift / Gamma / Gain / **Offset** wheels with master luma rings (wired to
   `ColorOp::Lggo`), contrast + pivot, saturation, temperature / tint. Defaults are identity.
3. **Curves panel** per node, reusing the extracted widget and the existing curve ops.
4. **Copy / paste / append grade** between clips (commands + menu + shortcuts); "apply grade to
   all selected clips".
5. **Stills and compare:** grab still (a frame + the grade that made it, stored in the project),
   gallery strip, apply a still's grade, wipe / split-screen slider in the Program monitor
   (extends Comparison mode).
6. **Film grain:** a new GPU kernel: luma-weighted, size / strength / softness, temporally
   stable seed per frame (deterministic, so golden tests work); offered as a node option.
7. Scopes follow the grade output (they already sample the composite, so check it only).
8. **Resolve workflow setting (owner, 2026-10-11):** Settings ▸ **Workflow: Premiere / Resolve**.
   *(Owner, 2026-10-12, FORK-PLAN §1a.39: Resolve is the fork's default, Premiere stays an
   option; the home screen, page chrome and page layouts continue in
   [`RESOLVE-UI-PLAN.md`](RESOLVE-UI-PLAN.md). Reference screenshots of every Color palette:
   `plan/resolve/screenshots/`.)*
   Resolve switches, in one go:
   - **Pages:** a page bar at the bottom (Media, Cut / Edit, Color, Fairlight, Deliver) mapped to
     FilmCraft workspaces (`crates/ui-egui/src/dock.rs` `workspace()`); the **Color page** layout:
     viewer centre, gallery left, node editor right, clip thumbnail strip, tool tabs below
     (primaries, curves, qualifier, window, tracker, scopes).
   - **Shortcuts:** a Resolve keymap preset next to "FilmCraft Default" and the Premiere preset
     (e.g. add serial node, bypass, grab still, wipe). Key assignments come from Resolve's public
     manual (functional facts); no Blackmagic assets, icons or code (clean-room).
   - **Defaults:** the colour panel opens the node grader instead of Lumetri; Resolve terms on
     the Color page.
   Switching back restores the Premiere workspaces and keymap; the user's own saved workspaces
   are kept. ≈ 10–20 h.

**Accept:** grade a synthetic F-Log2 clip with 3 serial nodes (primaries, curves, grain), copy
it to 5 clips, grab a still and wipe against it, all through MCP / control commands and
screenshotted; switch Workflow to Resolve and back (pages, keymap, Color page screenshotted); play-graded 6.2K quick pass ≥ 95 % of Phase 5's shown frames; oracle tests (§5).

### Phase G2: "Core", qualifiers, windows, noise reduction, node explainer, Grading Coach (≈ 63–117 h)
1. **Per-node qualifier:** reuse `HslKey`; add invert, sat max, a picker (click the viewer to set
   hue / sat / luma ranges), highlight mode (show matte / grey background).
2. **Per-node windows:** circle, polygon, Bézier from `mask.rs`, plus a new **linear (gradient)
   window**; on-viewer handles; softness, invert, inside / outside correction.
3. **Tracking for node windows:** drive the existing `track.rs` job from a node window; forward
   / backward / frame; results stored as keyframes like effect masks today.
4. **Spatial noise reduction** (the F-Log2 shadows need it): a GPU edge-preserving filter
   (bilateral or a small non-local-means), luma / chroma strength separately, as a node option.
   **Temporal NR is deferred out of G2 (owner, 2026-10-11)** to FORK-PLAN Phase 6, after the
   render cache (6.3) exists: motion-compensated, reusing the tracker's optical flow, 1–2 frames
   each side, applied at export and in the render cache (live playback only if the budget allows).
   Reason: too heavy for 6.2K playback without the cache, and no real X-M5 footage to tune
   ghosting on right now. G2 ships spatial NR only; G2's acceptance doesn't include temporal NR.
5. **Highlight control:** a per-grade highlight roll-off / "soft clip" knob feeding the existing
   BT.2390 output stage.
6. **LUT node** (reuse the existing LUT op).
8. **"Same grade?" and shot comparison (owner, 2026-10-11):**
   - **Same grade applied:** compare grade data; clips with identical grades share a marker /
     badge in the timeline and thumbnail strip, so a pasted grade and an accidental tweak are
     visible. Command `grade.compareData`.
   - **Do they look matched:** `grade.compareShots` (and a "Compare shots" button with the
     current clip and the still / reference clip) measures both *graded* frames with the existing
     statistics in `crates/render/src/color_match.rs` (Oklab, per-tonal-range L / a / b, chroma,
     skin-tone protection) and reports a match score plus plain-language differences ("B is ≈ ⅓
     stop darker, highlights slightly warmer, 8 % less saturated"). A scene check
     (`grade.checkScene` over selected clips / a range) flags the outlier shot. It is a guide,
     not a verdict: different framings legitimately differ, so compare overlapping tonal ranges
     and let the user mark a pair "intended". ≈ 8–16 h.
9. **Shot Match (moved up from deferred, owner 2026-10-11):** reuse the `color_match::solve`
   Levenberg–Marquardt solver, solving in a node's primaries (LGGO + saturation) instead of
   Lumetri's wheels, so "Match to still / reference" adds an editable node. ≈ 4–8 h.
10. **Node explainer (owner, 2026-10-11), measured facts first:** `grade.explainNode` /
    `grade.explainGrade` measure each node's input and output frame with the `color_match.rs`
    statistics (Oklab per tonal range, chroma, skin protection), read its controls (which wheels
    moved and where, curve bends, qualifier / window coverage as % of the frame) and produce
    structured facts plus a plain template sentence ("Node 2 'Skin': affects 18 % of the frame;
    inside it, midtones +⅙ stop, slightly less magenta, saturation −6 %; shadows and highlights
    untouched"). A "what changed" overlay shows where and how strongly the node acted. A one-line
    summary in the node strip, the full text in an Explain panel; cached per node state.
    ≈ 6–12 h.
11. **(Moved to Phase G2b, right after G2: owner, 2026-10-11.)** **Local AI explanations (owner, 2026-10-11):** an **optional, offline, quantized small
    language model** rewrites 10's facts in layman's terms ("this node warms up the skin a little
    and calms down the reds, without touching the sky").
    - Pure Rust inference on the CPU with **candle**, which `crates/speech` already uses for
      Whisper; reuse its model catalogue / verified downloader pattern (`crates/speech/src/models.rs`:
      source pinned to a revision, SHA-256, licence, per-user data dir). **Weights are never
      bundled or committed**: downloaded on first use after the user opts in (Settings ▸ Color ▸
      AI explanations), ≈ 0.4–1.2 GB.
    - Model: a small instruct model with an open licence, quantized to 4 bits (candidates to
      evaluate: Qwen2.5-0.5B / 1.5B-Instruct, Apache-2.0; SmolLM2-360M / 1.7B-Instruct, Apache-2.0).
      Pick by quality on a fixed set of 30 graded test cases and speed on this CPU (target: a
      short explanation within ~5 s, generated in the background).
    - **Grounded, never guessing:** the model only receives 10's structured facts and a fixed
      prompt; its output is checked (every number and direction it states must match the facts,
      length capped); on any mismatch, failure, timeout or missing model, the template sentence
      from 10 is shown instead. Inference runs under `catch_unwind` with memory and time caps
      (never crash). No network use other than the opt-in download.
    - Crate placement: a new L-level crate beside `crates/speech` (no UI deps, layering), behind a
      cargo feature that's off in wasm builds.
    ≈ 10–20 h.
11b. **Fine-tuned explainer (owner, 2026-10-11: "train them for use on our filmcraft fork").**
    After G2b's core (11, 12) works with a stock model: build a training set of facts →
    explanation pairs from G2.10's facts on many generated grades, keep only pairs that pass the
    G2.11 fact checker, LoRA-fine-tune the chosen base (SmolLM2 leaned, fully open data; decided
    by the stock 30-case evaluation), quantize to GGUF, and re-run the 30-case set (must beat the
    stock model, no fact-check regressions). Trainer, base-weight download and hosting of the
    tuned file: docs/fork/OWNER-QUESTIONS.md Q7. Training is offline tooling, never in the
    product; the tuned weights are never bundled or committed (opt-in download as in 11).
    **Owner, 2026-10-11 (Q7):** part of G2b now, as a test: test-train the two small bases
    (SmolLM2-360M-Instruct, Qwen2.5-0.5B-Instruct), pick the better one, and leave a repeatable
    training session (`docs/fork/TRAINING.md`: one command per stage: generate → train → quantize
    → evaluate). Trainer = mix: PyTorch + ROCm on the RX 6700 XT for LoRA (venv outside the repo)
    and candle on the CPU as the fallback; generator, evaluation and quantization in Rust.
    Settings lets the user choose which catalogue model to download. Host: a GitHub release on the public
    repo dawgon2/filmcraft-cachyos (owner, 2026-10-11; releases only, never git pushes there),
    published on the owner's "publish"; a local path until then.
12. **(Moved to Phase G2b, right after G2: owner, 2026-10-11.)** **Grading Coach: step-by-step guidance for beginners (owner, 2026-10-11):** so a beginner
    isn't lost in the node system. Three parts, all offline:
    - **Guided lessons (no AI needed):** a lesson engine that walks through a task one step at a
      time on the user's own clip (or a bundled sample frame from openly licensed footage, with
      attribution). Each step says what to do and **why**, **highlights the exact control** using
      the automation registry (`crates/ui-egui/src/automation.rs`: every widget's id and screen
      rect), and **waits until the step is actually done** by watching the engine commands
      (e.g. `grade.addNode`, `grade.setParam`), then moves on; Back / Skip / Exit at any time,
      progress remembered per user. Lessons are data files (`assets/lessons/*.toml`: steps,
      target automation id, expected command and param condition, text), so adding one needs no
      code. First set: *What a node is and how the chain flows*; *Your first grade* (balance →
      exposure → contrast → saturation → look, in that order and why); *Which node for what*
      (one job per node; a sensible starter tree for F-Log2: input CST → balance → exposure /
      contrast → secondaries → look → output); *Match two shots with stills and wipe*; *Fix skin
      with a qualifier*; *Brighten a face with a tracked window*; *Clean up noise in log shadows*.
    - **Ask the coach (uses G2.11's local model if installed):** a question box ("how do I make
      the sky bluer without changing skin?", "why did my skin go grey?"). It retrieves the
      relevant sections of **the fork guide** (`docs/guide/`, see its README: written for this
      fork, every section tagged [Resolve-style] / [FilmCraft] / [Fork] / [Standard] so the coach
      can say "this works like Resolve's …" or "this is a FilmCraft / fork feature"; clean-room:
      never copied from Resolve's or anyone's manual) with a small pure-Rust
      keyword / BM25 index, adds the current grade's facts (G2.10), and answers with numbered
      steps naming the real buttons. **Every control or command it names is checked against the
      live command list and automation ids**; anything unknown is removed, and if the answer
      fails the check (or there's no model) it offers the closest lesson instead. Steps in an
      answer can be clicked to highlight that control.
    - **Context hints:** the first time someone adds a node type, opens a panel or does something
      that usually confuses (e.g. a qualifier on a node after a heavy look), a small dismissible
      card says what it is, when to use it and links its lesson. Off with one switch.
    - Pure-Rust, never-crash rules as in G2.11; text is translatable via the existing i18n.
    ≈ 22–40 h (engine 8–15, lessons + guide text 6–10, coach 8–15).
13. **Resolve comparison (owner-run, optional):** Resolve is installed in `/opt/resolve`. The
   owner can render a few stills from Resolve with known settings (e.g. Offset +0.1, a qualifier
   on skin) so we can compare behaviour by eye. Never invent Resolve numbers (FORK-PLAN §6).

**Accept:** a skin-tone qualifier plus a tracked circle window on a moving synthetic subject
(motion fixture) hold over 5 s; NR visibly lowers noise on the F-Log2 fixture (measured: noise σ
in a flat patch drops ≥ 40 % without blurring an edge-sharpness patch by more than a set
threshold); play-graded with NR on is measured and recorded (synthetic); export matches the
viewer; `grade.compareShots` reports a near-perfect score for a clip against itself and flags a
deliberately offset copy (+⅓ stop, warm cast) with the right words; Shot Match brings that copy
back within a set tolerance; the node explainer's facts match known test grades exactly (template
text). G2 covers items 1–10; tag `grade-2`.

### Phase G2b: local AI explanations and the Grading Coach (G2.11, G2.12; right after G2)

Moved out of G2 by the owner (2026-10-11) so G2's grading tools land first; runs right after G2,
before FORK-PLAN Phase 6. The work is items 11 and 12 above, unchanged (G2.12's ask-the-coach
uses G2.11's model when installed and the guide index otherwise).

**Accept:** with the AI model installed the explainer's text passes the fact check on the
30-case set (and falls back cleanly to G2.10's template text with the model removed); every
lesson can be completed end to end through the control channel (each step's highlight hits a
real widget and its completion condition fires), and the coach's answers on a fixed set of 30
beginner questions pass the control-name check. Tag `grade-2b`, build per FORK-PLAN §1a.9.

### Phase G2c: white-balance picker and auto balance (owner, 2026-10-12; ≈ 6–12 h)

Added by the owner 2026-10-12: "a color dropper tool for white balance that automatically
balances it and the greens / purples". Runs right after FORK-PLAN Phase 6, before Y1 (§1a.6,
§1a.38). Today there is no white-balance picker anywhere (the qualifier picker and Effect
Controls' colour eyedropper exist and are reused).
1. **WB picker on a grade node** (Primaries, next to Temperature / Tint): click a point, or drag a
   box, on something that should be grey or white in the Program monitor. The sample is averaged
   over the box (5 × 5 px for a click) **at the node's input**, so picking again gives the same
   result instead of compounding. Clipped (near white), near-black and strongly saturated samples
   are rejected with a message ("pick something grey or white").
2. **Solve both axes at once:** find the node's **temperature (blue / amber) and tint (green /
   magenta)** that make the sample neutral (R = G = B in the working space), by numerically
   inverting the node's existing temperature / tint operation (no new grading maths). Two
   controls, two chromaticity coordinates, so it's exact within the slider ranges; if the cast
   needs more than the range, clamp and say so. One undo step; the sliders show the result and
   stay adjustable.
3. **Auto balance** (button, no click): estimate the neutral from the frame (robust grey-world
   over mid-tone, low-saturation pixels, excluding clipped areas), then the same solver. Also
   in **Lumetri ▸ Basic Correction** as Premiere's **WB Selector** (sets Lumetri's Temperature /
   Tint the same way; Premiere parity, a candidate to contribute upstream).
4. Commands `grade.wb.pick` (point or rect, uv), `grade.wb.auto`, `lumetri.wb.pick`; automation
   ids on every button; the Coach / guide (`color/primaries.md`) explain when to use it.

**Accept:** synthetic grey-chart frames with known casts (warm, cool, green, magenta and
combinations, in F-Log2 and Rec.709) come out neutral within ΔE 2000 < 1 on the picked patch;
picking twice changes nothing; auto balance lands within ΔE < 3 on the charts; clipped / black /
NaN / out-of-frame picks never crash and explain. Tag `grade-2c`.

### Phase G3: Magic Mask, ML subject isolation (owner, 2026-10-12; ≈ 40–80 h)

Added by the owner 2026-10-12 ("definitely something useful"); runs after AI-PLAN AI1 and AUDIO-PLAN A2,
whose run-time-dispatched candle inference layer this reuses (FORK-PLAN §1a.6, §1a.35).
1. **Spike first:** pick a permissively licensed (Apache-2.0 / MIT) promptable segmentation model
   with video propagation (e.g. the SAM 2 family) that candle can run; measure per-frame time on
   the i5-11600K CPU (candle has no AMD GPU backend) at preview and full resolution. Write the
   choice and numbers in §7. **The model download needs the owner's yes when G3 starts** (his
   A2 consent covers only the audio model). **Given 2026-10-21 (FORK-PLAN §1a.119 (iii)).**
2. **Tool:** on a grade node, click / scribble on a person or object (or "person" / "face" /
   "clothing" quick modes if the model supports them); the mask propagates forward and back as a
   background job (`catch_unwind`, cancellable), cached per frame like the render cache, and
   becomes a matte for the node exactly like a window or qualifier (`FxOp::Masked`), with
   refine controls (grow / shrink, soften, invert, clean black / white) and per-frame strokes to
   fix drift.
3. Commands `grade.magicMask.*`; the mask is stored per node (stroke prompts + cached mattes,
   regenerable), so projects stay small and load without the model.

**Accept:** on test clips with a known matte (synthetic composites of a person over varied
backgrounds, plus `~/Videos` clips judged by eye, §5) the mask's IoU stays above a target set in
the spike over 10 s of movement; a fix stroke updates only the affected frames; no model or a
corrupt model never crashes and says why. Tag `grade-3`.

### Phase G4: advanced grading tools (owner, 2026-10-12: later; ≈ 50–90 h)

Runs after COMPOSITOR-PLAN C4 (its planar tracker is reused here); the order can change.
1. **Colour warper:** hue / saturation mesh (and chroma / luma) with draggable points, compiled
   to a 2D LUT `ColorOp` in the fused pass.
2. **HDR-style zone wheels:** extra tonal zones (dark / shadow / light / highlight / specular)
   with adjustable ranges and falloff, plus global, as a corrector option on a node.
3. **3D qualifier:** strokes in the image pick a volume in colour space (soft, despill), as an
   alternative key to the HSL qualifier.
4. **Planar-tracked windows:** windows follow C3's planar tracker (signs, screens, walls).
   *Built (G4.4):* `Mask.planar: Option<PlanarTrack>` (a homography per frame, serde default, so
   old projects save byte-identical; hostile keys dropped on load) rather than baked path
   keyframes, because a projected Bézier isn't a Bézier: `render::mask::prepare` maps the
   *flattened* outline through it, so CPU and GPU (same polygon) stay identical. The stored path
   stays in the reference frame; on-monitor handles show / drag it where it is seen.
   `masks.trackPlanar` (both ways from the playhead, background job, frames streamed one at a
   time at ≤ 1920 px, `jobs.cancel`, one undo step; re-tracking composes with the old track) and
   `masks.clearPlanar`; Color page **Planar** / **✕ planar** (`grade.window.N.track.planar` /
   `.clearPlanar`). Synthetic perspective plane: corners within 0.016 px over 60 frames.
Tag `grade-4`.

### Later still: deferred (§6)

## 4. Order against FORK-PLAN (owner's decision)

Remaining FORK-PLAN phases: 6 (4:2:2, proxies, render cache), 7 (polish, soak), 8 (backup
backends). **Current order: FORK-PLAN §1a.6** (G0 ✓ → G1 ✓ → U0 ✓ → G2 → G2b → 6 → Y1 → 6E → U0.2 → Y3 → C1 → C2 → 7 (+ Y4) → D → 8 → Y2 → H1 → H2 → H3). The reasoning below was for the first version.
- G0 / G1 first: they're what you'll use, and graded 6.2K already plays at 1798/0.
- Phase 6 before G2: temporal NR and heavy multi-node grades are exactly what the render cache is
  for, and 4:2:2 clips need proxies before grading them is pleasant.
- Phase 7's soak test then covers the grader too. Phase 8 stays last.

## 5. Testing

- **Oracle:** a CPU per-pixel reference for a node chain (like `lumetri_chain_tests::reference`),
  checked against the compiled `ColorOp` chain on CPU and GPU over randomised settings; an
  identity grade is bit-exact.
- **Golden images** (`crates/golden`, `FILMCRAFT_BLESS=1`) for a handful of fixed grades,
  grain with a fixed seed, and NR.
- **Hostile input:** fuzz-ish tests for grades loaded from a project file: absurd values, 10 000
  nodes (capped), unknown fields, NaN / ∞ params; never crash.
- **UI:** `crates/ui-egui/tests` for the node strip and panels; screenshots through the
  `filmcraft` MCP server for each phase's acceptance.
- **Pictures to look at:** synthetic fixtures remain the benchmark source (§1a.3). For judging
  grades by eye, real-camera clips in `~/Videos` (e.g. the `MVI_*.MP4` files) can be used
  read-only, never committed, with results marked as not-X-M5.

## 6. Deferred (advanced / pro)

Parallel, layer, splitter / combiner nodes, key mixer, outside nodes (now scheduled: COMPOSITOR-PLAN C2, after C1); HDR wheels, colour warper,
3D qualifier, planar tracking (now scheduled: G4, owner 2026-10-12); grade groups (pre / post clip), timeline
grades; halation, glow; Magic Mask (now scheduled: G3, owner 2026-10-12; pure-Rust candle inference); ACES, Dolby Vision; matching
every control's exact feel to Resolve. The §2 data model keeps all of these possible.

## 7. How the work is run (subagents and sessions)

**Sessions.** Unchanged: the runner starts one fresh `claude -p` session per phase; each session
reads FORK-PLAN, this file and HANDOFF, finishes the phase, builds, rewrites HANDOFF and stops.
The runner already waits 30 min and retries after a usage-limit stop.

**Inside a session, the main agent is the integrator.** It owns the plan step, the shared types,
every commit and the quality gates. Subagents do bounded side work and report back conclusions,
not file dumps.

| Role | Agent / model | Used for | Builds? |
|---|---|---|---|
| Scout | Explore, cheap model (Haiku / Sonnet) | "where is X", call sites, how a subsystem works | no |
| Test author | general-purpose, Sonnet | writes the oracle / hostile-input tests for a step from its spec, in parallel with the implementation | crate only |
| Leaf builder | general-purpose, Sonnet or Opus, `isolation: worktree` | self-contained work in one crate: grain kernel, NR kernel, linear window shape, widget extraction | crate only, own `CARGO_TARGET_DIR` |
| Reviewer | general-purpose, Opus, fresh context | reviews each step's diff before commit: never-crash rules, hostile input, layering, clean-room, missing tests (same idea as §1a.5) | no |
| UI checker | general-purpose with the `filmcraft` MCP tools | drives the app, takes screenshots, reports what looks wrong | release build |

**Parallel lanes per phase** (start a lane only once the types it uses are committed):
- G0: main = types + render wiring; leaf builder = widget extraction (ui-egui only); test author =
  serde / old-project / identity tests.
- G1: main = node commands + Color workspace; leaf builder = grain kernel (`crates/gpu`); second
  leaf builder = stills / wipe in the monitor once the commands exist, then the Resolve keymap
  preset (`crates/engine`, no GPU); test author = node oracle.
- G2: main = qualifier + windows on nodes; leaf builder = NR kernel; leaf builder = linear window
  + tracking hook, then shot comparison / Shot Match (`crates/render`); reviewer on every merge.

**Limits that matter more than agent count:**
- **Disk:** `target/debug` was 114 GB; after deleting `target/debug/incremental` (2026-10-11)
  `/home` has 95 GB free (20 GB floor, §1a.12). A second full-workspace target dir still doesn't
  fit comfortably. So leaf builders only build their own crate
  (`cargo test -p <crate>`, own `CARGO_TARGET_DIR=target/agent-<name>`, `CARGO_INCREMENTAL=0`),
  at most two at once, and each deletes its target dir when it is merged.
- **One GPU:** benchmarks and screenshot checks run only in the main session, never while a
  builder is compiling (results would be skewed).
- **Usage (Max 5x):** parallel agents finish sooner in wall-clock time but use the 5-hour and
  weekly limits faster. Cheap models for scouting and tests, Opus for the integrator and reviewer,
  crate tests per step and one full `cargo xtask ci` per phase (§1a.11). Subagent prompts ask for
  answers under ~500 words.
- **Integration:** worktree branches are merged by the main agent only, one at a time, each after
  the reviewer passes it and the touched crates' tests are green.

## 8. Rough timeline (agent hours → calendar on Max 5x)

| Phase | Agent hours | Calendar estimate |
|---|---|---|
| G0 | 4–8 | 1–2 days |
| G1 (incl. Resolve workflow) | 30–55 | 6–10 days |
| (FORK-PLAN 6) | as planned | ~2–4 days |
| G2 (incl. shot compare + Shot Match) | 37–70 | 1.5–2.5 weeks |
| **Lite + Core** | **≈ 70–130** | **≈ 4–5 weeks** |

The limiting factor is the usage cap, not engineering: when the runner hits a limit it waits
and resumes, so the calendar stretches but nothing is lost. Without Phase 6 in between, Lite +
Core is about 3.5–4.5 weeks.
