# filmcraft-llm

An optional, offline, quantized small language model for FilmCraft (L2). Its first job is
GRADE-PLAN.md item 11: rewriting the grader's structured facts as one or two plain-language
sentences for a beginner ("this node warms up the skin a little and calms down the reds"). The
caller checks every answer against the facts and falls back to the template sentence on any
mismatch, error, timeout or missing model.

- `models`: the catalogue of candidate instruct models, each file pinned to a Hugging Face
  commit with its SHA-256 and size, plus `installed` / `missing_bytes` / `model_dir`.
- `gguf`: a bounds-checked walk over the GGUF header (lengths, tensor extents, alignment,
  hyper-parameters) that runs before candle reads the file, so a damaged or hostile file is an
  error rather than a huge allocation or a panic.
- `Model` (feature `infer`): `Model::load(dir, &info)` and
  `Model::generate(system, user, GenOptions { max_tokens, deadline, temperature, .. })`
  (`generate_detailed` also reports token counts, timing and why it stopped). Uses ChatML, the
  published chat template of both families, and stops at `<|im_end|>` / `<|endoftext|>`,
  `max_tokens` or the deadline. Prompt plus answer are capped at `MAX_CONTEXT` = 2048 tokens.
  Loading and generating run under `catch_unwind`, so a panic becomes `LlmError::Panicked`. Without
  the feature, `Model::load` returns `LlmError::Unavailable`, so callers need no `cfg`.
- `Model::verify(dir, &info)` (any build): re-hashes the files against the catalogue's SHA-256;
  run it once after a download or on first use and cache the result.
- Prefill: candle 0.9's quantized models only accept a multi-token step at position 0, so the
  first 512 prompt tokens go in one batch and any rest token by token (deadline checked between
  steps; slow, so keep prompts short).
- `explain_prompt(facts_json, template_sentence)`: the fixed, grounded prompt (use only the facts,
  no numbers that are not in them, no control names, one or two sentences). The facts go through
  `compact_facts` first (drops nulls, default flags, untouched ranges and zero numbers, rounds to 2
  decimals, keys named list entries): on the 36 evaluation cases the prompt shrank from 489 to 237
  tokens on average (Qwen2.5; SmolLM2 516 → 259), so it always fits the batched prefill.

## Speed (measured on the owner's i5-11600K, 12 threads)

candle 0.9's k-quant dot products are selected at **compile time** (`#[cfg(target_feature =
"avx2")]`), so a default `x86-64` build runs them scalar: Qwen2.5-0.5B prefill ~16 tok/s, decode
~12 tok/s, 15–19 s per explanation. With `RUSTFLAGS="-C target-cpu=x86-64-v3"` (or `native`) the
same answer takes ~4–5 s (prefill ~65–75 tok/s, decode ~34). Cargo profiles cannot set target
features per crate, and `#[target_feature]` in this crate cannot reach candle's `cfg`, so the fork first used
a build flag; it now runs its own forward pass with run-time AVX2 dispatch instead (AI1.1, flag
dropped in AI1.2). Prefill is slow even with AVX2 because candle multiplies a multi-token batch one row
at a time against the quantized weights; dequantized f32 weights (`CANDLE_DEQUANTIZE_ALL=1`) prefill
at ~350 tok/s through `gemm` (runtime CPU dispatch) but decode 3× slower and use 4× the memory.
`Generation::prefill` and the `llm_eval` example report both phases.

## Features

| feature    | what                                                                        |
|------------|-----------------------------------------------------------------------------|
| `infer`    | CPU inference on candle (`candle-core`, `candle-transformers` quantized `qwen2`/`llama`) and the `tokenizers` crate with `fancy-regex` (no Oniguruma, no esaxx C++). |
| `download` | Downloads through `filmcraft-speech`'s verified downloader (ureq + rustls + RustCrypto; SHA-256 and size checked before a file is renamed into place). |

Both are off by default and in wasm builds (`cargo xtask wasm` checks the crate without them).
Pure Rust: `cargo tree -p filmcraft-llm --all-features` has no `cc`, `cmake`, `onig` or C++ build
dependencies.

## Models and licences

Weights are **never bundled with FilmCraft or committed**. They are downloaded on first use, only
after the user opts in, into `<data dir>/models/<id>/`.

| id | weights | size | licence |
|----|---------|------|---------|
| `qwen2.5-0.5b-instruct-q4km` (default, provisional until the 30-case evaluation) | Qwen/Qwen2.5-0.5B-Instruct-GGUF, Q4_K_M | 491 MB + 7 MB tokenizer | Apache-2.0 |
| `qwen2.5-1.5b-instruct-q4km` | Qwen/Qwen2.5-1.5B-Instruct-GGUF, Q4_K_M | 1.12 GB + 7 MB | Apache-2.0 |
| `smollm2-360m-instruct-q8` | HuggingFaceTB/SmolLM2-360M-Instruct-GGUF, Q8_0 | 386 MB + 2 MB | Apache-2.0 |
| `smollm2-1.7b-instruct-q4km` | HuggingFaceTB/SmolLM2-1.7B-Instruct-GGUF, Q4_K_M | 1.06 GB + 2 MB | Apache-2.0 |
| `filmcraft-explainer-qwen05-v1-q8` (fork fine-tune, local) | Qwen2.5-0.5B-Instruct + LoRA, Q8_0 | 525 MB + 7 MB | Apache-2.0 |
| `filmcraft-explainer-smol360-v1-q8` (fork fine-tune, local) | SmolLM2-360M-Instruct + LoRA, Q8_0 | 385 MB + 2 MB | Apache-2.0 |
| `filmcraft-explainer-smol360-v3-q8` (fork fine-tune AI2, local; first choice) | SmolLM2-360M-Instruct + LoRA, Q8_0 (node, comp and branch lines) | 385 MB + 2 MB | Apache-2.0 |

The `filmcraft-explainer-*` entries are the fork's fine-tuned explainers (GRADE-PLAN G2.11b,
[`docs/fork/TRAINING.md`](../../docs/fork/TRAINING.md)): they write the fact checker's controlled
grammar (97 % / 94 % of the 36 held-out cases pass, against 14 % for the best stock model). Their
GGUF has no URL until the owner publishes a release (`ModelInfo::is_local`, `download` refuses);
they run with `ModelInfo::recommended_options()` (no repeat penalty).

Examples: `llm_eval` (runs the held-out cases; `--local id=file.gguf:llama|qwen2` for files
outside the catalogue, `--repeat-penalty`, `--prompts`) and `llm_quantize` (pure-Rust Hugging Face
safetensors → GGUF converter: Q8_0, a Q4_K_M-like mix, F16; `--inspect`, `--compare`).

The tokenizer of each family comes from its base repository (`Qwen/Qwen2.5-0.5B-Instruct`,
`HuggingFaceTB/SmolLM2-360M-Instruct`); the larger sibling of each family ships the identical file.

## Testing

`cargo test -p filmcraft-llm` (catalogue, prompts, GGUF walk on garbage and hostile headers);
`--features infer` adds the loader's hostile-file tests. The real-model test runs only when
`FILMCRAFT_LLM_DIR` points at a folder holding `<model id>/<files>`; tests never download.
To compare models: `cargo run --release -p filmcraft-llm --features infer --example llm_eval --
cases.jsonl [--model ID] [--out out.jsonl]` with one `{"label", "facts", "template"}` object per
line; it writes `{model, label, text, prompt_tokens, gen_tokens, prefill_ms, decode_ms, stop}` lines
(`--tokens-only` compares prompt sizes, `--raw` uses the uncompacted prompt).

## References

The GGUF file format specification published with ggml; the model cards, `tokenizer.json` and
`tokenizer_config.json` (chat templates) published with the weights; the candle and tokenizers
crates' public APIs.
