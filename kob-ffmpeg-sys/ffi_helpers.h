// Extern declarations for accessor functions compiled in ffi_helpers.c.
// These provide field access for opaque FFmpeg structs.

#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libavutil/frame.h>

// --- AVFormatContext ---
unsigned int kff_fmt_nb_streams(const AVFormatContext *ctx);
AVStream **kff_fmt_streams(const AVFormatContext *ctx);
const AVInputFormat *kff_fmt_iformat(const AVFormatContext *ctx);
const AVOutputFormat *kff_fmt_oformat(const AVFormatContext *ctx);
int64_t kff_fmt_duration(const AVFormatContext *ctx);
int64_t kff_fmt_bit_rate(const AVFormatContext *ctx);
AVDictionary *kff_fmt_metadata(const AVFormatContext *ctx);
unsigned int kff_fmt_nb_chapters(const AVFormatContext *ctx);
AVChapter **kff_fmt_chapters(const AVFormatContext *ctx);
AVIOContext *kff_fmt_pb(const AVFormatContext *ctx);
void kff_fmt_set_pb(AVFormatContext *ctx, AVIOContext *pb);
int kff_fmt_flags(const AVFormatContext *ctx);
void kff_fmt_set_flags(AVFormatContext *ctx, int flags);
int64_t kff_fmt_start_time(const AVFormatContext *ctx);
int64_t kff_fmt_probesize(const AVFormatContext *ctx);
void kff_fmt_set_probesize(AVFormatContext *ctx, int64_t val);
int64_t kff_fmt_max_analyze_duration(const AVFormatContext *ctx);
void kff_fmt_set_max_analyze_duration(AVFormatContext *ctx, int64_t val);

// --- AVStream ---
int kff_stream_index(const AVStream *st);
int kff_stream_id(const AVStream *st);
AVCodecParameters *kff_stream_codecpar(const AVStream *st);
AVRational kff_stream_time_base(const AVStream *st);
int64_t kff_stream_start_time(const AVStream *st);
int64_t kff_stream_duration(const AVStream *st);
int64_t kff_stream_nb_frames(const AVStream *st);
int kff_stream_disposition(const AVStream *st);
AVDictionary *kff_stream_metadata(const AVStream *st);
AVRational kff_stream_avg_frame_rate(const AVStream *st);
AVRational kff_stream_r_frame_rate(const AVStream *st);
AVRational kff_stream_sample_aspect_ratio(const AVStream *st);

void kff_stream_set_time_base(AVStream *st, AVRational tb);
void kff_stream_set_sample_aspect_ratio(AVStream *st, AVRational sar);
void kff_stream_set_discard(AVStream *st, enum AVDiscard discard);

// --- AVCodecContext ---
enum AVCodecID kff_codec_ctx_codec_id(const AVCodecContext *ctx);
enum AVMediaType kff_codec_ctx_codec_type(const AVCodecContext *ctx);
int kff_codec_ctx_width(const AVCodecContext *ctx);
int kff_codec_ctx_height(const AVCodecContext *ctx);
enum AVPixelFormat kff_codec_ctx_pix_fmt(const AVCodecContext *ctx);
int kff_codec_ctx_sample_rate(const AVCodecContext *ctx);
enum AVSampleFormat kff_codec_ctx_sample_fmt(const AVCodecContext *ctx);
int64_t kff_codec_ctx_bit_rate(const AVCodecContext *ctx);
AVRational kff_codec_ctx_time_base(const AVCodecContext *ctx);

// --- AVChapter ---
int64_t kff_chapter_id(const AVChapter *ch);
AVRational kff_chapter_time_base(const AVChapter *ch);
int64_t kff_chapter_start(const AVChapter *ch);
int64_t kff_chapter_end(const AVChapter *ch);
AVDictionary *kff_chapter_metadata(const AVChapter *ch);

// Additional AVFormatContext
AVIOContext **kff_fmt_pb_ptr(AVFormatContext *ctx);
int kff_fmt_max_delay(const AVFormatContext *ctx);
void kff_fmt_set_max_delay(AVFormatContext *ctx, int val);

// --- AVIOContext ---
int kff_avio_error(const AVIOContext *ctx);
int kff_avio_eof_reached(const AVIOContext *ctx);
