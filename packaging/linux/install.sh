#!/usr/bin/env bash
# Install the extracted Linux tarball. No sudo is needed for the default prefix.
set -euo pipefail

usage() {
  echo 'Usage: ./install.sh [--prefix /absolute/path]'
  echo 'Installs Revolution and its desktop integration into ~/.local by default.'
  echo 'For all users: sudo ./install.sh --prefix /usr/local'
}
fail() { echo "error: $*" >&2; exit 1; }
prefix="${HOME:?HOME is not set}/.local"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix)
      [ "$#" -ge 2 ] || fail '--prefix needs an absolute path'
      prefix="$2"; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; fail "unknown argument: $1" ;;
  esac
done
case "$prefix" in /*) ;; *) fail '--prefix must be an absolute path' ;; esac
prefix="$(realpath -m -- "$prefix")"
# Desktop Exec paths cannot contain '='; launchers also misinterpret '%' in executable names.
[[ "$prefix" != *'='* && "$prefix" != *'%'* && ! "$prefix" =~ [[:cntrl:]] ]] || fail 'prefix cannot contain =, % or control characters'
[ "$prefix" != / ] || fail 'use /usr/local or a user directory, not /'
source_dir="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)"
case "$prefix/" in "$source_dir/"*) fail 'choose an installation prefix outside the extracted tarball' ;; esac
app_id=io.github.dawgon2.Revolution
for binary in revolution revolution-cli; do
  [ -f "$source_dir/bin/$binary" ] || fail "missing bin/$binary; extract the complete tarball first"
done
[ -f "$source_dir/share/applications/$app_id.desktop" ] || fail 'missing desktop entry; extract the complete tarball first'

mkdir -p -- "$prefix/bin" "$prefix/share"
install -m755 -- "$source_dir/bin/revolution" "$prefix/bin/revolution"
install -m755 -- "$source_dir/bin/revolution-cli" "$prefix/bin/revolution-cli"
cp -R -- "$source_dir/share/." "$prefix/share/"

# Use an absolute path so launching from the application menu does not depend on PATH.
# Escape the Exec argument, then the desktop entry's string value (two separate layers).
# https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html
# Every Exec line keeps its arguments (`%F`, the actions' `--empty` / `--new-instance`).
exec_value() { printf '%s' "$1" | sed 's/[\\"`$]/\\&/g; s/\\/\\\\/g'; }
try_value() { printf '%s' "$1" | sed 's/\\/\\\\/g'; }
# rewrite_exec <program name> <absolute path> <file>: `Exec=<name> args` -> `Exec="<path>" args`
rewrite_exec() {
  local name="$1" exec_path try_path line
  exec_path="$(exec_value "$2")"
  try_path="$(try_value "$2")"
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      "Exec=$name"*) printf 'Exec="%s"%s\n' "$exec_path" "${line#"Exec=$name"}" ;;
      TryExec=*) printf 'TryExec=%s\n' "$try_path" ;;
      *) printf '%s\n' "$line" ;;
    esac
  done <"$3"
}
rewrite_exec revolution "$prefix/bin/revolution" "$source_dir/share/applications/$app_id.desktop" >"$prefix/share/applications/$app_id.desktop"
# The thumbnailer runs without our PATH (GNOME runs it in a sandbox): absolute path too.
thumbnailer="share/thumbnailers/$app_id.thumbnailer"
if [ -f "$source_dir/$thumbnailer" ]; then
  rewrite_exec revolution-cli "$prefix/bin/revolution-cli" "$source_dir/$thumbnailer" >"$prefix/$thumbnailer"
fi

# Cache tools are optional; installing binaries must also work on minimal systems.
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q "$prefix/share/applications" || echo 'warning: desktop cache refresh failed' >&2
fi
if command -v update-mime-database >/dev/null 2>&1; then
  update-mime-database "$prefix/share/mime" || echo 'warning: MIME cache refresh failed' >&2
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q -t -f "$prefix/share/icons/hicolor" || echo 'warning: icon cache refresh failed' >&2
fi
printf 'Installed Revolution in %s\nLaunch it from the application menu or run "%s/bin/revolution".\n' "$prefix" "$prefix"
case ":${PATH:-}:" in
  *":$prefix/bin:"*) ;;
  *) printf 'For terminal commands, add %s/bin to your PATH.\n' "$prefix" ;;
esac
