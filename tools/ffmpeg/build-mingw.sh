#!/usr/bin/env bash
# Builds the trimmed, LGPL-only, static FFmpeg the Windows receiver links
# (spec 4.5): HEVC decoder + parser + D3D11VA hwaccel, nothing else.
#
# Cross-compiles for x86_64-pc-windows-gnu with mingw-w64. Works on Linux
# (apt: mingw-w64 nasm) and in MSYS2.
#
# Usage: tools/ffmpeg/build-mingw.sh [prefix]
# Default prefix: third_party/ffmpeg-build (gitignored). The receiver's build
# script looks there, or wherever ANNEX_FFMPEG_DIR points.
set -euo pipefail

FFMPEG_TAG="${FFMPEG_TAG:-n8.1.3}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PREFIX="${1:-$ROOT/third_party/ffmpeg-build}"
SRC="${FFMPEG_SRC:-$ROOT/third_party/ffmpeg-src}"
CROSS="${CROSS_PREFIX:-x86_64-w64-mingw32-}"

if [ ! -d "$SRC/.git" ]; then
  git clone --depth 1 --branch "$FFMPEG_TAG" https://github.com/FFmpeg/FFmpeg.git "$SRC"
fi

cd "$SRC"
make distclean >/dev/null 2>&1 || true

./configure \
  --prefix="$PREFIX" \
  --target-os=mingw32 --arch=x86_64 --enable-cross-compile --cross-prefix="$CROSS" \
  --pkg-config=pkg-config \
  --enable-static --disable-shared \
  --disable-everything --disable-autodetect \
  --enable-w32threads \
  --enable-d3d11va \
  --enable-decoder=hevc --enable-parser=hevc \
  --enable-hwaccel=hevc_d3d11va --enable-hwaccel=hevc_d3d11va2 \
  --disable-programs --disable-doc --disable-network \
  --disable-avdevice --disable-avformat --disable-avfilter \
  --disable-swscale --disable-swresample \
  --disable-debug \
  --extra-cflags="-O2"

make -j"$(nproc 2>/dev/null || echo 4)"
make install

# The configure line is the licence record: no --enable-gpl / --enable-nonfree.
grep -q "enable-gpl\|enable-nonfree" "$PREFIX/lib/pkgconfig/libavcodec.pc" && {
  echo "refusing: FFmpeg was configured with GPL or nonfree parts" >&2
  exit 1
}
echo "FFmpeg $FFMPEG_TAG installed to $PREFIX"
