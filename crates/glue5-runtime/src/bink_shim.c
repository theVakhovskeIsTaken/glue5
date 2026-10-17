// The engine's Bink movies are decoded with the host's FFmpeg: this shim wraps
// the pieces of libavformat/libavcodec the wasm build has no access to (its
// Bink SDK is stripped). It is compiled against the real headers, so union
// layouts stay honest instead of being duplicated by hand in Rust.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

#include "libavformat/avformat.h"
#include "libavcodec/avcodec.h"
#include "libavutil/pixdesc.h"
#include "libavutil/error.h"
#include "libavutil/mathematics.h"

static char g_err[256];

const char* bink_last_error(void) { return g_err; }

typedef struct BinkPlane {
    const uint8_t* data;
    int pitch;
} BinkPlane;

typedef struct BinkMovie {
    AVFormatContext* fmt;
    AVCodecContext* dec;
    AVPacket* pkt;
    AVFrame* frame;
    int stream;
    int w, h;
    int alpha;
    int fps_num, fps_den;
    int nb_frames;
    int draining;
} BinkMovie;

static void set_err(const char* what, int code) {
    if (code >= 0) {
        snprintf(g_err, sizeof g_err, "%s", what);
        return;
    }
    char text[AV_ERROR_MAX_STRING_SIZE];
    av_strerror(code, text, sizeof text);
    snprintf(g_err, sizeof g_err, "%s: %s", what, text);
}

void bink_info(const BinkMovie* m, int out[6]) {
    out[0] = m->w;
    out[1] = m->h;
    out[2] = m->fps_num;
    out[3] = m->fps_den;
    out[4] = m->nb_frames;
    out[5] = m->alpha;
}

int bink_open_movie(const char* path, BinkMovie** out) {
    *out = NULL;
    g_err[0] = 0;
    AVFormatContext* fmt = NULL;
    int r = avformat_open_input(&fmt, path, NULL, NULL);
    if (r < 0) { set_err("avformat_open_input", r); return -1; }
    r = avformat_find_stream_info(fmt, NULL);
    if (r < 0) { set_err("avformat_find_stream_info", r); avformat_close_input(&fmt); return -2; }
    const AVCodec* codec = NULL;
    int vs = av_find_best_stream(fmt, AVMEDIA_TYPE_VIDEO, -1, -1, &codec, 0);
    if (vs < 0) { set_err("no video stream", vs); avformat_close_input(&fmt); return -3; }
    AVCodecContext* dec = avcodec_alloc_context3(codec);
    if (!dec) { set_err("avcodec_alloc_context3", 0); avformat_close_input(&fmt); return -4; }
    r = avcodec_parameters_to_context(dec, fmt->streams[vs]->codecpar);
    if (r < 0) { set_err("avcodec_parameters_to_context", r); avcodec_free_context(&dec); avformat_close_input(&fmt); return -5; }
    r = avcodec_open2(dec, codec, NULL);
    if (r < 0) { set_err("avcodec_open2", r); avcodec_free_context(&dec); avformat_close_input(&fmt); return -6; }
    AVPacket* pkt = av_packet_alloc();
    AVFrame* frame = av_frame_alloc();
    if (!pkt || !frame) {
        set_err("out of memory", 0);
        av_frame_free(&frame);
        av_packet_free(&pkt);
        avcodec_free_context(&dec);
        avformat_close_input(&fmt);
        return -7;
    }
    // Decode the first frame before the game's textures exist: the demuxer's
    // stream parameters say nothing about alpha, the decoder's choice does.
    int got = 0;
    while (!got) {
        r = av_read_frame(fmt, pkt);
        if (r < 0) { set_err("no frame decoded", r); got = 0; break; }
        if (pkt->stream_index != vs) { av_packet_unref(pkt); continue; }
        r = avcodec_send_packet(dec, pkt);
        av_packet_unref(pkt);
        if (r < 0 && r != AVERROR(EAGAIN)) { set_err("avcodec_send_packet", r); break; }
        r = avcodec_receive_frame(dec, frame);
        if (r == 0) { got = 1; }
        else if (r != AVERROR(EAGAIN)) { set_err("decode", r); break; }
    }
    if (!got) {
        av_frame_free(&frame);
        av_packet_free(&pkt);
        avcodec_free_context(&dec);
        avformat_close_input(&fmt);
        return -8;
    }
    BinkMovie* m = calloc(1, sizeof(BinkMovie));
    if (!m) { set_err("out of memory", 0); /* same cleanup */ av_frame_free(&frame); av_packet_free(&pkt); avcodec_free_context(&dec); avformat_close_input(&fmt); return -9; }
    m->fmt = fmt;
    m->dec = dec;
    m->pkt = pkt;
    m->frame = frame;
    m->stream = vs;
    m->w = frame->width > 0 ? frame->width : dec->width;
    m->h = frame->height > 0 ? frame->height : dec->height;
    const AVPixFmtDescriptor* desc = av_pix_fmt_desc_get(frame->format);
    m->alpha = desc && (desc->flags & AV_PIX_FMT_FLAG_ALPHA) ? 1 : 0;
    AVStream* st = fmt->streams[vs];
    AVRational fr = st->avg_frame_rate.num > 0 ? st->avg_frame_rate : st->r_frame_rate;
    if (fr.num <= 0) fr = dec->framerate.num > 0 ? dec->framerate : (AVRational){30, 1};
    m->fps_num = fr.num;
    m->fps_den = fr.den;
    m->nb_frames = st->nb_frames > 0 ? (int)(st->nb_frames & 0x7fffffff) : 0;
    if (m->nb_frames <= 0 && fmt->duration > 0 && fr.num > 0)
        m->nb_frames = (int)av_rescale(fmt->duration, fr.num, (int64_t)fr.den * AV_TIME_BASE);
    if (m->nb_frames < 1) m->nb_frames = 1;
    // Playback starts from the packet after the probe's: the first frame's
    // packets were already fed to the decoder (starting a movie at frame 1
    // instead of 0 loses nothing in practice: frame 0 is whatever the fade
    // starts from).
    *out = m;
    return 0;
}

/// Decodes the next full frame. `planes` are Bink's order (Y, cR, cB, A):
/// FFmpeg's is Y(0), U/Cb(1), V/Cr(2), A(3). Returns 1 with a frame, 0 at the
/// end, negative on error.
int bink_decode_frame(BinkMovie* m, BinkPlane planes[4]) {
    planes[0].data = NULL; planes[0].pitch = 0;
    planes[1].data = NULL; planes[1].pitch = 0;
    planes[2].data = NULL; planes[2].pitch = 0;
    planes[3].data = NULL; planes[3].pitch = 0;
    int r;
    for (;;) {
        r = avcodec_receive_frame(m->dec, m->frame);
        if (r == 0) break;
        if (r == AVERROR_EOF) return 0;
        if (r != AVERROR(EAGAIN)) { set_err("decode", r); return -1; }
        r = av_read_frame(m->fmt, m->pkt);
        if (r < 0) {
            if (!m->draining) { avcodec_send_packet(m->dec, NULL); m->draining = 1; }
            continue;
        }
        if (m->pkt->stream_index == m->stream) {
            r = avcodec_send_packet(m->dec, m->pkt);
            if (r < 0 && r != AVERROR(EAGAIN)) { set_err("send", r); return -2; }
        }
        av_packet_unref(m->pkt);
    }
    // cR is the red difference (FFmpeg's V plane), cB the blue one (U). Alpha
    // sits in the fourth plane whenever the pixel format has one.
    planes[0].data = m->frame->data[0]; planes[0].pitch = m->frame->linesize[0];
    planes[1].data = m->frame->data[2]; planes[1].pitch = m->frame->linesize[2];
    planes[2].data = m->frame->data[1]; planes[2].pitch = m->frame->linesize[1];
    const AVPixFmtDescriptor* desc = av_pix_fmt_desc_get(m->frame->format);
    if (desc && (desc->flags & AV_PIX_FMT_FLAG_ALPHA) && m->frame->data[3]) {
        planes[3].data = m->frame->data[3];
        planes[3].pitch = m->frame->linesize[3];
    }
    return 1;
}

void bink_close_movie(BinkMovie* m) {
    if (!m) return;
    av_frame_free(&m->frame);
    av_packet_free(&m->pkt);
    avcodec_free_context(&m->dec);
    avformat_close_input(&m->fmt);
    free(m);
}

// A diagnostic: decodes up to `count` frames from a fresh open, sampling the
// Y plane's centre rows every 25 frames, and writes a compact report.
int bink_probe(const char* path, int count, char* out, int outsz) {
    out[0] = 0;
    BinkMovie* m = NULL;
    if (bink_open_movie(path, &m) < 0) {
        snprintf(out, outsz, "open failed: %s", bink_last_error());
        return 0;
    }
    // TEMP DIAGNOSTIC: the decoded frame's layout, from the already-decoded
    // first frame.
    {
        const AVPixFmtDescriptor* d = av_pix_fmt_desc_get(m->frame->format);
        const char* name = (d && d->name) ? d->name : "unknown";
        snprintf(out, outsz, "fmt=%s ls=%d,%d,%d,%d null=%d,%d,%d,%d |", name,
            m->frame->linesize[0], m->frame->linesize[1], m->frame->linesize[2], m->frame->linesize[3],
            m->frame->data[0] == NULL, m->frame->data[1] == NULL, m->frame->data[2] == NULL, m->frame->data[3] == NULL);
    }
    BinkPlane pl[4];
    int frames = 0;
    for (;;) {
        int r = bink_decode_frame(m, pl);
        if (r <= 0) {
            strncat(out, " eof", outsz - strlen(out) - 1);
            break;
        }
        frames++;
        if (frames % 25 == 0 && pl[0].data && pl[0].pitch > 0) {
            const uint8_t* y = pl[0].data + (m->h / 2) * pl[0].pitch;
            unsigned long long s = 0;
            int nz = 0;
            for (int i = 0; i < 240; i++) {
                s += y[i];
                nz += y[i] > 0;
            }
            int uav = -1, vav = -1;
            if (pl[1].data && pl[1].pitch > 0) {
                const uint8_t* u = pl[1].data + (m->h / 4) * pl[1].pitch;
                unsigned long long su = 0;
                for (int i = 0; i < 120; i++) su += u[i];
                uav = (int)(su / 120);
            }
            if (pl[2].data && pl[2].pitch > 0) {
                const uint8_t* v = pl[2].data + (m->h / 4) * pl[2].pitch;
                unsigned long long sv = 0;
                for (int i = 0; i < 120; i++) sv += v[i];
                vav = (int)(sv / 120);
            }
            char tmp[64];
            snprintf(tmp, sizeof tmp, " f%d y=%d nz=%d u=%d v=%d", frames, (int)(s / 240), nz, uav, vav);
            strncat(out, tmp, outsz - strlen(out) - 1);
        }
        if (frames >= count) break;
    }
    bink_close_movie(m);
    return frames;
}
