// Host display and pointer bridge. All application state lives in the guest.
#include <SDL.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

enum { WIDTH = 720, HEIGHT = 1600, PITCH = WIDTH * 4 };
// Emulated sound card shared with the guest Audio service (targets/qemu/audio.rs).
enum { SOUND_PAGE = 0x49204000 - 0x40000000, SOUND_BYTES = 9 * 4096, SOUND_MAGIC = 0x31445541 };
static volatile uint32_t *sound;
static uint32_t heard;
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
    const size_t length = PITCH * HEIGHT;
    void *frame = mmap(NULL, length, PROT_READ, MAP_SHARED, fd, offset);
    sound = mmap(NULL, SOUND_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, SOUND_PAGE);
    close(fd);
    if (frame == MAP_FAILED || sound == MAP_FAILED) { perror("guest memory"); return 1; }
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
    SDL_Window *window = SDL_CreateWindow("VioletOS · seL4 on QEMU",
        SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED, WIDTH/2, HEIGHT/2,
        SDL_WINDOW_RESIZABLE | SDL_WINDOW_ALLOW_HIGHDPI);
    if (!window) goto fail;
    SDL_Renderer *renderer = SDL_CreateRenderer(window, -1, 0);
    if (!renderer || SDL_RenderSetLogicalSize(renderer, WIDTH, HEIGHT)) goto fail;
    SDL_Texture *texture = SDL_CreateTexture(renderer, SDL_PIXELFORMAT_ARGB8888,
        SDL_TEXTUREACCESS_STREAMING, WIDTH, HEIGHT);
    if (!texture) goto fail;
    title(window);
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
            }
            if (e.type == SDL_MOUSEBUTTONDOWN && e.button.button == SDL_BUTTON_LEFT &&
                e.button.x >= 0 && e.button.x < WIDTH && e.button.y >= 0 && e.button.y < HEIGHT) {
                if (!send_report(e.button.x, e.button.y, 1)) goto fail;
                held = 1;
            }
            if (held && ((e.type == SDL_MOUSEBUTTONUP && e.button.button == SDL_BUTTON_LEFT) ||
                (e.type == SDL_WINDOWEVENT && e.window.event == SDL_WINDOWEVENT_FOCUS_LOST))) {
                if (!send_report(0, 0, 0)) goto fail;
                held = 0;
            }
        }
        if (SDL_UpdateTexture(texture, NULL, frame, PITCH) ||
            SDL_RenderClear(renderer) || SDL_RenderCopy(renderer, texture, NULL, NULL)) goto fail;
        SDL_RenderPresent(renderer);
        SDL_Delay(32);
    }
    if (speaker) SDL_CloseAudioDevice(speaker);
    SDL_Quit();
    munmap(frame, length);
    return 0;
fail:
    fprintf(stderr, "VioletOS display: %s\n", SDL_GetError());
    SDL_Quit();
    munmap(frame, length);
    return 1;
}
