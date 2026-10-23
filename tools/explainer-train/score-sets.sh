#!/usr/bin/env bash
# Score a tuned explainer on every held-out set (fork AI2 item 4; docs/fork/TRAINING.md):
# sets 1-3 (node lines), 4 (comp lines), 5 (branch notes), 6 (G4's tools, AI2b) and 7 (C3 / C4
# comp nodes, AI2b), one eval + score per set.
#
#   tools/explainer-train/score-sets.sh <name> [family]
#     <name>    run name whose quantized weights are in $MODELS/tuned/<name>/<name>-<quant>.gguf
#     [family]  llama (SmolLM2, default) | qwen2 (Qwen2.5)
#
# Environment (all optional):
#   SETS="1 2 3 4 5 6 7"   sets to score
#   QUANTS=q8_0            quantizations to evaluate (space separated)
#   REPEAT_PENALTY=1.0     eval generation setting (tuned models run without a penalty)
#   MODELS, RUNS           as in run.sh
#
# Writes $RUNS/<name>/sets/{cases,eval,score}-<set>.* and prints one summary line per set
# (the first lines of each score.md). Run run.sh's eval / score stages for the set-1-only path.
# Offline tooling only: nothing here is part of FilmCraft or bundled with it.
set -euo pipefail

name="${1:-}"
family="${2:-llama}"
if [[ -z "$name" ]]; then
  sed -n '2,18p' "$0"
  exit 2
fi

repo="$(cd "$(dirname "$0")/../.." && pwd)"
MODELS="${MODELS:-$HOME/.local/share/filmcraft-fork/models}"
RUNS="${RUNS:-$HOME/Apps/filmcraft-train/runs}"
SETS="${SETS:-1 2 3 4 5 6 7}"
QUANTS="${QUANTS:-q8_0}"
tuned="$MODELS/tuned/$name"
out="$RUNS/$name/sets"
mkdir -p "$out"
cargo_bg() { if [ -x "$HOME/Apps/filmcraft-builds/fc-cargo.sh" ]; then "$HOME/Apps/filmcraft-builds/fc-cargo.sh" "$@"; else nice -n 19 ionice -c 3 cargo "$@"; fi; }

locals=()
for q in $QUANTS; do
  gguf="$tuned/$name-$q.gguf"
  if [[ ! -f "$gguf" ]]; then
    echo "missing $gguf (quantize first: run.sh with STAGES=quantize)" >&2
    exit 1
  fi
  locals+=(--local "$name-$q=$gguf:$family")
done

cd "$repo"
export FILMCRAFT_LLM_DIR="$MODELS"
for s in $SETS; do
  cases="$out/cases-$s.jsonl"
  case "$s" in
    1|2|3|5|6) cargo_bg run --release -q -p filmcraft-render --example explain_score -- --dump-cases "$cases" --set "$s"
             score_sel=(--set "$s") ;;
    4|7)     cargo_bg run --release -q -p filmcraft-engine --example comp_traindata -- --dump-cases "$cases" --set "$s"
             score_sel=(--cases "$cases") ;;
    *) echo "unknown set $s (1 to 7)" >&2; exit 2 ;;
  esac
  cargo_bg run --release -q -p filmcraft-llm --features infer --example llm_eval -- \
    "$cases" "${locals[@]}" --repeat-penalty "${REPEAT_PENALTY:-1.0}" --out "$out/eval-$s.jsonl" \
    2> "$out/eval-$s.log" > "$out/eval-$s.summary"
  cargo_bg run --release -q -p filmcraft-render --example explain_score -- "$out/eval-$s.jsonl" "${score_sel[@]}" \
    --summary "$out/score-$s.md" --json "$out/score-$s.json" --fails > /dev/null
  echo "== set $s"
  head -n 6 "$out/score-$s.md"
done
