#!/usr/bin/env bash
# Build and package Revolution for Linux (<arch> is x86_64 or aarch64, or riscv64 when cross-compiling).
# The programs are built as filmcraft / filmcraft-cli and installed as revolution / revolution-cli
# (fork rebrand, FORK-PLAN §1a.103 / §1a.107; internals keep their filmcraft-* names):
#
# <tag> below is $FILMCRAFT_RELEASE_TAG (revolution-0.1.3), else revolution-<version> (env.sh):
#
#   $DIST/<tag>-linux-<arch>.AppImage        any distro with glibc >= the build host's
#   $DIST/<tag>-linux-<arch>.AppImage.zsync  delta updates (needs zsyncmake)
#   $DIST/<tag>-linux-<arch>.deb             Debian, Ubuntu, Mint, Pop!_OS, ...
#   $DIST/<tag>-linux-<arch>.rpm             Fedora, openSUSE, RHEL, ...
#   $DIST/<tag>-linux-<arch>.tar.gz          portable folder <tag>/ (the programs + NOTES.md; what
#                                            the in-app updater installs)
#   $DIST/<tag>-linux-<arch>-fhs.tar.gz      FHS-style tree (bin/, share/, install.sh); the Flatpak
#                                            bundle is made from it
#
# Usage: packaging/linux/package.sh [--skip-build] [--formats "appimage deb rpm tar portable"]
# NOTES.md for the portable tarball: $REVOLUTION_NOTES (a file), else a short default.
#
# Needs: cargo; nfpm for deb/rpm (https://nfpm.goreleaser.com); appimagetool for the AppImage
# (downloaded into $CARGO_TARGET_DIR if missing). Build on an old distro (CI: Ubuntu 22.04,
# glibc 2.35) so the binaries run on newer ones. Optional: desktop-file-validate, appstreamcli,
# zsyncmake (the zsync package) for the AppImage's .zsync.
#
# Cross-compiling (CI's riscv64 job, tar only): set CROSS_ARCH (riscv64), CROSS_TARGET (the Rust
# target), CROSS_COMPILE (binutils prefix, for strip) and optionally EMULATOR (qemu-riscv64) to
# smoke-test the CLI.
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/linux"
APP_ID=io.github.dawgon2.Revolution
PROGRAM=revolution
CLI=revolution-cli

SKIP_BUILD=0
FORMATS="appimage deb rpm tar portable"
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-build) SKIP_BUILD=1; shift ;;
    --formats) FORMATS="$2"; shift 2 ;;
    -h | --help) sed -n '2,26p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

ARCH="${CROSS_ARCH:-$(uname -m)}"
case "$ARCH" in
  riscv64) DEB_ARCH=riscv64 ;;
  x86_64) DEB_ARCH=amd64 ;;
  aarch64 | arm64) ARCH=aarch64; DEB_ARCH=arm64 ;;
  *) echo "unsupported architecture $ARCH" >&2; exit 2 ;;
esac
export FILMCRAFT_MAINTAINER="${FILMCRAFT_MAINTAINER:-dawgon2 <dawgon2@users.noreply.github.com>}"
BASENAME="$RELEASE_NAME-linux-$ARCH"

echo "==> Revolution $VERSION for Linux $ARCH ($FORMATS)"

if [ "$SKIP_BUILD" = 0 ]; then
  if [ -n "${CROSS_TARGET:-}" ]; then
    (cd "$ROOT" && cargo build --release --locked -p filmcraft -p filmcraft-cli --target "$CROSS_TARGET")
  else
    (cd "$ROOT" && cargo build --release --locked -p filmcraft -p filmcraft-cli)
  fi
fi
BIN="$CARGO_TARGET_DIR/${CROSS_TARGET:+$CROSS_TARGET/}release"
WORK="$CARGO_TARGET_DIR/linux-package"
STAGE="$WORK/root"
rm -rf "$WORK"

# ---- stage an FHS tree (shared by every format) -------------------------------------------------
install -Dm755 "$BIN/filmcraft" "$STAGE/usr/bin/$PROGRAM"
install -Dm755 "$BIN/filmcraft-cli" "$STAGE/usr/bin/$CLI"
"${CROSS_COMPILE:-}strip" "$STAGE/usr/bin/$PROGRAM" "$STAGE/usr/bin/$CLI" 2>/dev/null || true
install -Dm644 "$HERE/$APP_ID.desktop" "$STAGE/usr/share/applications/$APP_ID.desktop"
install -Dm644 "$HERE/$APP_ID.mime.xml" "$STAGE/usr/share/mime/packages/$APP_ID.xml"
# project-file thumbnails for GNOME-family file managers (Dolphin: fork/kde-thumbnailer)
install -Dm644 "$HERE/$APP_ID.thumbnailer" "$STAGE/usr/share/thumbnailers/$APP_ID.thumbnailer"
mkdir -p "$STAGE/usr/share/metainfo"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@DATE@/$FILMCRAFT_BUILD_DATE/g" \
  "$HERE/$APP_ID.metainfo.xml.in" >"$STAGE/usr/share/metainfo/$APP_ID.metainfo.xml"
mkdir -p "$STAGE/usr/share/icons"
cp -R "$ROOT/assets/app-icon/hicolor" "$STAGE/usr/share/icons/"
find "$STAGE/usr/share/icons" -name '*.attribution' -delete
mkdir -p "$STAGE/usr/share/doc/$PROGRAM"
copy_docs "$STAGE/usr/share/doc/$PROGRAM"

if command -v desktop-file-validate >/dev/null; then
  desktop-file-validate "$STAGE/usr/share/applications/$APP_ID.desktop"
fi
if command -v appstreamcli >/dev/null; then
  appstreamcli validate --no-net --explain "$STAGE/usr/share/metainfo/$APP_ID.metainfo.xml"
fi

has() { case " $FORMATS " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }

# ---- -fhs.tar.gz (FHS tree + install.sh) -------------------------------------------------------
if has tar; then
  mkdir -p "$WORK/tar"
  cp -R "$STAGE/usr" "$WORK/tar/$BASENAME"
  install -m755 "$HERE/install.sh" "$WORK/tar/$BASENAME/install.sh"
  tar -C "$WORK/tar" -czf "$DIST/$BASENAME-fhs.tar.gz" "$BASENAME"
  echo "wrote $DIST/$BASENAME-fhs.tar.gz"
fi

# ---- portable .tar.gz (the fork's build layout, what the updater installs) ----------------------
# One folder `revolution-<version>/` with the two programs, NOTES.md and the licences: the layout
# of every Revolution release so far (`update_install` unpacks exactly this).
if has portable; then
  # Named after the release tag (revolution-0.2 even when the package version is 0.2.0): the
  # updater looks for `<tag>-linux-<arch>.tar.gz` holding `<tag>/`.
  TOP="$RELEASE_NAME"
  P="$WORK/portable/$TOP"
  mkdir -p "$P"
  install -m755 "$STAGE/usr/bin/$PROGRAM" "$P/$PROGRAM"
  install -m755 "$STAGE/usr/bin/$CLI" "$P/$CLI"
  for f in LICENSE-MIT LICENSE-APACHE ATTRIBUTION.md; do
    if [ -f "$ROOT/$f" ]; then cp "$ROOT/$f" "$P/"; fi
  done
  if [ -n "${REVOLUTION_NOTES:-}" ] && [ -f "$REVOLUTION_NOTES" ]; then
    cp "$REVOLUTION_NOTES" "$P/NOTES.md"
  else
    printf '# Revolution %s\n\nRun ./%s to start. Release notes: https://github.com/dawgon2/revolution/releases\n' "$VERSION" "$PROGRAM" >"$P/NOTES.md"
  fi
  tar -C "$WORK/portable" --owner=0 --group=0 -czf "$DIST/$TOP-linux-$ARCH.tar.gz" "$TOP"
  echo "wrote $DIST/$TOP-linux-$ARCH.tar.gz"
fi

# ---- .deb / .rpm --------------------------------------------------------------------------------
if has deb || has rpm; then
  command -v nfpm >/dev/null || { echo "error: nfpm not found (https://nfpm.goreleaser.com/install/)" >&2; exit 1; }
  export VERSION
  export NFPM_ARCH="$DEB_ARCH"
  # nfpm expands env vars in fields like `version` and `arch`, but not in `contents[].src`.
  sed "s|\${STAGE}|$STAGE|g" "$HERE/nfpm.yaml" >"$WORK/nfpm.yaml"
  for fmt in deb rpm; do
    if has "$fmt"; then (cd "$ROOT" && nfpm package -f "$WORK/nfpm.yaml" -p "$fmt" -t "$DIST/$BASENAME.$fmt"); fi
  done
fi

# ---- AppImage -----------------------------------------------------------------------------------
if has appimage; then
  APPDIR="$WORK/Revolution.AppDir"
  cp -R "$STAGE" "$APPDIR"
  mv "$APPDIR/usr/share/doc" "$WORK/doc-unused"
  ln -s "usr/bin/$PROGRAM" "$APPDIR/AppRun"
  cp "$HERE/$APP_ID.desktop" "$APPDIR/$APP_ID.desktop"
  cp "$ROOT/assets/app-icon/hicolor/256x256/apps/$APP_ID.png" "$APPDIR/$APP_ID.png"
  ln -s "$APP_ID.png" "$APPDIR/.DirIcon"

  TOOL="${APPIMAGETOOL:-$(command -v appimagetool || true)}"
  if [ -z "$TOOL" ]; then
    TOOL="$CARGO_TARGET_DIR/appimagetool-$ARCH.AppImage"
    if [ ! -x "$TOOL" ]; then
      curl -fsSL -o "$TOOL" "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"
      chmod +x "$TOOL"
    fi
  fi
  # Absolute, because appimagetool runs in $DIST below (CARGO_TARGET_DIR or APPIMAGETOOL may be
  # relative, e.g. target/agent-<name>).
  OUT="$(cd "$DIST" && pwd)/$BASENAME.AppImage"
  APPDIR="$(cd "$APPDIR" && pwd)"
  TOOL="$(cd "$(dirname "$TOOL")" && pwd)/$(basename "$TOOL")"
  # A .zsync left by an earlier run would hide a missing zsyncmake and describe another file.
  rm -f "$OUT.zsync"
  # Update information: AppImageUpdate, AppImageLauncher and the like read it from the file and
  # fetch only the blocks that changed in a newer release, through the .zsync published next to
  # each AppImage on GitHub Releases. `latest` is the newest published release that is not a
  # pre-release. A fork's builds point at its own releases through GITHUB_REPOSITORY.
  REPO="${GITHUB_REPOSITORY:-dawgon2/revolution}"
  UPDATE_INFO="gh-releases-zsync|${REPO%%/*}|${REPO#*/}|latest|$PROGRAM-*-linux-$ARCH.AppImage.zsync"
  # Extract-and-run: works without FUSE (containers, CI). The output embeds the static runtime,
  # so users don't need libfuse2 either. With zsyncmake on the host (CI installs the zsync
  # package) appimagetool also writes the .zsync, into its working directory, hence the cd.
  (cd "$DIST" && ARCH="$ARCH" APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream -u "$UPDATE_INFO" "$APPDIR" "$OUT")
  echo "wrote $OUT"
  if [ -s "$OUT.zsync" ]; then
    echo "wrote $OUT.zsync"
  else
    warn "zsyncmake not found, so $OUT.zsync was not written; AppImage delta updates need it"
  fi
fi

if [ -z "${CROSS_TARGET:-}" ]; then
  "$STAGE/usr/bin/$CLI" --version
elif [ -n "${EMULATOR:-}" ]; then
  "$EMULATOR" "$STAGE/usr/bin/$CLI" --version
fi
echo "==> done"
ls -lh "$DIST"
