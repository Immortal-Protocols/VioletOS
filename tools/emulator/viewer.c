// Host display and pointer bridge. All application state lives in the guest.
#include <SDL.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>
#ifdef __APPLE__
#import <AppKit/AppKit.h>
#endif

// Match the Moto G Pure framebuffer and keep square pixels on the host.
enum { WIDTH = 720, HEIGHT = 1600, PITCH = WIDTH * 4 };
// Emulated sound card shared with the guest Audio service (targets/qemu/audio.rs).
// Control page, eight-page PCM ring, then a two-page 16 kHz mono microphone ring.
enum { SOUND_PAGE = 0x49204000 - 0x40000000, SOUND_BYTES = 11 * 4096, SOUND_MAGIC = 0x31445541,
       MIC_OFFSET = 9 * 4096, MIC_SAMPLES = 4096 };
static volatile uint32_t *sound;
// The Power fixture's page (targets/qemu/power.rs): word 2 is set while the
// power key is held and word 3 counts its presses.
enum { POWER_MAP = 0x49000000 - 0x40000000, POWER_WORD = 0x2000 / 4 };
static volatile uint32_t *power;
static uint32_t heard;
// Captured samples go into the guest's ring; word 13 counts them.
static void capture_microphone(void *unused, Uint8 *stream, int bytes) {
    (void)unused;
    volatile int16_t *ring = (volatile int16_t *)((volatile uint8_t *)sound + MIC_OFFSET);
    const int16_t *in = (const int16_t *)stream;
    uint32_t written = sound[13];
    for (int i = 0; i < bytes / 2; i++) ring[written++ % MIC_SAMPLES] = in[i];
    __atomic_thread_fence(__ATOMIC_RELEASE);
    sound[13] = written;
}
// Without a capture device (or without permission) the guest hears silence.
static void silence(uint32_t samples) {
    volatile int16_t *ring = (volatile int16_t *)((volatile uint8_t *)sound + MIC_OFFSET);
    uint32_t written = sound[13];
    while (samples--) ring[written++ % MIC_SAMPLES] = 0;
    sound[13] = written;
}
static void play(void *unused, Uint8 *stream, int bytes) {
    (void)unused;
    int16_t *out = (int16_t *)stream;
    int frames = bytes / 4;
    const int16_t *ring = (const int16_t *)(sound + 1024);
    uint32_t size = sound[7], written = sound[5];
    if (sound[2] != SOUND_MAGIC || !size || size > 8192) size = 0;
    // Rejoin the guest after a stall rather than replaying stale samples.
    if (size && written - heard > size) heard = written - size / 4;
    for (int i = 0; i < frames; i++) {
        if (size && heard != written) {
            uint32_t at = heard++ % size;
            out[2 * i] = ring[2 * at];
            out[2 * i + 1] = ring[2 * at + 1];
        } else {
            out[2 * i] = out[2 * i + 1] = 0;
        }
    }
}
static void title(SDL_Window *window) {
    SDL_SetWindowTitle(window, sound[1] & 1 ? "VioletOS · seL4 on QEMU · headphones in"
                                            : "VioletOS · seL4 on QEMU");
}
static int send_report(int x, int y, int down) {
    uint8_t report[6] = {0};
    if (down) {
        report[0] = 9;  // first contact, pressed
        report[1] = x >> 4;
        report[2] = y >> 4;
        report[3] = ((x & 15) << 4) | (y & 15);
        report[4] = report[5] = 1;
    }
    return write(STDOUT_FILENO, report, sizeof report) == sizeof report;
}

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    int fd = open(argv[1], O_RDWR);
    if (fd < 0) { perror("guest RAM"); return 1; }
    // Align to host page size (16 KiB on Apple Silicon).
    off_t offset = 0x79450000 - 0x40000000;
    // The frame, then the page after it, where the guest says which rows changed.
    const size_t length = PITCH * HEIGHT + 4096;
    void *frame = mmap(NULL, length, PROT_READ | PROT_WRITE, MAP_SHARED, fd, offset);
    sound = mmap(NULL, SOUND_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, SOUND_PAGE);
    void *low = mmap(NULL, 0x4000, PROT_READ | PROT_WRITE, MAP_SHARED, fd, POWER_MAP);
    close(fd);
    if (frame == MAP_FAILED || sound == MAP_FAILED || low == MAP_FAILED) { perror("guest memory"); return 1; }
    power = (volatile uint32_t *)low + POWER_WORD;
    power[2] = 0;
    sound[0] = 0;
    heard = sound[5];
    if (SDL_Init(SDL_INIT_VIDEO)) goto fail;
    // Sound is optional: the guest keeps running on a Mac without an output device.
    SDL_AudioDeviceID speaker = 0;
    if (!SDL_InitSubSystem(SDL_INIT_AUDIO)) {
        SDL_AudioSpec want = {.freq = 48000, .format = AUDIO_S16SYS, .channels = 2,
                              .samples = 512, .callback = play};
        speaker = SDL_OpenAudioDevice(NULL, 0, &want, NULL, 0);
        if (speaker) SDL_PauseAudioDevice(speaker, 0);
    }
    if (!speaker) fprintf(stderr, "VioletOS display: no sound output (%s)\n", SDL_GetError());
    // The Mac's microphone opens only while the guest asks for it (word 12), so
    // macOS asks for permission the first time an app records.
    SDL_AudioDeviceID mic = 0;
    int mic_tried = 0;
    Uint32 mic_clock = 0;
    SDL_Window *window = SDL_CreateWindow("VioletOS · seL4 on QEMU",
        SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED, WIDTH / 2, HEIGHT / 2,
        SDL_WINDOW_RESIZABLE | SDL_WINDOW_ALLOW_HIGHDPI);
    if (!window) goto fail;
#ifdef __APPLE__
    // The launcher starts this executable directly to receive touch events.
    // Set the Dock icon after SDL has initialized the macOS application.
    @autoreleasepool {
        NSString *name = [NSBundle.mainBundle objectForInfoDictionaryKey:@"CFBundleIconFile"];
        NSURL *url = name ? [NSBundle.mainBundle URLForResource:name withExtension:nil] : nil;
        NSImage *icon = url ? [[NSImage alloc] initWithContentsOfURL:url] : nil;
        if (icon) NSApp.applicationIconImage = icon;
        [icon release];
    }
#endif
    // Presents follow the Mac's display refresh rather than a fixed delay.
    SDL_Renderer *renderer = SDL_CreateRenderer(window, -1, SDL_RENDERER_PRESENTVSYNC);
    if (!renderer) renderer = SDL_CreateRenderer(window, -1, 0);
    if (!renderer || SDL_RenderSetLogicalSize(renderer, WIDTH, HEIGHT)) goto fail;
    SDL_Texture *texture = SDL_CreateTexture(renderer, SDL_PIXELFORMAT_ARGB8888,
        SDL_TEXTUREACCESS_STREAMING, WIDTH, HEIGHT);
    if (!texture || SDL_SetTextureScaleMode(texture, SDL_ScaleModeLinear)) goto fail;
    title(window);
    // Dirty rows from the guest (runtime/open/scanout.rs): magic, first row,
    // end row, a sequence number, and the sequence this viewer has copied.
    volatile uint32_t *dirty = (volatile uint32_t *)((uint8_t *)frame + PITCH * HEIGHT);
    Uint32 full_at = 0;
    int running = 1, held = 0;
    while (running) {
        SDL_Event e;
        while (SDL_PollEvent(&e)) {
            if (e.type == SDL_QUIT || (e.type == SDL_KEYDOWN && e.key.keysym.sym == SDLK_ESCAPE))
                running = 0;
            // Up/Down (or +/-) hold the volume buttons. H plugs or unplugs headphones.
            if ((e.type == SDL_KEYDOWN || e.type == SDL_KEYUP) && !e.key.repeat) {
                SDL_Keycode k = e.key.keysym.sym;
                uint32_t bit = (k == SDLK_UP || k == SDLK_EQUALS || k == SDLK_PLUS) ? 1
                             : (k == SDLK_DOWN || k == SDLK_MINUS) ? 2 : 0;
                if (bit) sound[0] = e.type == SDL_KEYDOWN ? sound[0] | bit : sound[0] & ~bit;
                if (k == SDLK_h && e.type == SDL_KEYDOWN) { sound[1] ^= 1; title(window); }
                // P is the power button: tap to turn the screen off or on, hold for the power sheet.
                if (k == SDLK_p) {
                    power[2] = e.type == SDL_KEYDOWN;
                    if (e.type == SDL_KEYDOWN) power[3]++;
                }
            }
            if (e.type == SDL_MOUSEBUTTONDOWN && e.button.button == SDL_BUTTON_LEFT &&
                e.button.x >= 0 && e.button.x < WIDTH && e.button.y >= 0 && e.button.y < HEIGHT) {
                // SDL maps the mouse into guest coordinates, including letterboxing.
                if (!send_report(e.button.x, e.button.y, 1)) goto fail;
                held = 1;
            }
            if (held && ((e.type == SDL_MOUSEBUTTONUP && e.button.button == SDL_BUTTON_LEFT) ||
                (e.type == SDL_WINDOWEVENT && e.window.event == SDL_WINDOWEVENT_FOCUS_LOST))) {
                if (!send_report(0, 0, 0)) goto fail;
                held = 0;
            }
        }
        int wanted = sound[2] == SOUND_MAGIC && (sound[12] & 1);
        if (wanted && !mic && !mic_tried) {
            SDL_AudioSpec want = {.freq = 16000, .format = AUDIO_S16SYS, .channels = 1,
                                  .samples = 256, .callback = capture_microphone};
            mic = SDL_OpenAudioDevice(NULL, 1, &want, NULL, 0);
            mic_tried = 1;
            if (mic) SDL_PauseAudioDevice(mic, 0);
            else fprintf(stderr, "VioletOS display: no microphone (%s)\n", SDL_GetError());
            mic_clock = SDL_GetTicks();
        }
        if (!wanted && (mic || mic_tried)) {
            if (mic) SDL_CloseAudioDevice(mic);
            mic = 0;
            mic_tried = 0;
        }
        if (wanted && !mic) {
            Uint32 now = SDL_GetTicks();
            silence((now - mic_clock) * 16);
            mic_clock = now;
        }
        // Upload only the rows the guest changed; every quarter second the
        // whole frame too, for writers that do not report (the classic launcher).
        Uint32 now = SDL_GetTicks();
        int changed = 0;
        if (now - full_at >= 250) {
            full_at = now;
            if (SDL_UpdateTexture(texture, NULL, frame, PITCH)) goto fail;
            changed = 1;
        } else if (dirty[0] == 0x54524944 && dirty[3] != dirty[4]) {
            uint32_t seq = dirty[3];
            __atomic_thread_fence(__ATOMIC_ACQUIRE);
            uint32_t y0 = dirty[1], y1 = dirty[2];
            dirty[4] = seq;
            if (y0 < y1 && y1 <= HEIGHT) {
                SDL_Rect rows = {0, (int)y0, WIDTH, (int)(y1 - y0)};
                if (SDL_UpdateTexture(texture, &rows, (uint8_t *)frame + y0 * PITCH, PITCH)) goto fail;
                changed = 1;
            }
        }
        if (changed) {
            if (SDL_RenderClear(renderer) || SDL_RenderCopy(renderer, texture, NULL, NULL)) goto fail;
            SDL_RenderPresent(renderer); // waits for the Mac's VSync
        } else {
            SDL_Delay(2);
        }
    }
    if (speaker) SDL_CloseAudioDevice(speaker);
    if (mic) SDL_CloseAudioDevice(mic);
    SDL_Quit();
    munmap(frame, length);
    return 0;
fail:
    fprintf(stderr, "VioletOS display: %s\n", SDL_GetError());
    SDL_Quit();
    munmap(frame, length);
    return 1;
}
