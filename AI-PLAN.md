# AI-PLAN.md: local AI that runs well on every CPU

Written 2026-10-12 (live session, with the owner). Order: FORK-PLAN §1a.6 (AI1 right after C2,
before AUDIO-PLAN A2 and GRADE-PLAN G3, which build on it). Every FORK-PLAN rule applies (never
crash, everything is a command, layering, `cargo xtask ci` per phase, a build and a `cachyos`
publish per phase). All inference stays local: models are optional, downloaded once on demand,
checksummed, never bundled or committed; without a model the app falls back cleanly.

## Phase AI1: portable fast inference (≈ 8–15 h)

Today candle's quantized kernels only use SIMD when AVX2 is enabled at compile time, so the fork
builds for `x86-64-v3` (OWNER-QUESTIONS Q8, provisional (a)); without the flag an explanation
takes ~17–30 s instead of ~4 s. AI1 implements Q8 option (c) and retires the flag.
1. **Own quantized forward pass** in `crates/llm` (from candle's MIT / Apache-2.0 code, ~860
   lines, attribution kept), with the heavy matrix maths through `gemm`, which picks AVX-512 /
   AVX2 / SSE / NEON at run time. No `unsafe` in our code; one binary for every CPU.
2. ✓ (2026-10-13) **Delete the `x86-64-v3` block in `.cargo/config.toml`**, resolve Q8 in OWNER-QUESTIONS.md, rebuild.
   Done: candle's f32 ops checked without the flag (`crates/speech/examples/candle_ops_bench.rs`):
   gemm-backed ones unchanged; Kokoro's transposed conv and last-dim mean now go through gemm.
3. ✓ (2026-10-13, AI1.3a–c) **Feels instant everywhere:** stream tokens to the UI as they're generated; pre-generate the
   node explanation in the background (cancellable, `catch_unwind`) while the user adjusts, so it
   is usually ready before it's asked for; keep the template fallback for slow machines or no model.
4. ✓ (2026-10-14, `smol135-v1`, 34 / 36 + 36 / 40 + 13 / 13 at ~0.6 s; catalogued as the "fast" option, TRAINING.md) **Smaller tune (try, keep only if it passes):** SmolLM2-135M through TRAINING.md's pipeline
   (SmolLM2 is the base, §1a.27); ship it as a "fast" option only if it matches the 360M tune's
   fact-check pass rate within a few cases.
5. ✓ (2026-10-13, `crates/ml`) **Shared runtime for A2 / G3:** the run-time CPU dispatch and background-job plumbing are a small
   reusable layer that AUDIO-PLAN A2 (voice isolation) and GRADE-PLAN G3 (Magic Mask) use.

6. **Retrain at the end of AI1 (owner, 2026-10-12, FORK-PLAN §1a.49):** a new SmolLM2 tune,
   **`smol360-v2`**, on everything the explainer should know by then: the existing serial-grade
   data plus what shipped since v1 (G2c white balance / Auto balance, 6.7b clip-level Temporal NR,
   Y3's keys / effects where the explainer covers them), TRAINING.md's v2 ideas (more epochs,
   more near-threshold colour / saturation cases), and the owner's logged misses in
   `docs/fork/COACH-QUESTIONS.md` where they're explainer-shaped. Trained on the RX 6700 XT
   (~1 h per run), run through the new runtime (item 1), evaluated on both held-out sets plus a
   small new set for the features added since v1. Ships first in `TUNED_PREFERENCE` and is
   published as `explainer-smol360-v2` (§1a.33) only if it beats v1 (see Accept).

**Accept:** with the flag removed, `smol360` (Q8_0) scores the same on both held-out sets (75
cases) and answers in **≤ 2.5 s** mean on this PC (target ~2 s); a forced-SSE run (dispatch
override for tests) still works and its time is recorded; first streamed words appear in
< 0.5 s; results are identical to the old path within the fact checker; no model or a corrupt
model never crashes; **`smol360-v2` scores ≥ v1 on both old held-out sets and passes the
new-features set** (otherwise v1 stays the default and the reason is logged). Tag `ai-1`.

## Phase AI2: teach the explainer the compositor and branch nodes (≈ 6–12 h; owner, 2026-10-12)

Right after C2 (FORK-PLAN §1a.48). C1 adds comp graphs and C2 adds parallel / layer / splitter-
combiner / key-mixer / outside nodes, all with template explanations (C1 item 7, C2's Coach
lessons); this phase trains the local model on them so its explanations are as good there as on
serial grades.
1. **Data:** extend the training-data generator (`tools/explainer-train/`) with comp-graph and
   branch-node cases (sampled graphs → the template explainer's facts → target sentences), keeping
   the existing serial-grade data; near-threshold cases for blend modes, key strength, opacity.
2. **Fact checker:** extend it to comp nodes and branch nodes (only real controls and real values;
   rejects invented ones), with tests.
3. **Held-out sets:** a third held-out set for comp graphs and a fourth for branch-node grades
   (never seen in training), plus the two existing sets (no regression allowed).
4. **Train** the SmolLM2 tune (`smol360-v3`, starting from AI1's v2 data and recipe; also the 135M "fast" tune if AI1 kept it) on the RX
   6700 XT per TRAINING.md (~1 h per run); Qwen stays frozen (§1a.27).
5. **Ship:** new version first in `TUNED_PREFERENCE`, catalogue entry pointing at its release
   asset, published as `explainer-smol360-v3` (§1a.33; never `manifest.private.json`).
6. **Coach:** add the C1 / C2 questions to `docs/fork/coach-eval/questions.toml` if C1 / C2 didn't,
   and re-measure.

**Accept:** on all held-out sets the new tune's fact-check pass rate ≥ v2's on the old sets
and ≥ the template explainer's coverage on the new ones; mean answer time within AI1's ≤ 2.5 s;
no crash on hostile graphs. Tag `ai-2`.

## Phase AI2b: retrain on what shipped up to 0.3.0 (owner, 2026-10-22, FORK-PLAN §1a.128; ≈ 6–12 h)

Right after Revolution 0.3.0 is published (after G4). Same recipe as AI2, on everything added since: A3 mixer,
Y2 VFR, G3 Magic Mask + A3.7 voice isolation, C3 trackers / planar / roto / paint, C4 expressions / groups /
templates, G4's grading tools. 1. **Data:** extend `tools/explainer-train/` with those node / tool cases (template
explainer facts → target sentences), keeping all older data. 2. **Fact checker** knows the new controls. 3.
**Held-out set 5** for the new features; sets 1–4 must not regress. 4. **Train** `smol360-v4` on the RX 6700 XT
(TRAINING.md). 5. **Coach:** add questions for the new guide sections to `docs/fork/coach-eval/questions.toml`,
re-measure `ai_pool` / coach floors (raise, never lower). 6. **Ship** as `explainer-smol360-v4` (§1a.33) if it
beats v3 on sets 1–4 and covers set 5 at least as well as the template explainer. Tag `ai-2b`.

## Phase AI3: the grading assistant, a model-backed "Ask the coach" (owner, 2026-10-14; ≈ 70–120 h)

Owner: "I want to be able to ask it advanced questions about how to grade. What nodes do what,
what to do if I want to grade in a specific way, how to tweak something just right, what a
certain tool in the app is and how to use it, etc. We can use a bigger model if needed."
Today's Ask the coach (G2.12 part 2) is a keyword search that returns one guide section's steps;
the local model only rewrites node explanations. AI3 makes the coach a real assistant, grounded so
it can't invent buttons or steps.

**Scope (owner, 2026-10-14 ~12:55, FORK-PLAN §1a.75): all of editing, not only grading.** The
corpus, feature pages, mapping, training pairs and eval set cover every feature built when the
tune runs, the Compositor (C1) included. Features not built yet aren't written ahead; each later
phase adds its pages, pairs and eval questions, and the tune is refreshed as they land.

0. **Owner additions (2026-10-14): real professional practice, every feature, mapped, token-lean.**
   "We should feed it in-depth detail from real data of people doing professional work in resolve
   and premier. We should also add an in-depth explanation of every feature in our app and how it
   works (especially ones that are unique). Then we should train it on how what those people did in
   resolve and premier work in our app. We should make the guide have sections that is easily
   parsable to save on tokens." So:
   a. **Professional practice corpus (sources rule):** collect how working colourists and editors
      actually do things in Resolve and Premiere (node trees, grading orders, look recipes,
      matching, skin, log / HDR delivery, editing habits). Openly licensed material (CC-BY,
      CC-BY-SA, CC0 / public domain, permissively licensed docs) may be quoted and used verbatim in
      training data, with a `.attribution` entry like assets. Everything else (tutorial videos,
      courses, forum posts, blogs, Blackmagic / Adobe manuals) is a **reading source only**: the
      techniques and facts are written up in our own words as "practice notes" (one technique per
      note: goal, when, steps in Resolve / Premiere terms, why, pitfalls), never transcripts,
      quotes or copied tables; the source list stays maintainer-local (`plan/`), not in the repo
      or the published model. Clean-room (CLAUDE.md, GRADE-PLAN G2.12): never copy a manual. No
      scraping against a site's terms; video transcripts aren't stored. ≈ 10–20 h of research and
      writing, ≥ 150 notes.
   b. **Every FilmCraft feature explained in depth** (fork guide): what it is, how it works
      inside (the model behind it, e.g. DWG / DaVinci Intermediate working space, the HSL
      qualifier's soft edges, Temporal NR, the node explainer's measurement), when to use it, its
      limits; unique fork features first (Grading Coach, node explainer, two-GPU export, proxies /
      render cache, Linux integration…). Every control named by `{ui:}` id / command. ≈ 15–25 h.
   c. **Mapping: Resolve / Premiere practice → FilmCraft.** For each practice note, the FilmCraft
      way (same tool, closest equivalent, or "not yet", logged as a gap). Then **training pairs**
      generated from notes + mapping + feature pages (question → grounded answer with the
      FilmCraft steps), human-reviewed samples, used to tune the chosen model (TRAINING.md
      pipeline on the RX 6700 XT) and as eval cases. Gaps feed `docs/gaps.md`.
   d. **Token-lean, parsable sections:** every guide section (and practice note) starts with a
      fixed compact header the retriever sends instead of prose when that is enough, e.g.
      `> id: color.qualifier.hsl | does: isolate a colour range | ui: grade.qualifier.* |
      cmd: grade.qualifier.* | resolve: Qualifier (HSL) | premiere: Lumetri HSL Secondary |
      when: skin, sky, single colours | see: color.windows`, then short numbered steps, then the
      long explanation. A test checks every section has a valid header; the retriever builds an
      index from headers and sends header + steps first, the full text only if the model asks
      (≈ 3–5× fewer prompt tokens). Applies to existing pages too (one pass over the guide).
1. **Knowledge first: the guide grows an advanced grading part** (clean-room, written for this
   fork, tagged as today): what each node type and tool does and when to use it; grading
   approaches (look development, matching, skin, log workflows, F-Log2 → DWG / DaVinci
   Intermediate, contrast shaping, highlight roll-off, film-style looks, day-for-night…) written as
   options with trade-offs (§1a.47 b, never "the one right way"); "how to tweak X just right"
   notes per control (ranges, what overdoing looks like, pairs that interact). Every app-specific
   step names real `{ui:}` ids / commands. ≈ 12–20 h. This is most of the quality.
2. **Retrieval that understands questions:** keep BM25, add a small local embedding model (or the
   chat model's own embeddings) so "node system", "why is my skin grey", "make it look like
   film" find the right sections; top-k sections (not one) go to the model, plus the current
   grade's facts (G2.10 explainer) and what's on screen (page, selected node / tool). ≈ 4–6 h.
3. **A bigger chat model, still local and offline:** pick by a measured eval (item 6), candidates
   with open licences that allow shipping: Phi-4-mini (3.8B, MIT), SmolLM3-3B (Apache-2.0),
   Qwen2.5-7B-Instruct (Apache-2.0), plus today's small tunes as the fast fallback. Run on the
   AI1 runtime (CPU, any machine; streaming; Q4/Q5); measure tokens/s on this PC and the laptop
   (Core Ultra 7 155H). GPU inference (wgpu) only if CPU is too slow for a 3–4B model: a separate,
   bigger step, decide on the numbers. Model downloads need the owner's yes per model (size,
   source). ≈ 6–10 h.
4. **Grounded answers, checked:** the model answers from the retrieved sections and the grade's
   facts; app steps must come from the guide (every named control / command checked against the
   live app as today, unknown ones removed); general colour-science / grading advice the guide
   doesn't state is allowed only when marked as general advice, never as a FilmCraft step; "I
   don't know" beats a guess. Sources listed under the answer. ≈ 6–10 h.
5. **A chat, not a box:** follow-up questions keep context ("and if it's backlit?"), steps stay
   clickable (outline the control), "Show me" starts the matching lesson, answers stream. The
   current model-free coach stays as the fallback without a model. ≈ 4–6 h.
6. **Eval:** an advanced question set (≥ 60: node roles, approaches, tweaks, tools, multi-step
   "how do I get this look") with expected sources and must / must-not facts; score each
   candidate model + retrieval; the owner's misses go to `docs/fork/COACH-QUESTIONS.md` and the
   set. Optional tune on the guide (TRAINING.md pipeline) if a base model misses the floor. ≈ 4–8 h.

**Accept:** every guide section has a valid compact header; ≥ 150 practice notes with mapping
(sources rule followed: no verbatim non-open text in the repo, the training data or the model);
every feature in the command list has a feature page section; on the advanced set (now including
"how would a Resolve / Premiere pro do X here" questions) the chosen setup answers ≥ 85 % correctly with correct sources
and **no invented controls, commands or steps** (any invented one fails the case); first words in
< 1 s and a typical answer in < 10 s on this PC (laptop time recorded); works offline; no model,
a corrupt model or a slow machine falls back to today's coach and never crashes. Tag `ai-3`.

## Phase AI4: the assistant sees your footage (owner, 2026-10-15; right after AI3; ≈ 55–100 h)

Today the assistant (AI3) answers from the guide and the page name only: it doesn't see the picture,
and the Assistant panel doesn't even send the grade and selection the engine already accepts. AI4
makes answers fit the shot in front of you. Design (owner, 2026-10-15, FORK-PLAN §1a.83): **measured
facts + a short description, both as text, given to the same assistant model** (SmolLM3 stays the
one that answers: it is proven on the eval and grounded; the vision model only describes). Order:

1. **Measured picture facts (≈ 15–25 h):** from FilmCraft's own scopes on the current frame (after
   the input transform and, separately, after the grade): exposure (clipped / crushed share, where
   the mids sit), white balance (cast of near-neutral areas, in plain words and K / tint), saturation,
   skin against the skin-tone line, contrast, log vs display-referred. A compact "fact sheet" text
   plus the grade's facts (G2.10) and the selection, sent with every assistant question (the panel
   and the Ask box); cached per frame. Useful on its own, before any vision model.
2. **Vision model: Qwen3-VL 4B Instruct** (approved by the owner 2026-10-15, download yes, **after a
   check that its licence is Apache-2.0**; 4-bit ≈ 3 GB): run on FilmCraft's own inference (the
   SigLIP-style image encoder on wgpu, AMD + Intel, CPU fallback; the language part on the AI3 GPU
   path). It writes one or two sentences about what matters for grading: light sources and their
   colour and direction, faces, sky, practicals, time of day, interior / exterior. Made once per
   shot (cached), only when a question needs the picture; unloaded after, so editing keeps its VRAM.
   ≈ 20–40 h.
2b. **Question-aware looking (owner, 2026-10-15, FORK-PLAN §1a.89; ≈ 10–20 h):** when a question needs
   the picture, the vision model gets **the user's question with the frame** (and the measured facts),
   not only a generic description request, and writes what it sees that bears on that question ("the
   lamp's glow is orange, the walls are neutral grey-brown, no skin in frame"). Still text handed to
   the assistant, which answers with the guide's steps; the generic description stays cached per shot
   for questions that don't need a fresh look. Scored on the AI-POOL `needs_clip` questions with
   fixture frames (does the look mention what the question is about; no invented content checked on
   frames with known content).
3. **Tune it for grading descriptions (≈ 20–35 h):** LoRA on the language part, image encoder frozen
   (the explainer's tune pipeline, AI1 / G2.11). Training frames: the owner's own X-M5 footage
   (**approved 2026-10-15; frames stay local, never in the repo, a published model's data or a
   release**), FilmCraft's test clips, generated frames with known lighting, openly licensed images
   (licences checked). Labels: the measurable parts from the scopes, the rest written / checked by
   hand; **the owner reviews a sample before training** (as for the explainer). Kept only if a
   held-out frame set scores clearly better than the base model.
   **(Owner, 2026-10-16, FORK-PLAN §1a.96 (c): no X-M5 footage yet, so synthetic frames + downloaded openly
   licensed footage by others, log footage required (F-Log2 first); licences logged in
   `coach-eval/VISION-SOURCES.md`. The tune runs in AI4 regardless of the owner's sample review, which is
   not a gate (later corrections → re-tune), Q44.)**
5. **Train and test on the larger question pool, once AI-POOL is done (owner, 2026-10-15, FORK-PLAN
   §1a.86):** (a) **test** the assistant (and the AI4 picture-aware answers) on AI-POOL's held-out
   set: the ~150-question daily sample in every AI4 step's check, the full ~900 at acceptance;
   (b) **train**: tune the assistant model (SmolLM3-3B, LoRA, the explainer's pipeline on the RX
   6700 XT) on AI-POOL's training pairs (the `needs_clip` ones with their picture facts), after the
   owner has reviewed a random 30–50 pairs (Q35 rule); keep the tune only if it beats the untuned
   model on the held-out set with 0 invented, else keep the untuned one and log why. The vision
   tune (item 3) may use the pool's `needs_clip` questions too. If AI-POOL isn't done when AI4's
   other items are, AI4 waits for it (or the owner says otherwise).
   (c) **Open-ended creative questions, not only what the guide says (owner, live, 2026-10-15, FORK-PLAN
   §1a.90).** The owner's example: "how do I remove the person from this clip and replace it with a
   translucent ghost figure instead". No guide section answers that; a good answer is a **plan built from
   several tools**, e.g. clean plate (a frame or second take without the person; tripod shots only), mask /
   roto the person on a duplicate layer and track the mask, drop the person from the base layer with the clean
   plate, then the ghost: the masked person at 20–40 % opacity, Screen / Add blend, blur + glow, a light
   desaturate / cool tint, optional echo or offset; plus what FilmCraft can't do yet (object removal / fill,
   Magic Mask before G3) said plainly with the closest workaround, and options with trade-offs (§ guide rule:
   never one correct way). More of the kind: "make it look like a security camera / VHS / old film", "freeze
   the frame and make the colour drain out", "clone myself in the same shot", "make the sky stormy", "a
   sudden earthquake shake", "the phone screen shows something else", "turn day into night", "a
   whip-pan transition between two places", "make the music duck when I talk without keyframes",
   "subtitle style where words pop as they are spoken". Work:
   1. `creative-*` AI-POOL batches: held-out questions + training pairs whose answers are multi-step
      **recipes across sections** (`sources` = every section used, ≥ 2), each step naming real controls /
      command ids; a `gap` part where a step isn't possible yet (expected wording: says so + workaround).
      Mixed difficulty: some fully possible, some partly, a few not at all (must say so, no invented tool).
   2. Prompt / retrieval: such questions need sections for each sub-task, not one best match: split the
      question into sub-goals (mask, track, opacity, blend, blur …) and retrieve per sub-goal (a decomposition
      step before retrieval; measured on the creative held-out set, other sets must not lose).
   3. Eval: score creative questions apart from guide-lookup questions (plan covers the expected sub-tasks,
      every named control real, gaps admitted, 0 invented); the tune is kept only if creative doesn't
      fall. Where the guide lacks a reusable technique the recipes keep needing (clean plates, ghost /
      double-exposure, clone shots …), add a practice note (options + trade-offs) so answers can cite it.
   CPU-only except the model eval: good MINIMAL / LEAN work for the main runner, the Dell lane or CLOUD-QUEUE.
   **Size (owner, live, 2026-10-15 ~22:45): a creative set as large as the current pool**: ~850 held-out
   creative questions and ~800 creative training pairs (the pool today: 846 advanced held-out, 811 pool pairs).
   How, so the numbers stay honest:
   - **Split by goal family, not by question.** A family is one creative goal ("person → translucent ghost")
     with its variants (tripod vs handheld, one person vs a crowd, short vs long shot, beginner vs pro
     wording); a whole family is either held out or training, never both, so the test can't be passed by
     having seen a reworded copy. ~3–5 questions per family → roughly 200+ held-out families and 200+
     training families (~400–450 distinct goals across looks, compositing, transitions, motion / titles,
     audio tricks, social formats, repair jobs). Recorded per item as `family` and checked in `ai_pool.rs`.
   - **Held-out first.** Write the test families before the training ones (the test is what measures
     progress); a first ~300 held-out are enough to start scoring creative answers.
   - **Expected answers as sub-goals**, not one text: each held-out item lists the sub-tasks a good plan
     covers (e.g. clean plate, mask, track, opacity, blend, glow), the ones FilmCraft can't do yet (gap +
     workaround wording), and that nothing may be invented; the scorer checks those automatically,
     plus the existing `ground` checks. Training answers are full recipes with real controls / command
     ids.
   - **Check the recipes work.** Every command id / control is checked automatically; a random sample of
     ~1 in 20 recipes (fully possible ones) is run through the headless app over MCP to see that the steps
     really produce the effect; a recipe that doesn't gets fixed and its family re-checked.
   - **Gaps are data.** Goals FilmCraft can't do (object removal / fill, roto, 3D, particles…) stay in
     the set as admitted-gap cases (aim ≤ ~1/3 of families) and their gaps go to a list for the roadmap.
   - **Owner read** of 30–50 random creative pairs before any tune uses them (Q35 rule).
   - **Cost / order:** answers are ~3× longer than lookup pairs and need more thought, so this costs
     roughly 2–3× AI-POOL; it runs as a lane (the Dell lane or the main runner in small batches) paced by
     §2c, never at the expense of the weekly line. The AI4 tune can start with whatever training families
     exist; the creative held-out set is scored from its first ~300 on.
   - **Who (owner, live, 2026-10-15 ~22:55, FORK-PLAN §1a.91):** the batches are the Dell lane's (CREATIVE-POOL);
     the main runner does items 2–3 (retrieval, scorer) and the practice notes the lane asks for.
4. **Tiers:** 4B on a discrete GPU with enough free VRAM; measured facts only otherwise (and on
   request). **Qwen3-VL 2B for lower-end hardware is deferred** (owner: "only do the 4B for now";
   ≈ 4–8 h later: same code, one download, one tune).

**Accept:** AI-POOL's full held-out set scored (item 5), with the tuned or untuned assistant as item 5
chose; on a set of test frames with known content and known problems (the "needs_clip" coach
questions with fixture clips, ASSISTANT-PLAN-DRAFT §3), answers use at least one picture fact and stay
grounded (0 invented controls); the AI3 85-case eval doesn't regress; the description adds < 1.5 s the
first time per shot on the RX 6700 XT and nothing when cached; no model / no GPU / a corrupt model
falls back to facts only, never a crash. Tag `ai-4`.

## Phase AI5: share an Assistant example to improve it (owner, 2026-10-15; right after AI4; ≈ 6–10 h)

Reverses the owner's 2026-10-14 "no AI usage data" decision, **only as an explicit, per-answer,
opt-in action** (FORK-PLAN §1a.84). Nothing is ever sent in the background; FilmCraft has no server.

1. **On every Assistant answer** (panel and Ask box): 👍 / 👎, an optional "what should it have
   said?" box, and **Help improve the Assistant…**.
2. **Preview before anything leaves:** exactly what is shared, shown as text: the question, the
   answer, the guide sections it used, FilmCraft version, page / workspace. Ticked off by default
   and added only if the user ticks them: the current grade's settings (G2.10 facts), the AI4
   picture facts, and (separate, never pre-ticked) a small frame from their footage. Paths,
   project / clip / user names and LAN addresses are stripped (the publish script's rules).
3. **Consent text, shown before sending:** it will be used to train and improve FilmCraft's
   Assistant, including models released openly; it will be public on GitHub; don't include
   anything private. The user must tick that they agree.
4. **Route: GitHub.** "Open on GitHub" opens a pre-filled *New issue* page on the fork's repo (label
   `assistant-example`, title from the question, body = the previewed text) in the browser; the
   user submits with their own account. No keys or tokens in the app. A frame can't go in a URL:
   the issue body says to drag the saved image in, and the image is saved next to the text.
   Long examples beyond the URL limit are saved and the page says to paste the file.
5. **Save to file, for anyone:** "Save example…" writes the same previewed text (and the frame, if
   ticked) to a folder of the user's choosing, for those who'd rather check it or send it another
   way. Available with or without consent / GitHub.
6. **On the owner's side:** a script (or a later runner step) reads issues labelled
   `assistant-example` into the eval set / training pairs after a human review; never into a
   tune without review (same rule as the training pairs, Q35).
7. Commands `coach.feedback` (rating, correction) and `coach.shareExample` (`route`: github | file,
   with the ticked extras) for agents; never crash on a missing browser or an unwritable folder.
8. **Fork-only: Send to my training inbox (owner, live, 2026-10-15 ~23:45, FORK-PLAN §1a.92; ≈ +2–4 h).** A third
   button on the owner's build (`FORK-ONLY:` commits, listed in FORK-ONLY.md; `coach.shareExample` `route: inbox`).
   Nothing goes over the network. Writes **the whole conversation** (every turn, the guide sections each answer used,
   👍 / 👎 and the correction, selection / on-screen context, the AI4 picture facts, the grade's settings, and the
   frame when ticked) as one JSON file per conversation to `<data dir>/assistant-inbox/` (follows `--data-dir`;
   default `~/.local/share/filmcraft-fork/assistant-inbox/`). No stripping (it never leaves the machine; own frames
   are approved local-only training data). Capped sizes, atomic write, never crashes on an unwritable folder.
   **Runner side:** a step of the main runner (and `tools/fork/ai-inbox-import.py`) reads new inbox files each run
   and triages each conversation, whole, into held-out *or* training (never both; creative ones by family), owner
   corrections become `must` / the reference answer, gap answers go to the gaps list; `origin = "owner-inbox"`.
   The raw inbox stays out of git; only cleaned cases are committed. Owner read before any tune (Q35) as always.

**Accept:** preview equals what is sent / saved byte for byte (test); nothing leaves without the
consent tick; stripped fields never appear (test with a project full of private paths and names);
the GitHub URL opens the right repo with the label; Save works without consent; (fork) the inbox file
holds every turn and the ticked extras, and the import script turns a test file into a held-out or training case; guide page
(options: share on GitHub, save and send yourself, or neither, and why). Tag `ai-5`.

## Phase REF: "how do I get this look?" from a picture you like (owner, 2026-10-16; after S1, before C2; ≈ 15–30 h)

FORK-PLAN §1a.93. Builds on AI4 (picture facts, Qwen3-VL, question-aware looking) and the existing
Shot Match / Gallery (`docs/guide/color/matching.md`).

1. **Bring in a reference picture (≈ 3–5 h):** drop, paste or pick an image file (PNG / JPEG / TIFF
   via our own decoders; size and pixel caps, hostile-input checks) into the Ask box or the Gallery
   (`stills.import`, a still without a grade). It is a command with an automation id, like everything.
2. **Measured difference (≈ 4–6 h):** AI4's `PictureFacts` on the reference (display-referred) and on
   the current frame after the grade; a short "theirs vs yours" sheet (exposure, contrast, shadow /
   highlight colour, saturation, skin vs the skin line, black level / letterbox) in the context.
3. **Grade first pass (≈ 3–6 h):** Shot Match accepts a still as the reference (today only a clip),
   so the answer's **Do this** builds an ordinary node you can see, adjust or switch off.
4. **What the picture shows (≈ 3–8 h):** the vision model (question-aware, item 2b) describes what
   numbers can't: light direction / hardness, depth of field, haze / bloom / halation, grain, framing.
5. **The answer (≈ 2–5 h):** two parts: **in the grade** (a guide-grounded recipe: nodes, curves,
   saturation, grain, letterbox) and **on set** (lighting, lens, focus: said plainly, with what can be
   faked and how close it gets). It never claims to know exactly how the shot was made, and says when
   the footage is too different for a grade to get there (daylight → neon night).

**Privacy:** the reference picture stays on the machine; never training data, never in an AI5 shared
example (stripped like other private data). **Accept:** reference fixtures (generated pictures with
known looks: teal / orange, faded blacks, warm highlights, letterbox, heavy grain) → the measured
difference names each; Shot Match on a still moves the clip's measured look toward it (test); answers
stay grounded (0 invented controls) and include both parts; a corrupt / huge / odd image never crashes;
works with facts only when the vision model is missing. Guide page with options and trade-offs (match
a still vs a clip vs by eye, beginner vs pro). Tag `ref-1`.

## AI-POOL: a question pool ten times larger (owner, 2026-10-15; Dell lane, alongside AI4)

Grow the assistant's question pools about tenfold for future training and for evaluation that
catches more regressions (FORK-PLAN §1a.85). Writing work, no GPU: the Dell lane.

| Pool | Before | Target |
|---|---|---|
| Advanced eval (`docs/fork/coach-eval/advanced.toml`) | 87 | ~900 (≈ 2 per guide section) |
| Keyword-coach eval (`questions.toml`) | 126 | ~1 250 |
| Draft training pairs (`training-pairs.jsonl`) | 885 | ~9 000 |

- **Variety per section** (454 sections): beginner and pro wording, Resolve and Premiere words,
  short / sloppy phrasings, follow-ups ("and if it's still orange?"), "does FilmCraft have X?" for
  missing features (gap answers), picture-dependent questions marked `needs_clip` for AI4. The
  owner's real questions (`origin = "owner-verbatim"`) are the model for tone.
- **Grounded:** every question names its guide source section(s) and the `must` / `must_not` facts
  a right answer contains, from the guide's text; no invented features. Existing well-formedness /
  header tests apply; generator scripts live under `tools/fork/`.
- **Splits, never mixed:** a held-out **test set** the model never trains on (the eval files), a
  **training set** (the pairs), and a fixed **daily sample** of ~150 test questions for quick GPU
  runs (the full ~900 takes ~40 min on the RX 6700 XT). A check refuses overlap (same or
  near-same question in both).
- **More training pairs without rewording test questions (owner, 2026-10-15, FORK-PLAN §1a.87):** the
  ~9 000 target can't be met with single-section questions (most would paraphrase a held-out one).
  Add three kinds that don't: (1) **multi-section tasks** spanning two or three sections in the order
  a job needs them ("grade a log clip, then export it for YouTube"; answer = the steps from each
  section, in order, with their sources); (2) **follow-up chains** of 2–4 turns (the next question
  depends on the last answer: "and if it's still orange?", "now only the sky"), stored with the
  earlier turns as context like the Assistant sends them; (3) **picture-dependent pairs** using AI4's
  measured-fact sheet (the same wording and fields the engine writes for the model, e.g. a frame
  600 K warm with clipped highlights) + a question, answered with the guide steps that fit those
  facts (`needs_clip`). The overlap check still applies (a chain or task whose first question
  rewords a held-out one is left out). Mix kept balanced across areas; a held-out slice of each new
  kind joins the test set too, so AI4 is scored on them.
- **Review:** the owner reads a random 30–50 pairs before any tune uses them (Q35 rule); a fresh-
  context review samples each batch before merge.
- **Usage:** mostly Sonnet subagents writing in batches (one at a time on this lane), Opus spot
  checks; batches per guide area so each merge is small. Floors: the coach eval and retrieval
  floors stay green (new questions may lower the *rate*, never the counts that passed before).
- **Done:** the targets reached (or the guide's limit stated, if fewer sections support honest
  questions), splits checked, a ~150-question daily sample chosen, counts in HANDOFF; marker
  `.dell-pool-complete`.

## Phase AI7: an assistant that can walk you through a whole complex edit (owner, 2026-10-22, FORK-PLAN §1a.129; ≈ 90–160 h)

Owner (live, 2026-10-22 ~15:45): "What I really want though is the AI to be a lottt better. I asked some questions to
it a couple hours ago and it just wasn't very helpful at all. I don't necessarily want to implement AI6 right now (nor
do I think it would be good as a hands on assistant yet) but I want to have a phase that actually makes the AI good
enough to walk you through an entire complex editing process." AI7 makes the Assistant a good **teacher and guide**;
it still never runs commands (that is AI6's "Do this", which stays unscheduled). Runs after the pause that follows
AI2b, before DECODER-PLAN D (FORK-PLAN §1a.129).

**Why it is weak today (the owner's inbox conversation, 2026-10-22 13:24–13:27, `assistant-inbox/`, five turns,
SmolLM3-3B `smollm3-assist-v1`, page Compositing, Resolve workflow):**
- *"how do I use the compositor"*: ten lines stitched from four guide sections, one of them unrelated
  (`command-reference.export-lists`: export presets in a compositor answer) and one aimed at agents, not people
  (`comp.explain` over the control channel / MCP). No overview, no "what you'll see".
- *"only give me instructions for the resolve workflow"*: answered "how to switch to the Resolve workflow" instead
  of redoing the previous answer the Resolve way. **No follow-up memory**, and it didn't know the owner was already
  in the Resolve workflow.
- *"how do I use the compositor in the resolve workflow"*: three lines about viewing a node; nothing about the
  Fusion page.
- *"help me with step one more in depth"*: "I don't know". It can't see its own previous answer.
- *"how do I use fusion on a clip I already have"*: skin-grading and golden-hour notes. "Fusion" didn't map to the
  compositor, and `compositor.convert` (the right answer) was retrieved but not used.

So the problems are, in order: (1) no conversation state, (2) no app state (workflow, page, selection), (3)
retrieval that lets unrelated sections in and misses Resolve / Premiere vocabulary, (4) an answer built by pasting
guide lines rather than explaining, (5) a small model with no training on multi-step or multi-turn help, (6) no notion
of a long task with stages. AI7 fixes them in that order, measuring after each.

1. **The five turns above become eval cases first** (multi-turn, with expected sources and must / must-not facts),
   plus every later owner-inbox conversation (§1a.92). They fail today; they must pass at the end. ≈ 2–3 h.
2. **Conversation memory.** Each question is rewritten into a standalone one using the earlier turns before
   retrieval ("step one more in depth" → "explain in depth: open the Fusion page …"; "only the Resolve way" →
   re-answer the last question in the Resolve workflow). The previous answer's steps are addressable ("step 3",
   "the second one", "that node"). Long conversations are summarised to fit the context. ≈ 8–14 h.
3. **Knows where you are (read-only facts).** The current workflow (Premiere / Resolve), page, open panels,
   selection (clip, comp node, grade node), project facts (clips imported, sequence exists, frame size, rate,
   log profile from the Fuji maker note) go in as compact facts. Answers use **only that workflow's names**
   (Fusion page, not "Window ▸ Compositor", in Resolve). Agent-only routes (control channel, MCP, command ids as
   instructions) never appear unless the question asks for them; ids stay as clickable outlines. ≈ 8–14 h.
4. **Retrieval that finds the right thing.** A synonym / vocabulary map from Resolve and Premiere terms to
   FilmCraft sections (Fusion → compositor, Fairlight → audio, Lumetri → colour, Power Window → windows, nest,
   compound clip …), generated from the guide headers' `resolve:` / `premiere:` fields and checked by a test; a
   reranker or relevance floor so off-topic sections (export lists, skin practice notes) are dropped; for a task
   question, sections from every stage it touches. Recall@5 on the eval set must rise; off-topic citations ≈ 0.
   ≈ 8–14 h.
5. **Answers that explain, not pasted lines.** The model writes in its own words from the retrieved sections, in a
   fixed shape: one-line goal → what it does / why → numbered steps (each with what you should see) → common
   mistakes → what to do next. The grounding checker stays (every control and command checked against the live app;
   unknown ones removed; general advice marked as general). "I don't know" stays better than a guess, but it must
   say what it *does* know and ask a clarifying question when the question is ambiguous. ≈ 10–16 h.
6. **Walkthrough mode for long tasks.** "Walk me through …" (e.g. a YouTube video from a card of F-Log2 clips to
   upload; a music video with a node grade, titles and a Fusion comp; a two-camera interview with voice clean-up)
   gives a **plan of stages** first (ingest → organise → rough cut → fine cut → audio → colour → titles /
   compositing → export), each with options and trade-offs (beginner vs pro, never one right way, §1a.47), and
   then guides one stage at a time. It **watches the app state** (read-only) to see when a step is done ("you've
   added the comp clip; next, connect …") and offers the next step; you can skip, go back, ask a side question and
   return, or pause and resume later (the plan is saved in the project). Nothing is executed for you. ≈ 20–35 h.
7. **Whole-workflow knowledge.** The guide gains end-to-end workflow pages (the stages above, joined up; how the
   pages / panels hand off; where each workflow differs) and ≥ 10 full worked scenarios written as options, each
   tested by walking it in the real app via the control channel. Unique fork features (proxies, render cache,
   Magic Mask, node explainer, two-GPU export, Send to Resolve once it exists) placed in the right stage. ≈ 15–25 h.
8. **A stronger model for capable GPUs, the small one kept.** Per §1a.124 ("compete with Resolve Studio": pick
   the strongest tier capable GPUs can run, keep Faster + CPU fallback), evaluate a 7–8B open-licence chat model on
   the existing GPU inference path (`crates/llm` `gpu_forward`; the RX 6700 XT's 12 GB fits ~8B at Q4), e.g.
   Qwen3-8B (Apache-2.0), OLMo 2 7B (Apache-2.0), against a 4B tier and today's SmolLM3-3B. Settings: Better (GPU,
   big) / Faster (small) / Auto. Pick by the eval, not by size. **Each model download needs the owner's yes** (name,
   source, size). **Qwen3-8B and Qwen3-4B approved (owner 2026-10-22, FORK-PLAN §1a.130);** others still need a yes. ≈ 10–18 h.
9. **Train for it.** Multi-turn and walkthrough training data from items 1, 6 and 7 (follow-ups, "more in depth",
   "the Resolve way", side questions, a stage done / not done), generated from the guide and reviewed; tune the chosen
   model(s) on the RX 6700 XT (TRAINING.md). Held-out set: the walkthrough scenarios and the owner's turns never go in
   training. ≈ 10–20 h (mostly GPU time).
10. **Eval and acceptance.** A multi-turn set (≥ 80 conversations, ≥ 300 turns) and ≥ 10 full walkthroughs scored
   per turn (right sources, right workflow's names, no agent-only routes, no off-topic lines, follows up on the
   previous turn) and per walkthrough (completes the task in the real app when followed literally, driven via the
   control channel). **Accept:** the owner's five turns all pass; ≥ 90 % of multi-turn turns correct; ≥ 8 of 10
   walkthroughs completed end to end by following the Assistant alone; the coach / `ai_pool` floors don't drop;
   answers stream first words in ≤ 2 s on the Better tier on this PC; without a model the keyword coach still works.
   Guide pages, es + zh-cn strings, tag `ai-7`, local build. The owner trying a walkthrough is welcome but never
   blocks acceptance (OWNER-QUESTIONS: never block on the owner).

## Phase AI6: the hands-on assistant, and Claude as co-editor over MCP (owner, 2026-10-15; before Phase 7; ≈ 135–255 h)

Goal: "guide me through a full YouTube video" (grading, Fusion, voice-over, cuts) becomes realistic, and Phase
7's real-edit check (Y4) is done with the Assistant's help (FORK-PLAN §1a.88). In this order:

1. **Do it for me (≈ 25–45 h).** Answers carry **Do this** actions: the model proposes commands and
   values only; FilmCraft checks them against the real command list and parameters (nothing invented
   runs), shows exactly what will change, runs them as one undo step on the user's click, and
   outlines what changed. Destructive or file-writing commands always ask separately.
2. **Knows your edit (≈ 20–35 h).** A compact timeline summary (tracks, clips, lengths, markers, gaps) and
   the transcript (Text panel ▸ Transcript) sent as facts; suggestions the user accepts / rejects: long
   pauses, filler words, repeated takes, a slow intro, jump-cut candidates (each as a Do-this action).
3. **Audio facts (≈ 10–20 h):** loudness (LUFS / true peak), noise floor, clipping, voice vs music level per
   section, like the picture facts.
4. **Longer memory (≈ 10–15 h):** a whole-session chat with older turns condensed automatically (prompt
   size stays as today; first words ~1–2 s even in long sessions), and a per-project plan notepad
   saved with the project.
5. **Creative suggestions across the video (≈ 60–120 h):** per-shot descriptions (AI4's vision model over
   every shot, cached) + the facts above → whole-video notes ("the intro drags", "B-roll at 2:10 doesn't
   match scene 3's grade", "music too loud under the voice"). Mechanical notes are the reliable part;
   taste-level advice is a second opinion and is labelled so. (A 7–8B tier from the maybe list may help.)
6. **Claude (or any MCP client) as co-editor (≈ 10–20 h).** FilmCraft's MCP server (`filmcraft-cli mcp`,
   bridge to the live app) already exposes every command, project / sequence inspection, UI control,
   screenshots and rendered frames. Add MCP tools / resources for everything above: picture and audio
   facts, timeline + transcript summary, guide search (`coach.ask` / sections), the project plan notepad,
   and **proposals** that appear in the app as the same Do-this preview (an MCP client's edits can be
   set to "ask me first" in Settings, default on for edits, off for read-only calls). A guide page:
   using Claude Desktop / Claude Code as a co-editor, what is sent (frames, transcript, project
   details go to the client's provider), the local Assistant as the private / offline option.

**Accept:** a scripted edit of a real-length YouTube-style test project (talking head + B-roll + VO +
music + one comp) where the Assistant's Do-this actions do the grade, the voice clean-up, the loudness
pass, and accepted cut suggestions, every action undoable and previewed, 0 invented commands; the same
project driven over MCP by an external client; no crash on hostile model output (bad commands /
params refused). Tag `ai-6`.
