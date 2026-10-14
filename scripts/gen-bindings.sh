#!/bin/sh
# Regenerate src/sys/ff.rs for the TV's FFmpeg 5.0 (avformat 59.16 / avcodec 59.18), 32-bit ARM layouts.
set -e
cd "$(dirname "$0")/.."
if [ ! -d third_party/ffmpeg ]; then
  mkdir -p third_party/ffmpeg && curl -sSfL https://ffmpeg.org/releases/ffmpeg-5.0.1.tar.xz | tar xJ -C third_party/ffmpeg --strip-components=1 --wildcards '*/libavformat/*.h' '*/libavcodec/*.h' '*/libavutil/*.h' '*/libswresample/*.h'
fi
# avconfig.h is made by FFmpeg's configure, not shipped in the tarball: the TV is 32-bit little-endian ARM
printf '#ifndef AVUTIL_AVCONFIG_H\n#define AVUTIL_AVCONFIG_H\n#define AV_HAVE_BIGENDIAN 0\n#define AV_HAVE_FAST_UNALIGNED 1\n#endif\n' > third_party/ffmpeg/libavutil/avconfig.h
Z=$HOME/.local/share/zig/current/lib/libc/include
bindgen third_party/ffwrap.h -o src/sys/ff.rs \
  --allowlist-function '(avformat|avio|av_read_frame|av_seek_frame|av_find_best_stream|av_bsf|av_packet|av_malloc|av_free|av_freep|av_dict|av_strerror|avcodec_parameters|av_log_set_level|avformat_version|avcodec_find_decoder|avcodec_alloc_context3|avcodec_open2|avcodec_decode_subtitle2|avsubtitle_free|avcodec_free_context|avcodec_find_encoder|avcodec_send_|avcodec_receive_|av_frame_|swr_|av_get_default_channel_layout|av_get_channel_layout_nb_channels|av_get_bytes_per_sample|av_sample_fmt_is_planar)\w*' \
  --allowlist-type 'AV(FormatContext|Stream|Packet|CodecParameters|IOContext|BSFContext|Rational|Dictionary|DictionaryEntry|MediaType|CodecID|PacketSideData|Subtitle|SubtitleRect|SubtitleType|CodecContext|Frame|SampleFormat|Codec)\w*' \
  --allowlist-var '(AV(SEEK|FMT|ERROR|_PKT|_NOPTS|_TIME|IO|_CH)|SWR)\w*' --no-layout-tests --no-doc-comments --use-core -- \
  --target=armv7-unknown-linux-gnueabi -Ithird_party/ffmpeg -isystem $Z/arm-linux-gnueabi -isystem $Z/generic-glibc -isystem $Z/any-linux-any
