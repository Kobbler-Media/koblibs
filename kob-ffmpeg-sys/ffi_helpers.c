// Accessor functions for opaque FFmpeg structs.
// Compiled into a static library and linked alongside the FFmpeg shared libs.
// These exist because bindgen cannot generate struct field access for types
// that are forward-declared before their full definition (FFmpeg 7.x pattern).

#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libavutil/frame.h>

// --- AVFormatContext accessors ---

unsigned int kff_fmt_nb_streams(const AVFormatContext *ctx) { return ctx->nb_streams; }
AVStream **kff_fmt_streams(const AVFormatContext *ctx) { return ctx->streams; }
const AVInputFormat *kff_fmt_iformat(const AVFormatContext *ctx) { return ctx->iformat; }
const AVOutputFormat *kff_fmt_oformat(const AVFormatContext *ctx) { return ctx->oformat; }
int64_t kff_fmt_duration(const AVFormatContext *ctx) { return ctx->duration; }
int64_t kff_fmt_bit_rate(const AVFormatContext *ctx) { return ctx->bit_rate; }
AVDictionary *kff_fmt_metadata(const AVFormatContext *ctx) { return ctx->metadata; }
unsigned int kff_fmt_nb_chapters(const AVFormatContext *ctx) { return ctx->nb_chapters; }
AVChapter **kff_fmt_chapters(const AVFormatContext *ctx) { return ctx->chapters; }
AVIOContext *kff_fmt_pb(const AVFormatContext *ctx) { return ctx->pb; }
void kff_fmt_set_pb(AVFormatContext *ctx, AVIOContext *pb) { ctx->pb = pb; }
int kff_fmt_flags(const AVFormatContext *ctx) { return ctx->flags; }
void kff_fmt_set_flags(AVFormatContext *ctx, int flags) { ctx->flags = flags; }
int64_t kff_fmt_start_time(const AVFormatContext *ctx) { return ctx->start_time; }
int64_t kff_fmt_probesize(const AVFormatContext *ctx) { return ctx->probesize; }
void kff_fmt_set_probesize(AVFormatContext *ctx, int64_t val) { ctx->probesize = val; }
int64_t kff_fmt_max_analyze_duration(const AVFormatContext *ctx) { return ctx->max_analyze_duration; }
void kff_fmt_set_max_analyze_duration(AVFormatContext *ctx, int64_t val) { ctx->max_analyze_duration = val; }

// --- AVStream accessors ---

int kff_stream_index(const AVStream *st) { return st->index; }
int kff_stream_id(const AVStream *st) { return st->id; }
AVCodecParameters *kff_stream_codecpar(const AVStream *st) { return st->codecpar; }
AVRational kff_stream_time_base(const AVStream *st) { return st->time_base; }
int64_t kff_stream_start_time(const AVStream *st) { return st->start_time; }
int64_t kff_stream_duration(const AVStream *st) { return st->duration; }
int64_t kff_stream_nb_frames(const AVStream *st) { return st->nb_frames; }
int kff_stream_disposition(const AVStream *st) { return st->disposition; }
AVDictionary *kff_stream_metadata(const AVStream *st) { return st->metadata; }
AVRational kff_stream_avg_frame_rate(const AVStream *st) { return st->avg_frame_rate; }
AVRational kff_stream_r_frame_rate(const AVStream *st) { return st->r_frame_rate; }
AVRational kff_stream_sample_aspect_ratio(const AVStream *st) { return st->sample_aspect_ratio; }

void kff_stream_set_time_base(AVStream *st, AVRational tb) { st->time_base = tb; }
// The mov/mp4 muxer writes `pasp` from AVStream.sample_aspect_ratio, not from
// codecpar, and init_muxer rejects the two disagreeing — so a non-square source
// has to be set on both.
void kff_stream_set_sample_aspect_ratio(AVStream *st, AVRational sar) { st->sample_aspect_ratio = sar; }
void kff_stream_set_discard(AVStream *st, enum AVDiscard discard) { st->discard = discard; }

// --- AVCodecContext accessors ---

enum AVCodecID kff_codec_ctx_codec_id(const AVCodecContext *ctx) { return ctx->codec_id; }
enum AVMediaType kff_codec_ctx_codec_type(const AVCodecContext *ctx) { return ctx->codec_type; }
int kff_codec_ctx_width(const AVCodecContext *ctx) { return ctx->width; }
int kff_codec_ctx_height(const AVCodecContext *ctx) { return ctx->height; }
enum AVPixelFormat kff_codec_ctx_pix_fmt(const AVCodecContext *ctx) { return ctx->pix_fmt; }
int kff_codec_ctx_sample_rate(const AVCodecContext *ctx) { return ctx->sample_rate; }
enum AVSampleFormat kff_codec_ctx_sample_fmt(const AVCodecContext *ctx) { return ctx->sample_fmt; }
int64_t kff_codec_ctx_bit_rate(const AVCodecContext *ctx) { return ctx->bit_rate; }
AVRational kff_codec_ctx_time_base(const AVCodecContext *ctx) { return ctx->time_base; }

// --- AVChapter accessors ---

int64_t kff_chapter_id(const AVChapter *ch) { return ch->id; }
AVRational kff_chapter_time_base(const AVChapter *ch) { return ch->time_base; }
int64_t kff_chapter_start(const AVChapter *ch) { return ch->start; }
int64_t kff_chapter_end(const AVChapter *ch) { return ch->end; }
AVDictionary *kff_chapter_metadata(const AVChapter *ch) { return ch->metadata; }

// Additional AVFormatContext accessors
AVIOContext **kff_fmt_pb_ptr(AVFormatContext *ctx) { return &ctx->pb; }
int kff_fmt_max_delay(const AVFormatContext *ctx) { return ctx->max_delay; }
void kff_fmt_set_max_delay(AVFormatContext *ctx, int val) { ctx->max_delay = val; }

// --- AVIOContext accessors ---

int kff_avio_error(const AVIOContext *ctx) { return ctx->error; }
int kff_avio_eof_reached(const AVIOContext *ctx) { return ctx->eof_reached; }
