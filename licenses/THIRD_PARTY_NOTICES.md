# Third-party software in Xspector Record

## FFmpeg (bundled as a separate program)

Xspector Record ships an unmodified build of **FFmpeg 9.0.2** (https://ffmpeg.org) as a separate
executable (`ffmpeg`, next to the application). The application starts it as a child process; it is not
linked into the application.

* License: **GNU Lesser General Public License v2.1 or later** — text in `FFmpeg-COPYING.LGPLv2.1.txt`.
* Configured with `--disable-gpl --disable-nonfree --disable-version3 --disable-autodetect`, so no GPL
  or non-free component (for example x264, x265, yadif) is included. The exact configure line and
  version information are in `FFmpeg-BUILD_INFO-*.txt`.
* Source code: https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz (SHA-256
  `8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e`). The build script that produced
  the binary is `scripts/build-ffmpeg-macos.sh` in the application's source repository.
* You may replace the bundled `ffmpeg` with your own build: put it next to the application, or set the
  `FFMPEG` environment variable to its path.

Only the video encoders provided by the operating system / graphics hardware are used (Apple
VideoToolbox; NVIDIA NVENC, Intel QuickSync, AMD AMF on Windows).

## FreeType and HarfBuzz (inside the bundled FFmpeg, for overlay text)

* **FreeType 2.15.0** — FreeType License (FTL); text in `FreeType-FTL.txt`.
  Portions of this software are copyright © The FreeType Project (www.freetype.org). All rights reserved.
* **HarfBuzz 15.0.0** — MIT License; text in `HarfBuzz-COPYING.txt`.

Both are compiled into the `ffmpeg` executable as static libraries.

## Other

Rust crates, React and the other npm packages used by the application are listed in `Cargo.lock` and
`package-lock.json` together with their licenses.
