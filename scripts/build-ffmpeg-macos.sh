#!/usr/bin/env bash
# Builds the FFmpeg that ships inside the macOS app: LGPL only, no GPL parts (no x264, no yadif),
# no third-party libraries, and only the components Xspector Record uses.
#
#   scripts/build-ffmpeg-macos.sh [arm64|x86_64]
#
# Output: src-tauri/binaries/ffmpeg-<rust target triple>  (what Tauri's externalBin expects)
#         build/ffmpeg-<arch>/BUILD_INFO.txt (configure line + versions, for the license notice)
set -euo pipefail

VERSION="9.0.2"
SHA256="8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e" # from the Homebrew formula
URL="https://ffmpeg.org/releases/ffmpeg-${VERSION}.tar.xz"

ARCH="${1:-$(uname -m)}"
case "$ARCH" in
  arm64|aarch64) ARCH=arm64;  TRIPLE=aarch64-apple-darwin ;;
  x86_64)        ARCH=x86_64; TRIPLE=x86_64-apple-darwin ;;
  *) echo "unknown arch $ARCH" >&2; exit 1 ;;
esac

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$ROOT/build/ffmpeg-$ARCH"
SRC="$ROOT/build/ffmpeg-src"
OUT="$ROOT/src-tauri/binaries"
mkdir -p "$WORK" "$OUT" "$ROOT/build"

# Source: download once into its own directory and verify the checksum before unpacking.
if [ ! -f "$SRC/configure" ]; then
  rm -rf "$SRC" && mkdir -p "$SRC"
  TARBALL="$ROOT/build/ffmpeg-${VERSION}.tar.xz"
  [ -f "$TARBALL" ] || curl -fL --retry 3 -o "$TARBALL" "$URL"
  echo "$SHA256  $TARBALL" | shasum -a 256 -c -
  tar -xJf "$TARBALL" -C "$SRC" --strip-components=1
fi

# drawtext (used by the "Fixed" overlay mode) needs FreeType + HarfBuzz. Both are built here as static
# libraries from pinned, checksum-verified sources, so the shipped ffmpeg still links only system frameworks.
# Needs: brew install pkgconf meson ninja
for tool in pkg-config meson ninja; do
  command -v "$tool" >/dev/null || { echo "$tool is needed (brew install pkgconf meson ninja)" >&2; exit 1; }
done
fetch() { # name version url sha256
  local name="$1" ver="$2" url="$3" sha="$4"
  local dir="$ROOT/build/$name-src" tar="$ROOT/build/$name-$ver.tar.xz"
  if [ ! -d "$dir" ]; then
    [ -f "$tar" ] || curl -fL --retry 3 -o "$tar" "$url"
    echo "$sha  $tar" | shasum -a 256 -c - >&2
    mkdir -p "$dir" && tar -xJf "$tar" -C "$dir" --strip-components=1
  fi
  echo "$dir"
}
FT_VERSION="2.15.0"   # FreeType Licence (BSD-style) — shas below come from the Homebrew formulae
FT_SRC="$(fetch freetype "$FT_VERSION" "https://downloads.sourceforge.net/project/freetype/freetype2/${FT_VERSION}/freetype-${FT_VERSION}.tar.xz" 93cd47eb719c325107f2d3cf087d06299e944c3b48a40b55537302304dea1dd5)"
HB_VERSION="15.0.0"   # MIT
HB_SRC="$(fetch harfbuzz "$HB_VERSION" "https://github.com/harfbuzz/harfbuzz/releases/download/${HB_VERSION}/harfbuzz-${HB_VERSION}.tar.xz" 968536e9abd782f07f7954044c0c4020199fcdd39b41d221ec42d2a3be4a44df)"
DEPS="$WORK/deps"
FLAGS="-arch $ARCH -mmacosx-version-min=12.0"

if [ ! -f "$DEPS/lib/libfreetype.a" ]; then
  mkdir -p "$WORK/freetype-build" && (cd "$WORK/freetype-build" &&
    CFLAGS="$FLAGS" "$FT_SRC/configure" --prefix="$DEPS" --disable-shared --enable-static \
      --with-harfbuzz=no --with-brotli=no --with-bzip2=no --with-png=no --with-zlib=no &&
    make -j"$(sysctl -n hw.ncpu)" && make install)
fi
if [ ! -f "$DEPS/lib/libharfbuzz.a" ]; then
  rm -rf "$WORK/harfbuzz-build"
  PKG_CONFIG_PATH="$DEPS/lib/pkgconfig" meson setup "$WORK/harfbuzz-build" "$HB_SRC" --prefix="$DEPS" \
    --default-library=static --buildtype=release \
    -Dfreetype=enabled -Dglib=disabled -Dgobject=disabled -Dcairo=disabled -Dicu=disabled -Dgraphite=disabled \
    -Dcoretext=disabled -Dtests=disabled -Ddocs=disabled -Dbenchmark=disabled -Dintrospection=disabled \
    -Dc_args="$FLAGS" -Dcpp_args="$FLAGS" -Dc_link_args="$FLAGS" -Dcpp_link_args="$FLAGS"
  ninja -C "$WORK/harfbuzz-build" install
fi
export PKG_CONFIG_PATH="$DEPS/lib/pkgconfig"
cp "$FT_SRC/docs/FTL.TXT" "$ROOT/licenses/FreeType-FTL.txt" 2>/dev/null || true
cp "$HB_SRC/COPYING" "$ROOT/licenses/HarfBuzz-COPYING.txt" 2>/dev/null || true

FILTERS="buffer,buffersink,format,null,nullsink,scale,scale2ref,fps,setpts,split,select,separatefields,bwdif,overlay,colorchannelmixer,movie,testsrc2,drawtext"
CONFIG=(
  --prefix="$WORK/install"
  --arch="$ARCH" --cc=clang
  --extra-cflags="-arch $ARCH -mmacosx-version-min=12.0"
  --extra-ldflags="-arch $ARCH -mmacosx-version-min=12.0"
  --disable-gpl --disable-nonfree --disable-version3 --disable-autodetect
  --disable-doc --disable-debug --disable-ffplay --disable-ffprobe --disable-shared --enable-static
  --disable-everything
  --enable-ffmpeg --enable-avfilter --enable-swscale --enable-network --enable-zlib
  --enable-videotoolbox --enable-avfoundation
  --enable-libfreetype --enable-libharfbuzz --pkg-config-flags=--static
  --enable-indev=avfoundation,lavfi
  --enable-protocol=file,pipe,tcp
  --enable-demuxer=rawvideo,yuv4mpegpipe,image2,image2pipe,png_pipe,jpeg_pipe,lavfi
  --enable-muxer=mp4,segment,image2,yuv4mpegpipe,rawvideo,null
  --enable-decoder=rawvideo,mjpeg,h264,hevc,png,wrapped_avframe
  --enable-encoder=h264_videotoolbox,hevc_videotoolbox,mjpeg,rawvideo
  --enable-parser=h264,hevc,mjpeg,png
  --enable-filter="$FILTERS"
)
[ "$ARCH" != "$(uname -m)" ] && CONFIG+=(--enable-cross-compile --target-os=darwin)

cd "$WORK"
"$SRC/configure" "${CONFIG[@]}"
make -j"$(sysctl -n hw.ncpu)"
cp ffmpeg "$OUT/ffmpeg-$TRIPLE"
strip -x "$OUT/ffmpeg-$TRIPLE"

{
  echo "FFmpeg $VERSION ($URL)"
  echo "sha256 $SHA256"
  echo "configure: ${CONFIG[*]}"
  "$OUT/ffmpeg-$TRIPLE" -hide_banner -version | sed -n 1,3p
} > "$WORK/BUILD_INFO.txt"
echo "built $OUT/ffmpeg-$TRIPLE"
cat "$WORK/BUILD_INFO.txt"
