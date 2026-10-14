#!/usr/bin/env bash
# Copy the TV libraries the app links against into ./sysroot (needs root SSH to the TV, see config.example.env).
# They are LG's proprietary libraries: never commit or redistribute them.
set -euo pipefail
. "$(dirname "$0")/env.sh"
cd "$ROOT"; mkdir -p sysroot
L="usr/lib/libSDL2-2.0.so.0 usr/lib/libSDL2-2.0.so.0.14.0 usr/lib/libEGL.so usr/lib/libEGL.so.1 usr/lib/libEGL.so.1.4.0 usr/lib/libGLESv2.so usr/lib/libGLESv2.so.2 usr/lib/libGLESv2.so.2.1.0 usr/lib/libplayerAPIs.so.1 usr/lib/libplayerAPIs.so.1.0.0 usr/lib/libavformat.so.59 usr/lib/libavformat.so.59.16.100 usr/lib/libavcodec.so.59 usr/lib/libavcodec.so.59.18.100 usr/lib/libavutil.so.57 usr/lib/libavutil.so.57.17.100 usr/lib/libswresample.so.4 usr/lib/libswresample.so.4.3.100 lib/libc.so.6 lib/libm.so.6 lib/libdl.so.2 lib/libpthread.so.0 lib/librt.so.1 lib/libgcc_s.so.1 lib/ld-linux.so.3"
tv "tar -C / -cf - $L" | tar -C sysroot -xf -
cd sysroot/usr/lib
ln -sfn libSDL2-2.0.so.0 libSDL2.so; ln -sfn libplayerAPIs.so.1 libplayerAPIs.so
ln -sfn libavformat.so.59 libavformat.so; ln -sfn libavcodec.so.59 libavcodec.so; ln -sfn libavutil.so.57 libavutil.so; ln -sfn libswresample.so.4 libswresample.so
echo "sysroot ready"
