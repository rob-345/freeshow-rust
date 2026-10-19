/* GPL-3.0-only. Access public libltc layouts through the pinned public header.
 * No libltc private structure or Rust bitfield layout is assumed. */
#include <ltc.h>
#include <dlfcn.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>

_Static_assert(LIBLTC_VERSION_MAJOR == 1 && LIBLTC_VERSION_MINOR == 3 &&
                   LIBLTC_VERSION_MICRO == 1,
               "pinned header required");
_Static_assert(sizeof(ltcsnd_sample_t) == 1 && sizeof(ltc_off_t) == 8 &&
                   sizeof(LTCFrame) == 12,
               "unsupported public ABI");

#define API_FUNCTIONS(X) \
    X(ltc_encoder_create) X(ltc_encoder_free) X(ltc_encoder_set_timecode) \
    X(ltc_encoder_buffer_flush) X(ltc_encoder_encode_frame) \
    X(ltc_encoder_get_bufptr) X(ltc_encoder_get_buffersize) \
    X(ltc_decoder_create) X(ltc_decoder_free) X(ltc_decoder_write) \
    X(ltc_decoder_read) X(ltc_decoder_queue_length) X(ltc_frame_to_time)
#define FIELD(name) __typeof__(&name) name;
struct fs_ltc {
    void *library;
    API_FUNCTIONS(FIELD)
    LTCEncoder *encoder;
    LTCDecoder *decoder;
    ltc_off_t position;
};
#undef FIELD

/* Our bridge layout, not a reimplementation of a libltc structure. */
struct fs_frame {
    int32_t drop, days, months, years, hours, minutes, seconds, frames, reverse;
    int64_t offset_start;
    double volume;
    char timezone[6];
};
_Static_assert(sizeof(struct fs_frame) == 64, "bridge ABI");

void *fs_ltc_open(const char *path) {
    struct fs_ltc *a = calloc(1, sizeof(*a));
    if (!a) return NULL;
    a->library = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (!a->library) {
        free(a);
        return NULL;
    }
#define LOAD(name) \
    a->name = (__typeof__(&name))dlsym(a->library, #name); \
    if (!a->name) { \
        dlclose(a->library); \
        free(a); \
        return NULL; \
    }
    API_FUNCTIONS(LOAD)
#undef LOAD
    return a;
}

void fs_ltc_stop_encoder(void *p) {
    struct fs_ltc *a = p;
    if (a->encoder) {
        a->ltc_encoder_free(a->encoder);
        a->encoder = NULL;
    }
}

int fs_ltc_stop_decoder(void *p) {
    struct fs_ltc *a = p;
    int result = 0;
    if (a->decoder) {
        result = a->ltc_decoder_free(a->decoder);
        a->decoder = NULL;
    }
    a->position = 0;
    return result;
}

void fs_ltc_close(void *p) {
    if (!p) return;
    struct fs_ltc *a = p;
    fs_ltc_stop_encoder(a);
    fs_ltc_stop_decoder(a);
    dlclose(a->library);
    free(a);
}

int fs_ltc_encoder(void *p, double fps) {
    struct fs_ltc *a = p;
    LTCEncoder *encoder = a->ltc_encoder_create(
        48000, fps, fps == 25 ? LTC_TV_625_50 : LTC_TV_525_60, LTC_USE_DATE);
    if (!encoder) return -1;
    size_t length = a->ltc_encoder_get_buffersize(encoder);
    if (length > 4096) {
        a->ltc_encoder_free(encoder);
        return -2;
    }
    fs_ltc_stop_encoder(p);
    a->encoder = encoder;
    return 0;
}

int fs_ltc_decoder(void *p, int apv) {
    struct fs_ltc *a = p;
    LTCDecoder *decoder = a->ltc_decoder_create(apv, 32);
    if (!decoder) return -1;
    fs_ltc_stop_decoder(p);
    a->decoder = decoder;
    return 0;
}

int fs_ltc_encode(void *p, int hours, int minutes, int seconds,
                  unsigned char *out, size_t capacity) {
    struct fs_ltc *a = p;
    if (!a->encoder) return -1;
    SMPTETimecode time;
    memset(&time, 0, sizeof(time));
    memcpy(time.timezone, "+0200", 6);
    time.hours = hours;
    time.mins = minutes;
    time.secs = seconds;
    /* Wrapper adds singular frame:0; FreeShow's plural frames is ignored. */
    a->ltc_encoder_set_timecode(a->encoder, &time);
    a->ltc_encoder_buffer_flush(a->encoder);
    a->ltc_encoder_encode_frame(a->encoder);
    int length = 0;
    ltcsnd_sample_t *buffer = a->ltc_encoder_get_bufptr(a->encoder, &length, 0);
    if (!buffer || length < 0 || (size_t)length > capacity) return -2;
    memcpy(out, buffer, (size_t)length);
    return length;
}

int fs_ltc_decode(void *p, unsigned char *buffer, size_t size,
                  struct fs_frame *out, int *backlog) {
    struct fs_ltc *a = p;
    if (!a->decoder) return -1;
    a->ltc_decoder_write(a->decoder, buffer, size, a->position);
    a->position += (ltc_off_t)size;
    LTCFrameExt frame;
    int result = a->ltc_decoder_read(a->decoder, &frame);
    *backlog = a->ltc_decoder_queue_length(a->decoder);
    if (!result) return 0;
    SMPTETimecode time;
    memset(&time, 0, sizeof(time));
    a->ltc_frame_to_time(&time, &frame.ltc, LTC_USE_DATE);
    memset(out, 0, sizeof(*out));
    out->drop = frame.ltc.dfbit != 0;
    out->days = time.days;
    out->months = time.months;
    out->years = time.years;
    out->hours = time.hours;
    out->minutes = time.mins;
    out->seconds = time.secs;
    out->frames = time.frame;
    out->reverse = frame.reverse != 0;
    out->offset_start = frame.off_start;
    out->volume = frame.volume;
    memcpy(out->timezone, time.timezone, 5);
    return 1;
}
