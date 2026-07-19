#!/usr/bin/env bash
# Build .deb and AppImage packages from a release binary.
#
# Usage:
#   scripts/package-linux.sh <binary> <version> <arch> <out-dir>
#
# <arch> is Rust/target style: x86_64 | aarch64
# Version may include a leading "v" (stripped for package metadata).
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: $0 <binary> <version> <arch> <out-dir>" >&2
  exit 2
fi

BINARY=$(readlink -f "$1")
VERSION_RAW="$2"
ARCH="$3"
OUT_DIR=$(mkdir -p "$4" && readlink -f "$4")
ROOT=$(cd "$(dirname "$0")/.." && pwd)

VERSION="${VERSION_RAW#v}"
case "$ARCH" in
  x86_64|amd64)
    ARCH=x86_64
    DEB_ARCH=amd64
    LINUXDEPLOY_ARCH=x86_64
    ;;
  aarch64|arm64)
    ARCH=aarch64
    DEB_ARCH=arm64
    LINUXDEPLOY_ARCH=aarch64
    ;;
  *)
    echo "unsupported arch: $ARCH (expected x86_64 or aarch64)" >&2
    exit 1
    ;;
esac

if [[ ! -x "$BINARY" && ! -f "$BINARY" ]]; then
  echo "binary not found: $BINARY" >&2
  exit 1
fi

DESKTOP_SRC="$ROOT/assets/gog-conjure.desktop"
ICON_SRC="$ROOT/assets/icon-512.png"
if [[ ! -f "$ICON_SRC" ]]; then
  ICON_SRC="$ROOT/assets/icon.png"
fi
if [[ ! -f "$DESKTOP_SRC" || ! -f "$ICON_SRC" ]]; then
  echo "missing desktop/icon assets under assets/" >&2
  exit 1
fi

RAW_NAME="gog-conjure-linux-${ARCH}"
DEB_NAME="gog-conjure-linux-${ARCH}.deb"
APPIMAGE_NAME="gog-conjure-linux-${ARCH}.AppImage"

# linuxdeploy only accepts standard icon pixel sizes (not arbitrary source art).
ICON_WORK=$(mktemp -d)
ICON_512="$ICON_WORK/gog-conjure.png"
if command -v convert >/dev/null 2>&1; then
  convert "$ICON_SRC" -resize 512x512 "$ICON_512"
elif python3 -c 'import PIL' >/dev/null 2>&1; then
  python3 - "$ICON_SRC" "$ICON_512" <<'PY'
import sys
from PIL import Image
src, dst = sys.argv[1], sys.argv[2]
Image.open(src).convert("RGBA").resize((512, 512), Image.Resampling.LANCZOS).save(dst)
PY
else
  # Prefer the committed 512² asset when no resizer is available.
  cp "$ICON_SRC" "$ICON_512"
fi

install -D -m 755 "$BINARY" "$OUT_DIR/$RAW_NAME"

# --- .deb -------------------------------------------------------------------
DEB_ROOT=$(mktemp -d)
trap 'rm -rf "$DEB_ROOT" "${APPDIR:-}" "$ICON_WORK" "${TOOLS:-}"' EXIT

install -D -m 755 "$BINARY" "$DEB_ROOT/usr/bin/gog-conjure"
install -D -m 644 "$DESKTOP_SRC" "$DEB_ROOT/usr/share/applications/gog-conjure.desktop"
install -D -m 644 "$ICON_512" "$DEB_ROOT/usr/share/icons/hicolor/512x512/apps/gog-conjure.png"
install -D -m 644 "$ICON_512" "$DEB_ROOT/usr/share/pixmaps/gog-conjure.png"

mkdir -p "$DEB_ROOT/DEBIAN"
SIZE_KB=$(du -sk "$DEB_ROOT/usr" | awk '{print $1}')
cat >"$DEB_ROOT/DEBIAN/control" <<EOF
Package: gog-conjure
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: ${DEB_ARCH}
Maintainer: gog-conjure contributors
Installed-Size: ${SIZE_KB}
Depends: libgtk-3-0, libwebkit2gtk-4.1-0 | libwebkit2gtk-4.0-37, libxkbcommon0, libc6
Description: GOG library downloader with desktop UI
 Cross-platform GOG library downloader and disc burner with a
 distinctive egui desktop interface.
EOF

dpkg-deb --build --root-owner-group "$DEB_ROOT" "$OUT_DIR/$DEB_NAME"
echo "wrote $OUT_DIR/$DEB_NAME"

# --- AppImage ---------------------------------------------------------------
if ! command -v curl >/dev/null 2>&1; then
  echo "curl required to fetch linuxdeploy" >&2
  exit 1
fi

TOOLS=$(mktemp -d)

LINUXDEPLOY="$TOOLS/linuxdeploy-${LINUXDEPLOY_ARCH}.AppImage"
PLUGIN_GTK="$TOOLS/linuxdeploy-plugin-gtk.sh"
BASE_URL="https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous"
PLUGIN_URL="https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh"

curl -fsSL -o "$LINUXDEPLOY" \
  "${BASE_URL}/linuxdeploy-${LINUXDEPLOY_ARCH}.AppImage"
curl -fsSL -o "$PLUGIN_GTK" "$PLUGIN_URL"
chmod +x "$LINUXDEPLOY" "$PLUGIN_GTK"
export PATH="$TOOLS:$PATH"

APPDIR=$(mktemp -d)
install -D -m 755 "$BINARY" "$APPDIR/usr/bin/gog-conjure"
install -D -m 644 "$DESKTOP_SRC" "$APPDIR/usr/share/applications/gog-conjure.desktop"
install -D -m 644 "$ICON_512" "$APPDIR/usr/share/icons/hicolor/512x512/apps/gog-conjure.png"

# Avoid FUSE on CI/Act runners.
export APPIMAGE_EXTRACT_AND_RUN=1
export LINUXDEPLOY_OUTPUT_VERSION="$VERSION"
export DEPLOY_GTK_VERSION=3

(
  cd "$OUT_DIR"
  "$LINUXDEPLOY" \
    --appimage-extract-and-run \
    --appdir "$APPDIR" \
    --executable "$APPDIR/usr/bin/gog-conjure" \
    --desktop-file "$APPDIR/usr/share/applications/gog-conjure.desktop" \
    --icon-file "$APPDIR/usr/share/icons/hicolor/512x512/apps/gog-conjure.png" \
    --plugin gtk \
    --output appimage
)

# Normalize linuxdeploy's default AppImage filename.
shopt -s nullglob
produced=("$OUT_DIR"/gog-conjure*.AppImage)
if [[ ${#produced[@]} -eq 0 ]]; then
  echo "linuxdeploy did not produce an AppImage" >&2
  exit 1
fi
mv -f "${produced[0]}" "$OUT_DIR/$APPIMAGE_NAME"
chmod +x "$OUT_DIR/$APPIMAGE_NAME"
echo "wrote $OUT_DIR/$APPIMAGE_NAME"

ls -lah "$OUT_DIR/$RAW_NAME" "$OUT_DIR/$DEB_NAME" "$OUT_DIR/$APPIMAGE_NAME"
