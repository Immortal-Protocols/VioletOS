// FFmpeg owns the demuxer and the lifetime of each decoded frame. The two
// callbacks consume the pixels or PCM before the next call into the decoder.
#include <errno.h>
#include <stdint.h>
#include <string.h>
#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libavutil/imgutils.h>
#include <libswresample/swresample.h>
#include <libswscale/swscale.h>

struct input {
    const uint8_t *next;
    int remaining;
};

static int read_bytes(void *opaque, uint8_t *out, int size)
{
    struct input *input = opaque;
    int count = FFMIN(size, input->remaining);
    if (!count)
        return AVERROR_EOF;
    memcpy(out, input->next, count);
    input->next += count;
    input->remaining -= count;
    return count;
}

struct track {
    AVCodecContext *codec;
    AVStream *stream;
};

struct playback {
    struct track video, audio;
    AVFrame *frame, *bgra;
    struct SwsContext *scale;
    struct SwrContext *resample;
    uint8_t *pcm;
    unsigned pcm_capacity;
    void *opaque;
    int (*video_frame)(void *, int64_t, const uint8_t *, int, int, int);
    int (*audio_frame)(void *, int64_t, const uint8_t *, int);
};

static int open_track(struct track *track, AVStream *stream)
{
    const AVCodec *decoder = avcodec_find_decoder(stream->codecpar->codec_id);
    if (!decoder)
        return AVERROR_DECODER_NOT_FOUND;
    track->stream = stream;
    track->codec = avcodec_alloc_context3(decoder);
    if (!track->codec)
        return AVERROR(ENOMEM);
    int rc = avcodec_parameters_to_context(track->codec, stream->codecpar);
    if (rc < 0)
        return rc;
    track->codec->thread_count = 1;
    track->codec->max_pixels = 1920 * 1080;
    track->codec->pkt_timebase = stream->time_base;
    return avcodec_open2(track->codec, decoder, NULL);
}

static int output_video(struct playback *p, int64_t pts)
{
    const AVFrame *f = p->frame;
    // Keep the aspect ratio and fit in Violet's 720-pixel-wide viewport.
    if (f->width <= 0 || f->height <= 0)
        return AVERROR_INVALIDDATA;
    double scale = FFMIN(720.0 / f->width, 900.0 / f->height);
    int w = FFMAX(1, (int)(f->width * scale));
    int h = FFMAX(1, (int)(f->height * scale));
    if (p->bgra->width != w || p->bgra->height != h) {
        av_frame_unref(p->bgra);
        p->bgra->format = AV_PIX_FMT_BGRA;
        p->bgra->width = w;
        p->bgra->height = h;
        int rc = av_frame_get_buffer(p->bgra, 32);
        if (rc < 0)
            return rc;
    }
    p->scale = sws_getCachedContext(p->scale, f->width, f->height, f->format,
                                    w, h, AV_PIX_FMT_BGRA, SWS_FAST_BILINEAR, NULL, NULL, NULL);
    if (!p->scale)
        return AVERROR(ENOMEM);
    int rc = sws_scale(p->scale, (const uint8_t *const *)f->data, f->linesize,
                       0, f->height, p->bgra->data, p->bgra->linesize);
    if (rc < 0)
        return rc;
    return p->video_frame(p->opaque, pts, p->bgra->data[0], w, h, p->bgra->linesize[0]);
}

// A NULL frame drains the last resampled samples at end of stream.
static int output_audio(struct playback *p, const AVFrame *f, int64_t pts)
{
    if (!p->resample) {
        if (!f)
            return 0;
        if (f->sample_rate <= 0 || f->sample_rate > 384000)
            return AVERROR_INVALIDDATA;
        AVChannelLayout stereo = AV_CHANNEL_LAYOUT_STEREO;
        int rc = swr_alloc_set_opts2(&p->resample, &stereo, AV_SAMPLE_FMT_S16, 48000,
                                     &f->ch_layout, f->format, f->sample_rate, 0, NULL);
        if (rc < 0 || (rc = swr_init(p->resample)) < 0)
            return rc;
    }
    int input = f ? f->nb_samples : 0;
    int room = swr_get_out_samples(p->resample, input);
    if (room < 0 || room > 48000)
        return AVERROR_INVALIDDATA;
    if (!room)
        return 0;
    av_fast_malloc(&p->pcm, &p->pcm_capacity, (size_t)room * 4);
    if (!p->pcm)
        return AVERROR(ENOMEM);
    int count = swr_convert(p->resample, &p->pcm, room,
                            f ? (const uint8_t **)f->extended_data : NULL, input);
    if (count <= 0)
        return count;
    return p->audio_frame(p->opaque, pts, p->pcm, count * 4);
}

static int decode(struct playback *p, struct track *t, AVPacket *packet)
{
    int rc = avcodec_send_packet(t->codec, packet);
    if (rc < 0)
        return rc;
    while ((rc = avcodec_receive_frame(t->codec, p->frame)) >= 0) {
        int64_t ts = p->frame->best_effort_timestamp;
        int64_t ms = ts == AV_NOPTS_VALUE ? -1 : av_rescale_q(ts, t->stream->time_base, (AVRational){1,1000});
        rc = t == &p->video ? output_video(p, ms) : output_audio(p, p->frame, ms);
        av_frame_unref(p->frame);
        if (rc)
            return rc;
    }
    return rc == AVERROR_EOF || rc == AVERROR(EAGAIN) ? 0 : rc;
}

int violet_play_webm(const uint8_t *data, int length, void *opaque,
                     int (*video)(void *, int64_t, const uint8_t *, int, int, int),
                     int (*audio)(void *, int64_t, const uint8_t *, int), char error[128])
{
    struct input input = {data, length};
    struct playback p = {.opaque = opaque, .video_frame = video, .audio_frame = audio};
    AVFormatContext *fmt = avformat_alloc_context();
    uint8_t *buffer = av_malloc(16384);
    AVIOContext *io = NULL;
    AVPacket *packet = av_packet_alloc();
    int rc = AVERROR(ENOMEM);
    p.frame = av_frame_alloc();
    p.bgra = av_frame_alloc();
    if (!fmt || !buffer || !packet || !p.frame || !p.bgra)
        goto done;
    io = avio_alloc_context(buffer, 16384, 0, &input, read_bytes, NULL, NULL);
    if (!io)
        goto done;
    buffer = NULL; // owned by io, FFmpeg can replace it
    fmt->pb = io;
    fmt->max_streams = 16;
    rc = avformat_open_input(&fmt, NULL, av_find_input_format("matroska"), NULL);
    if (rc < 0)
        goto done;
    for (unsigned i = 0; i < fmt->nb_streams; ++i) {
        AVStream *s = fmt->streams[i];
        enum AVCodecID id = s->codecpar->codec_id;
        if ((id == AV_CODEC_ID_VP8 || id == AV_CODEC_ID_VP9) && !p.video.codec)
            rc = open_track(&p.video, s);
        if ((id == AV_CODEC_ID_OPUS || id == AV_CODEC_ID_VORBIS) && !p.audio.codec)
            rc = open_track(&p.audio, s);
        if (rc < 0)
            goto done;
    }
    if (!p.video.codec) {
        rc = AVERROR_DECODER_NOT_FOUND;
        goto done;
    }
    while ((rc = av_read_frame(fmt, packet)) >= 0) {
        struct track *t = NULL;
        if (packet->stream_index == p.video.stream->index)
            t = &p.video;
        else if (p.audio.stream && packet->stream_index == p.audio.stream->index)
            t = &p.audio;
        if (t)
            rc = decode(&p, t, packet);
        av_packet_unref(packet);
        if (rc)
            goto done;
    }
    if (rc == AVERROR_EOF) {
        rc = decode(&p, &p.video, NULL);
        if (!rc && p.audio.codec)
            rc = decode(&p, &p.audio, NULL);
        if (!rc)
            rc = output_audio(&p, NULL, -1);
    }
done:
    if (rc < 0)
        av_strerror(rc, error, 128);
    av_frame_free(&p.frame);
    av_frame_free(&p.bgra);
    sws_freeContext(p.scale);
    swr_free(&p.resample);
    av_free(p.pcm);
    avcodec_free_context(&p.video.codec);
    avcodec_free_context(&p.audio.codec);
    av_packet_free(&packet);
    avformat_close_input(&fmt);
    if (io)
        av_freep(&io->buffer);
    avio_context_free(&io);
    av_free(buffer);
    return rc;
}
