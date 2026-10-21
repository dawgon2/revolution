# Training the node explainer (GRADE-PLAN G2.11b)

A repeatable session that fine-tunes a small instruct model so it explains a colour-grade node in
the controlled grammar the fact checker accepts (`crates/render/src/grade_explain/`), then
quantizes it to GGUF for `filmcraft-llm` (candle, CPU) and scores it on the 36 held-out cases.
Everything here is **offline tooling**: the product never contains Python, PyTorch or training
code, and tuned weights are never committed or bundled (owner, OWNER-QUESTIONS Q6–Q8).

## Prerequisites

- This repository, Rust toolchain (`~/.cargo`).
- The full-precision bases (safetensors + config + tokenizer, pinned in `REVISION`):
  `~/.local/share/filmcraft-fork/models/llm-base/{SmolLM2-360M-Instruct,Qwen2.5-0.5B-Instruct}/`.
- The trainer venv `~/Apps/filmcraft-train/.venv` (Python 3.12, `torch` 2.5.1+rocm6.2,
  `safetensors`, `tokenizers`). `train.py` uses **only** those three: the venv's
  `transformers` 5.19 / `peft` need torch ≥ 2.6 and fail to import with torch 2.5.1, so the
  trainer carries its own small Llama / Qwen2 implementation (`tinylm.py`, checked against the
  bases: same greedy answers as candle on F16 GGUFs, see "Converter validation").
- GPU: the RX 6700 XT (gfx1031) needs `HSA_OVERRIDE_GFX_VERSION=10.3.0` (run.sh sets it).
- Disk: ≈ 2 GB per run (merged safetensors + GGUFs).

## One command

```sh
tools/explainer-train/run.sh smol360 smol360-v2     # SmolLM2-360M → Q8_0
tools/explainer-train/run.sh qwen05  qwen05-v2      # Qwen2.5-0.5B → Q8_0 and Q4_K_M-like
```

`STAGES="train quantize"` runs only some stages; `TRAIN_ARGS="--epochs 3 --lr 1e-4"` passes
options to `train.py`; `DATA=…` picks another data folder; `DEVICE=cpu` is the CPU fallback (see
below). run.sh writes `session.log` with each stage's duration.

## The stages (one command each)

| stage | command | output | time here |
|---|---|---|---|
| generate | `cargo run --release -p filmcraft-render --example explain_traindata -- --out-dir ~/Apps/filmcraft-train/runs/data-v1 --grades 2600 --per-grade 2 --seed 1` | `train.jsonl`, `val.jsonl`, `data-stats.json` | 9 s (+ build) |
| train | `HSA_OVERRIDE_GFX_VERSION=10.3.0 ~/Apps/filmcraft-train/.venv/bin/python tools/explainer-train/train.py --base <llm-base/…> --data <data> --run-dir ~/Apps/filmcraft-train/runs/<name> --out ~/.local/share/filmcraft-fork/models/tuned/<name>` | merged `model.safetensors`, config, tokenizer, `LICENSE`, `NOTICE`, `manifest.json` | 53 min (SmolLM2-360M), 49 min (Qwen2.5-0.5B), 2 epochs |
| quantize | `cargo run --release -p filmcraft-llm --features infer --example llm_quantize -- <tuned dir> <tuned dir>/<name>-q8_0.gguf --type q8_0` (`--type q4k` for the Q4_K_M-like mix) | GGUF + its size and SHA-256 | 2–4 s |
| eval | `cargo run --release -p filmcraft-render --example explain_score -- --dump-cases cases.jsonl` then `FILMCRAFT_LLM_DIR=~/.local/share/filmcraft-fork/models cargo run --release -p filmcraft-llm --features infer --example llm_eval -- cases.jsonl --local <name>=<gguf>:llama --out eval.jsonl` | answers with timings | ≈ 2–4 min per model |
| score | `cargo run --release -p filmcraft-render --example explain_score -- eval.jsonl --summary score.md --json score.json --fails` | pass rate, rejection histogram, mean time | 1 s |

Where things go: data, logs, eval outputs and scores in `~/Apps/filmcraft-train/runs/<name>/`
(data in `runs/data-v1/`); weights in `~/.local/share/filmcraft-fork/models/tuned/<name>/`.

### What each stage does

- **generate** (`explain_traindata`): random one-node grades over every control family of the
  evaluation set (wheels, contrast, saturation, temperature / tint, curves, qualifier incl.
  inverted, windows incl. inverted, LUT, grain, noise reduction, highlight roll-off, disabled,
  matte view, identity) on synthetic frames (the evaluation frame or random patch colours);
  facts from `explain_node`; the prompt from `filmcraft_llm::explain_prompt` (exactly what the
  app sends); targets from a rule-based realizer that words the verified facts several ways
  (by dimension with verbs, by subject, template-like; leads such as "inside the window", "in the
  selection", "It affects 13 % of the frame; inside it,"; optional omission of minor changes).
  **Every target passes `check_explanation_labeled`**; the shorter 60 % of the passing candidates
  are kept, two per grade. Grades whose compact facts equal one of the 36 evaluation cases are
  dropped; train / val are split by grade. Deterministic per seed.
- **train** (`train.py`): LoRA r 16, α 32, dropout 0.05 on all attention and MLP projections of
  the frozen bf16 base; ChatML as `filmcraft-llm`'s `chat_prompt`, prompt and answer tokenized
  separately as at inference, loss on the answer + `<|im_end|>` only; AdamW lr 2e-4, 30 warm-up
  steps, cosine to 10 %, batch 16, 2 epochs, gradient checkpointing (≈ 2 GB of VRAM), validation
  loss every 50 steps, early stopping (patience 3), the best adapter merged into the base.
- **quantize** (`llm_quantize`, pure Rust / candle): Hugging Face names → GGUF names, the `<arch>.*`
  metadata candle reads, the q/k row permutation candle's Llama needs (interleaved rope), Q8_0 or
  the Q4_K_M-like mix (k-quant fallback to Q5_0 / Q8_0 for 896- and 960-wide rows, as llama.cpp
  does). Prints size and SHA-256 for the catalogue.
- **eval / score**: the 36 held-out cases, greedy, 80 tokens, repeat penalty 1.0 (run.sh's
  `REPEAT_PENALTY` default; the tuned models' `ModelInfo::recommended_options`, which the app uses
  too); each answer checked by `check_explanation_labeled(facts, None, Some(1), text)`.
- **all sets (AI2)**: `tools/explainer-train/score-sets.sh <name>` evaluates and scores the
  quantized model on sets 1–3 (node lines), 4 (comp lines, `comp_traindata --dump-cases`) and 5
  (branch notes) into `$RUNS/<name>/sets/`; `SETS="4 5"` picks a subset.

## First test training (2026-10-11)

Data `data-v1` (seed 1): 2600 random grades → 4755 train / 236 val examples (2 targets per
grade, mean 310 tokens per example); 55 grades dropped for matching held-out facts, 17 duplicate
facts, 32 without a passing target; 92 % of the realizer's 98 455 candidates passed the checker.

| run | base | LoRA params | steps | train time | val loss before → best |
|---|---|---|---|---|---|
| `smol360-v1` | SmolLM2-360M-Instruct @ a10cc15 | 8.7 M | 596 (2 epochs) | 53 min | 2.076 → 0.510 |
| `qwen05-v1` | Qwen2.5-0.5B-Instruct @ 7ae5576 | 8.8 M | 596 (2 epochs) | 49 min | 2.025 → 0.478 |

Neither run early-stopped; val loss was still creeping down at the end (more epochs or data may
help a little). Evaluation on the 36 held-out cases, greedy, 80 tokens, repeat penalty 1.0, all in
one `llm_eval` run on this PC (x86-64-v3 build, idle machine):

| model | pass | mean s / answer | rejections |
|---|---|---|---|
| best stock: `smollm2-1.7b-instruct-q4km` | 5/36 (14 %) | 8.28 | UnknownWord 26, Ungrammatical 4, TooManySentences 1 |
| tuned `smol360-v1` Q8_0 | 34/36 (94 %) | 2.95 | Direction 2 |
| **tuned `qwen05-v1` Q8_0 (pick)** | **35/36 (97 %)** | **2.44** | Direction 1 |
| tuned `qwen05-v1` Q4_K_M-like | 35/36 (97 %) | 6.26 | Direction 1 |

With the stock default repeat penalty 1.1 the stock models score 0–8 % and the tuned SmolLM2
58 %: the controlled grammar repeats small words, so tuned models run without the penalty
(`ModelInfo::recommended_options`). **Owner decision 2026-10-12 (FORK-PLAN §1a.27): SmolLM2 is the base going forward (open training data); `qwen05-v1` is frozen. Original pick by score: `filmcraft-explainer-qwen05-v1-q8`** (best pass rate,
and faster than the 360M model: 24 layers and a shorter prompt in tokens). Q8_0 beats Q4_K on
speed here because candle's Q4_K / Q5_0 prefill is ≈ 3× slower than Q8_0 on this CPU; the
Q4_K file is only 40 MB smaller. The one shared failure (`warm_gain`) is a tint claim
("more magenta") for the midtones, whose tint is just below the checker's threshold; the checker
rejected it, so the user would see the template sentence.

Outputs: `~/Apps/filmcraft-train/runs/compare-v1/{eval.jsonl,score.md}`; stock runs in
`runs/stock/`.

### Held-out integrity

The generator drops a training grade only when its **compact facts are exactly equal** to one of
the 36 evaluation cases. That is an exact-match dedupe, not a similarity filter: 12 of the 36
evaluation reference sentences also appear as training targets (the trivial cases: "no visible
change", "disabled", the matte view, single-control whole-frame shifts whose wording has only one
form). On the 24 cases with no overlapping reference the tuned Qwen2.5-0.5B scored **23/24**, so
the result is not memorisation of the easy cases, but 36 cases on one frame is a small test.
**Follow-up (done 2026-10-12):** the second held-out set below confirms it: `qwen05-v1` Q8_0
passes 39/39 on another frame and seed (74/75 over both sets), `smol360-v1` 38/39 (72/75).

### Second held-out set (2026-10-12)

**How it is built** (`eval_cases_v2` in `crates/render/src/grade_explain/cases.rs`, code, not a
file): 40 one-node grades covering the families of the first set (wheels, contrast incl. pivot,
saturation, temperature / tint, master and red-channel curves, qualifier incl. inverted, windows
incl. inverted, a real look LUT, grain, noise reduction, roll-off, disabled, matte view, identity,
tiny no-op) on a **different frame**: 96 × 64 (every training frame is 120 × 60 with a horizontal
ramp on top), a vertical grey ramp of another curve on the left 40 columns and a 2 × 2 grid of
patches on the right whose colours are not in the generator's palette; control values drawn from
**seed 2** (SplitMix64, as the generator). Labels `h2_…`. Dump, deduped against the training data:

```sh
cargo run --release -p filmcraft-render --example explain_score -- --set 2 --dump-cases cases-v2.jsonl \
    --dedupe ~/Apps/filmcraft-train/runs/data-v1/train.jsonl --dedupe ~/Apps/filmcraft-train/runs/data-v1/val.jsonl
```

**Dedupe** (against all 4991 data-v1 examples, 2496 distinct facts; compact facts as the prompt
carries them): **1 dropped** for exactly equal facts (`h2_disabled_contrast`: a disabled node's
facts don't depend on the frame) → **39 cases**. Near-duplicates (kept; the reference sentence
without its "Node k" label equals a training example's, i.e. equal at the precision the sentence
and the checker use): **7/39** (identity, +½ stop, green tint, matte view, grain, roll-off, tiny
gain), against **12/36** for set 1 by the same measure; 2/39 references are also training targets
(set 1: 5/36). Set 1 re-checked: 0 exact matches.

**Results**, both sets in one `llm_eval` run (`runs/heldout2/`: `eval.jsonl`, `eval.log`,
`score-set{1,2}.md`), greedy, 80 tokens, repeat penalty 1.0 for all four, deadline 120 s. The machine
was **busy** (another session's ffmpeg / x265 fixture encode, load average 17 → 9.5 over the run),
so the stock model's times are inflated (it ran first, at the peak); the Q8_0 tunes' times are
within 5 % of the idle first run (Q4_K ran faster than its first 6.26 s; timing is noisy):

| model | set 1 (36) | set 2 (39) | both (75) | mean s / answer (set 1 / set 2) |
|---|---|---|---|---|
| best stock: `smollm2-1.7b-instruct-q4km` | 5 (14 %) | 3 (8 %) | 8 (11 %) | 13.65 / 7.71 (loaded) |
| tuned `smol360-v1` Q8_0 | 34 (94 %) | 38 (97 %) | 72 (96 %) | 2.98 / 3.16 |
| **tuned `qwen05-v1` Q8_0** | **35 (97 %)** | **39 (100 %)** | **74 (99 %)** | **2.47 / 2.59** |
| tuned `qwen05-v1` Q4_K_M-like | 35 (97 %) | 37 (95 %) | 72 (96 %) | 4.30 / 4.49 |

Set 1 answers are identical to the first run (greedy), so its numbers repeat.

**Every failure inspected:**
- Tunes, all **model errors** (correct rejections): set 1 `warm_gain` (all three: "more magenta"
  midtones, below the tint threshold), `offset_blue` (smol: "less saturated" midtones, no clear
  change); set 2 `h2_qualifier_skin_inverted` (smol: midtones "a sixth of a stop darker", measured
  −0.07 stop = 0 sixths; Q4_K: omits the main change, shadows −30 % saturation), `h2_lut_look`
  (Q4_K: "greener" highlights, tint −0.003, below the threshold). The tunes' misses are all small
  colour-cast / saturation claims just below the checker's thresholds or an omitted change.
- Stock: 35 of 36 set-2 rejections are model errors (words outside the controlled vocabulary such
  as "with", "making", "respectively"; non-grammar sentences; too long; 3 sentences).
- **One checker false-reject (a real checker bug, reported, not fixed):** `h2_noise_reduction_only`.
  On this frame noise reduction changes 1 % of the pixels (shadows +⅓ stop at the ramp / patch
  edge) and the template is "affects 1 % of the frame; inside it, shadows +⅓ stop; also reduces
  noise; midtones and highlights untouched." `check_explanation_labeled` **rejects its own
  template** (`Range`: "the text speaks about the whole picture, but the midtones are untouched":
  the clause "also reduces noise" is read as a whole-picture claim). The stock model copied the
  template verbatim and was rejected for it; both tunes passed by writing "reduces / removes noise
  in the shadows". `second_set_is_fixed_distinct_and_its_templates_pass` pins this as the one known
  template rejection, so a checker fix shows up there.

**Conclusion:** the pass rate holds on unseen data: `qwen05-v1` Q8_0 is the best on both sets
(74/75, no misses on set 2) and faster than `smol360-v1` (72/75); the gap is small (2 cases over 75)
but consistent in direction on both sets, and the Qwen tune is also ~20 % faster per answer.
Q4_K is not better than Q8_0 on either set.

## smol360-v2 (AI1.6, 2026-10-13)

**Why:** v1's misses on both held-out sets were small colour-cast / saturation claims just below
the checker's thresholds (see above), and G2c (white-balance picker, Auto balance) shipped since.
**Data** (`runs/data-v2`): `explain_traindata --grades 3400 --per-grade 2 --seed 2 --subtle 0.3`:
30 % of the corrections have their colour / tone amounts shrunk to 10–40 %, so many targets leave
a sub-threshold change out; facts of all three held-out sets are dropped (sets 2 and 3 were not
excluded before; data-v1 regenerates unchanged with the defaults). 6066 train / 352 val examples.
**Train:** `DATA=…/data-v2 TRAIN_ARGS="--epochs 3" tools/explainer-train/run.sh smol360 smol360-v2`.
**Third held-out set** (`eval_cases_v3`, `--set 3`, 13 `h3_` cases on the second set's frame):
Temperature / Tint pairs as the picker and Auto balance write them (alone, with exposure, in a
qualifier, in a window) and corrections with a too-small colour / saturation change to leave out.
**Accept** (AI-PLAN): v2 ≥ v1 on sets 1 and 2 and passes set 3, else v1 stays the default.
**Results** (2026-10-14, `runs/v2-compare/`, one `llm_eval` run, our own runtime, greedy, 80
tokens, repeat penalty 1.0, cases deduped against data-v2; synthetic prompts, idle machine):

| model | set 1 (36) | set 2 (40) | set 3 (13) | mean s / answer |
|---|---|---|---|---|
| `smol360-v1` Q8_0 | **34** | **37** | 13 | 1.26 / 1.34 / 1.14 |
| `smol360-v2` Q8_0 | 33 | 36 | 13 | 1.27 / 1.34 / 1.10 |

Training: 3 epochs ran to the end (val loss 0.461 at step 1000/1140). **v2 does not beat v1**,
so it is not catalogued and v1 stays (AI-PLAN Accept). The kind of miss moved: v2 makes fewer
"Direction" errors (sub-threshold casts) but new "Omission" ones (it leaves out a main change, e.g.
`contrast_sat_warm`'s warmer shadows), so the subtle data taught omission a little too broadly.
Set 3 doesn't separate them (both 13/13). Ideas for a v3: a smaller `--subtle` (0.1–0.15),
subtle only on a secondary correction of a multi-correction grade, or more set-3-like near-threshold
cases. Weights stay in `models/tuned/smol360-v2/` (unused).

## smol135-v1, the "fast" tune (AI1.4, 2026-10-14)

**Why:** a faster option for slower PCs (owner Q24 (a)). **Base:** SmolLM2-135M-Instruct, revision
`12fd25f77366fa6b3b4b768ec3050bf629380bac` (Apache-2.0, `llm-base/SmolLM2-135M-Instruct/`, ~270 MB),
`run.sh` base `smol135`. **Run:** `tools/explainer-train/run.sh smol135 smol135-v1` (data-v1, v1's
recipe, 2 epochs; train 1411 s). Q8_0 143 MB. **Results** (our runtime, greedy, 80 tokens, repeat
penalty 1.0; sets 2 and 3 not deduped against data-v1; synthetic prompts):

| model | set 1 (36) | set 2 (40) | set 3 (13) | mean s / answer |
|---|---|---|---|---|
| `smol360-v1` Q8_0 | 34 | 37 | 13 | 1.26 / 1.34 / 1.14 |
| `smol135-v1` Q8_0 | 34 | 36 | 13 | 0.62 / 0.65 / 0.56 |

Within a few cases of the 360M tune at half the time, so it is catalogued as
`filmcraft-explainer-smol135-v1-q8` ("fast"), not in `TUNED_PREFERENCE` (picked only by the user).
(zsh note: write `"id=${G}:llama"` for `llm_eval --local`; `$G:l…` is a zsh modifier.)

## smol360-v3 (fork AI2, 2026-10-21)

**Data:** `data-v3` (`explain_traindata --grades 3400 --per-grade 2 --seed 3 --subtle 0.12 --branch-grades 1200`
plus `comp_traindata --comps 800 --seed 3`; 11 383 train / 686 val). **Train:** `TRAIN_ARGS="--epochs 2"`
on the RX 6700 XT, early stop by patience after about 94 min. **Scores** (`score-sets.sh`, Q8_0):
set 1 36 / 36, set 2 38 / 40, set 3 12 / 13, set 4 (comp) 32 / 32, set 5 (branch) 20 / 20; mean 1.27 s per
answer on set 1. For comparison v1: 34 / 37 / 13 / 6 / 2, v2: 33 / 36 / 13 on sets 1–3. Shipped first in
`TUNED_PREFERENCE` (provisional, OWNER-QUESTIONS Q61).

## Converter validation

`llm_quantize` was checked on the stock bases before any tuned model:
- SmolLM2-360M-Instruct → Q8_0: **all 290 tensors byte-identical** to the published
  `smollm2-360m-instruct-q8_0.gguf` (`llm_quantize --compare`), so identical outputs.
- Both bases → F16 GGUF: candle's greedy answers equal a PyTorch float32 reference
  (`tinylm.py`) on 8/8 eval prompts for each family (names, rope permutation, biases, tied head).
- Qwen2.5-0.5B → Q4_K_M-like: the same type mix as the published file (133 Q5_0, 12 Q4_K, 12
  Q6_K, 13 Q8_0, 121 F32), but its data cannot match: the published GGUF was made from different
  weights (even its F32 norm tensors differ from the safetensors at revision 7ae5576). Answers are
  coherent and as close to the reference as the published file's (common-prefix share 0.20 vs 0.21).

## Using a new run in the app

1. Put the files where the catalogue expects them (hard links save space):
   `mkdir -p ~/.local/share/filmcraft-fork/models/filmcraft-explainer-<name>-q8` then
   `ln ~/.local/share/filmcraft-fork/models/tuned/<name>/{<name>-q8_0.gguf,tokenizer.json} ~/.local/share/filmcraft-fork/models/filmcraft-explainer-<name>-q8/`.
   Until the user picks a model in Settings ▸ Color ▸ AI explanations, the app uses the best
   installed tune (`TUNED_PREFERENCE` in `models.rs`: the Qwen2.5-0.5B tune, then the SmolLM2
   one), else the stock default.
2. Add or update its entry in `crates/llm/src/models.rs` (`local!(file, sha256, size)` with the
   values `llm_quantize` printed; ids start with `filmcraft-explainer-`; description names the
   base, its revision and the pass rate).
3. `FILMCRAFT_LLM_DIR=~/.local/share/filmcraft-fork/models cargo test --release -p filmcraft-llm --features infer`
   verifies the checksum and runs it.

## CPU fallback

`DEVICE=cpu tools/explainer-train/run.sh smol360 <name>` (or `train.py --device cpu`) runs the
same trainer on the CPU in float32 (same code path; not timed in the test run, expect many hours
for 2 epochs of SmolLM2-360M); use `--epochs 0.5` or fewer `GRADES` to try it. A
pure-Rust candle LoRA trainer (OWNER-QUESTIONS Q7's first option) is a **follow-up**, not built:
the PyTorch path was cheaper and the GPU makes it fast.

## Adding training data

- More of the same: raise `GRADES` (or `--per-grade`) or change `SEED`, write to a new folder
  (`DATA=~/Apps/filmcraft-train/runs/data-v2`).
- New phrasings: extend the realizer in `crates/render/examples/explain_traindata.rs` (`realize`,
  `verb_phrase`, `predicate`, the no-op phrase lists). Anything the checker rejects is dropped and
  counted in `data-stats.json` (`candidate_rejections`), so wording experiments are safe.
- New control families: add them to `correction` / `random_grade` there.
- When the checker's grammar grows (`grammar.ebnf`), regenerate the data; the targets must
  always pass the checker of the build that will run the model.
- Hand-written examples: append lines in the same chat format
  (`{"messages":[{"role":"system",…},{"role":"user",…},{"role":"assistant",…}]}`, the system
  and user text from `explain_prompt`) to `train.jsonl`; check them first.
- Never add the 36 evaluation cases (or their facts) to the training data.

## Publishing (only when the owner says "publish")

Host: a GitHub **release** on the public repo `dawgon2/filmcraft-cachyos` (never a git push
there). Tag e.g. `explainer-smol360-v1`. Assets:

1. the GGUF (`<name>-q8_0.gguf`), 2. `tokenizer.json` (the base's, unchanged), 3. `LICENSE`
(Apache-2.0: `train.py` copies the base's licence file, or writes the Apache-2.0 text from
`tools/explainer-train/LICENSE-Apache-2.0.txt` when the base folder has none), 4. `NOTICE`
(written by `train.py` from the manifest; check it), 5. `manifest.json` (base repo name and data
folder name, no local paths). All five are in the tuned folder
`~/.local/share/filmcraft-fork/models/tuned/<name>/`. `manifest.private.json` (the same with the
local paths; `train.py` writes it to the run dir only, and `qwen05-v1` / `smol360-v1` keep one in
the tuned folder too) is **never** published.

NOTICE text (as `train.py` writes it):

```text
FilmCraft node explainer <name>
A fine-tune of <base repo> (revision <commit from manifest.json base_revision>), licensed under the
Apache License 2.0 (see LICENSE). Changes: LoRA fine-tuning (merged) on synthetic training data
generated by FilmCraft's explain_traindata from measured grade facts (no third-party text);
quantized to GGUF with FilmCraft's llm_quantize (the type is in the file name). Training data
provenance: generated by FilmCraft (fork of storytold/filmcraft), seed <seed>; every target passes
FilmCraft's fact checker. Provided "AS IS", without warranties of any kind.
```

Command (owner's machine, after "publish"):

```sh
# <name> already carries the version (e.g. qwen05-v1 → tag explainer-qwen05-v1)
gh release create explainer-<name> --repo dawgon2/filmcraft-cachyos \
  --title "FilmCraft explainer <name>" --notes-file NOTICE \
  <gguf> tokenizer.json LICENSE NOTICE manifest.json
```

Then point the catalogue entry's GGUF `url` at the release asset
(`https://github.com/dawgon2/filmcraft-cachyos/releases/download/<tag>/<file>`, `FORK_RELEASES` in
`models.rs`; the catalogue test accepts these besides pinned Hugging Face URLs); its `sha256` and
`size` already pin the file, and `ModelInfo::is_local()` turns false so Settings can download it.
The tokenizer keeps its pinned Hugging Face URL (it is the base's, unchanged).
