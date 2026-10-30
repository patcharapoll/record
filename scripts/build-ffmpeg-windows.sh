#!/usr/bin/env bash
# UNTESTED draft — written alongside the verified macOS script, never run on Windows yet.
#
# Builds the LGPL FFmpeg for the Windows installer. Run inside an MSYS2 "MINGW64" shell on Windows
# with: pacman -S --needed base-devel mingw-w64-x86_64-toolchain mingw-w64-x86_64-nasm
#       mingw-w64-x86_64-ffnvcodec-headers mingw-w64-x86_64-amf-headers
#       mingw-w64-x86_64-freetype mingw-w64-x86_64-harfbuzz mingw-w64-x86_64-zlib mingw-w64-x86_64-pkgconf   (static libs, for drawtext)
#
# Output: src-tauri/binaries/ffmpeg-x86_64-pc-windows-msvc.exe   (name Tauri's externalBin expects)
set -euo pipefail

VERSION="9.0.2"
SHA256="8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e"
URL="https://ffmpeg.org/releases/ffmpeg-${VERSION}.tar.xz"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/build/ffmpeg-src"; WORK="$ROOT/build/ffmpeg-win64"; OUT="$ROOT/src-tauri/binaries"
mkdir -p "$ROOT/build" "$WORK" "$OUT"

if [ ! -f "$SRC/configure" ]; then
  rm -rf "$SRC" && mkdir -p "$SRC"
  TARBALL="$ROOT/build/ffmpeg-${VERSION}.tar.xz"
  [ -f "$TARBALL" ] || curl -fL --retry 3 -o "$TARBALL" "$URL"
  echo "$SHA256  $TARBALL" | sha256sum -c -
  tar -xJf "$TARBALL" -C "$SRC" --strip-components=1
fi

# Encoders: NVENC and AMF (header-only, loaded from the GPU driver at run time) and Media Foundation
# (Windows' own API: Intel/AMD/NVIDIA hardware when present, Microsoft's software H.264 encoder
# otherwise). Intel QuickSync (libvpl) is left out for now: it needs an extra DLL next to the app.
cd "$WORK"
"$SRC/configure" \
  --arch=x86_64 --target-os=mingw32 --prefix="$WORK/install" \
  --disable-gpl --disable-nonfree --disable-version3 --disable-autodetect \
  --disable-doc --disable-debug --disable-ffplay --disable-ffprobe --disable-shared --enable-static \
  --disable-everything \
  --enable-ffmpeg --enable-avfilter --enable-swscale --enable-network --enable-zlib \
  --enable-indev=dshow,lavfi \
  --enable-protocol=file,pipe,tcp \
  --enable-demuxer=rawvideo,yuv4mpegpipe,image2,image2pipe,png_pipe,jpeg_pipe,lavfi \
  --enable-muxer=mp4,segment,image2,yuv4mpegpipe,rawvideo,null \
  --enable-decoder=rawvideo,mjpeg,h264,hevc,png,wrapped_avframe \
  --enable-parser=h264,hevc,mjpeg,png \
  --enable-libfreetype --enable-libharfbuzz --pkg-config-flags=--static \
  --enable-ffnvcodec --enable-nvenc --enable-amf --enable-mediafoundation --enable-d3d11va --enable-dxva2 \
  --enable-encoder=h264_nvenc,hevc_nvenc,h264_amf,hevc_amf,h264_mf,hevc_mf,mjpeg,rawvideo \
  --enable-filter=buffer,buffersink,format,null,nullsink,scale,scale2ref,fps,setpts,split,select,separatefields,bwdif,overlay,colorchannelmixer,movie,testsrc2,drawtext
make -j"$(nproc)"
cp ffmpeg.exe "$OUT/ffmpeg-x86_64-pc-windows-msvc.exe"
strip "$OUT/ffmpeg-x86_64-pc-windows-msvc.exe"
{
  echo "FFmpeg $VERSION ($URL)"
  echo "sha256 $SHA256"
  ./ffmpeg.exe -hide_banner -version | sed -n 1,3p
} > "$ROOT/licenses/FFmpeg-BUILD_INFO-win64.txt"
echo "built $OUT/ffmpeg-x86_64-pc-windows-msvc.exe"
