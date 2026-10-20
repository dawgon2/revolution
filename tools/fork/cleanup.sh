#!/usr/bin/env bash
# Free disk space after a phase (owner, 2026-10-20, FORK-PLAN §1a.109), safely for the project:
# only what a build or a bench can make again. Reports what it would free; `--apply` deletes.
#
#   tools/fork/cleanup.sh            # dry run: sizes only
#   tools/fork/cleanup.sh --apply    # delete
#
# Removed:
#   - cargo's incremental caches (target/*/incremental, target/agent-*/*/incremental)
#   - build artifacts not touched for 7 days (target/*/deps, build, .fingerprint): stale versions
#     of crates; anything still needed is rebuilt on the next build
#   - bench media in target/fork-bench/bench-* (video files only; tables / logs / json stay);
#     folders named keep-* are kept whole
#   - target/tmp/*, /tmp/filmcraft-* test folders older than a day, /var/tmp/revolution-* release
#     build trees (run after the release is published)
#   - ~/Apps/filmcraft-builds: release-staging, and builds that are neither `latest`, a tag in
#     this repo, nor among the 3 newest
#   - clean git worktrees under .claude/worktrees whose branch is merged into hevc-vaapi and that
#     nothing touched for 7 days (never the Dell lane's)
#     (`git worktree remove` without --force: anything uncommitted keeps the worktree)
# Never touched: sources, .git, target/fixtures (tests and benches need them), models
# (target/ai3-models, models/), data folders, the owner's media and projects.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APPLY=0
[ "${1:-}" = "--apply" ] && APPLY=1
BUILDS="$HOME/Apps/filmcraft-builds"
total=0

size() { du -sk "$@" 2>/dev/null | awk '{s+=$1} END {print s+0}'; }
gb() { awk -v k="$1" 'BEGIN {printf "%8.1f", k / 1048576}'; }
drop() { # label, paths...
  local label="$1"; shift
  [ "$#" -eq 0 ] && return
  local kb; kb=$(size "$@")
  [ "$kb" -eq 0 ] && return
  total=$((total + kb))
  printf '%s GB  %s\n' "$(gb "$kb")" "$label"
  if [ "$APPLY" = 1 ]; then rm -rf -- "$@"; fi
}

cd "$ROOT" || exit 1
mapfile -t inc < <(find target -maxdepth 3 -type d -name incremental 2>/dev/null)
drop "cargo incremental caches" "${inc[@]}"
mapfile -t stale < <(find target -mindepth 3 -maxdepth 4 \( -path '*/deps/*' -o -path '*/build/*' -o -path '*/.fingerprint/*' \) \
  -not -path 'target/fixtures/*' -not -path 'target/fork-bench/*' -not -path 'target/ai3-models/*' -mtime +7 2>/dev/null)
drop "build artifacts untouched for 7 days" "${stale[@]}"
mapfile -t media < <(find target/fork-bench -path 'target/fork-bench/keep-*' -prune -o -type f \
  \( -iname '*.mov' -o -iname '*.mp4' -o -iname '*.mkv' -o -iname '*.hevc' -o -iname '*.h264' -o -iname '*.mxf' -o -iname '*.y4m' -o -iname '*.raw' \) -print 2>/dev/null)
drop "bench media (results kept)" "${media[@]}"
mapfile -t tmp < <(find target/tmp -mindepth 1 -maxdepth 1 2>/dev/null; find /tmp -maxdepth 1 -name 'filmcraft-*' -user "$(id -u)" -mmin +1440 2>/dev/null; find /var/tmp -maxdepth 1 -name 'revolution-*' -user "$(id -u)" 2>/dev/null)
drop "temporary folders" "${tmp[@]}"

if [ -d "$BUILDS" ]; then
  latest=$(readlink -f "$BUILDS/latest" 2>/dev/null || true)
  mapfile -t tags < <(git tag)
  mapfile -t newest < <(find "$BUILDS" -mindepth 1 -maxdepth 1 -type d -not -name release-staging -printf '%T@ %p\n' | sort -rn | head -3 | cut -d' ' -f2-)
  old=()
  for b in "$BUILDS"/*/; do
    b="${b%/}"; n="$(basename "$b")"
    [ "$n" = release-staging ] && { old+=("$b"); continue; }
    [ "$(readlink -f "$b")" = "$latest" ] && continue
    printf '%s\n' "${tags[@]}" | grep -qxF -e "$n" -e "fork-$n" -e "revolution-$n" && continue
    printf '%s\n' "${newest[@]}" | grep -qxF "$b" && continue
    old+=("$b")
  done
  drop "old app builds (latest, tagged and 3 newest kept)" "${old[@]}"
fi

while read -r wt branch; do
  [ -z "$wt" ] && continue
  # standing lanes (the Dell lane) and anything worked on this week stay
  case "$branch" in dell-lane|revolution-wip|hevc-vaapi) continue ;; esac
  [ -n "$(find "$wt" -maxdepth 1 -newermt '-7 days' -print -quit 2>/dev/null)" ] && continue
  git merge-base --is-ancestor "$branch" hevc-vaapi 2>/dev/null || continue
  [ -n "$(git -C "$wt" status --porcelain 2>/dev/null)" ] && continue
  kb=$(size "$wt"); total=$((total + kb))
  printf '%s GB  merged worktree %s\n' "$(gb "$kb")" "$wt"
  [ "$APPLY" = 1 ] && git worktree remove "$wt"
done < <(git worktree list --porcelain | awk '/^worktree /{w=$2} /^branch /{sub("refs/heads/","",$2); if (w ~ /\.claude\/worktrees\//) print w, $2}')

printf '%s GB  total %s\n' "$(gb "$total")" "$([ "$APPLY" = 1 ] && echo freed || echo 'would be freed (dry run; --apply deletes)')"
