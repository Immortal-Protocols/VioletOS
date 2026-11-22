// MP3 decoding for the music stream, using minimp3 (CC0, pinned in
// kernel/sel4/build.py). Built with hardware floating point; the seL4 kernel
// saves the FPU state per thread.
#define MINIMP3_IMPLEMENTATION
#define MINIMP3_ONLY_MP3
#define MINIMP3_NO_SIMD
#include "minimp3.h"

static mp3dec_t decoder;

void music_mp3_reset(void) { mp3dec_init(&decoder); }

// Decode one frame from `data`. Returns the stereo frames written to `pcm`
// (up to 1152, interleaved) and stores the bytes consumed in `*used`. A zero
// return with a nonzero `*used` skipped junk; zero with zero needs more data.
int music_mp3_decode(const unsigned char *data, int size, short *pcm, int *used,
                     int *hz, int *channels) {
  mp3dec_frame_info_t info;
  int frames = mp3dec_decode_frame(&decoder, data, size, pcm, &info);
  *used = info.frame_bytes;
  *hz = info.hz;
  *channels = info.channels;
  if (frames && info.channels == 1)
    for (int i = frames - 1; i >= 0; i--)
      pcm[2 * i] = pcm[2 * i + 1] = pcm[i];
  return frames;
}
