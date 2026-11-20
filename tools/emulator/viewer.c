// Host display and pointer bridge. All application state lives in the guest.
#include <SDL.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

enum { WIDTH = 720, HEIGHT = 1600, PITCH = WIDTH * 4 };
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
    int fd = open(argv[1], O_RDONLY);
    if (fd < 0) { perror("guest RAM"); return 1; }
    // Align to host page size (16 KiB on Apple Silicon).
    off_t offset = 0x79450000 - 0x40000000;
    const size_t length = PITCH * HEIGHT;
    void *frame = mmap(NULL, length, PROT_READ, MAP_SHARED, fd, offset);
    close(fd);
    if (frame == MAP_FAILED) { perror("guest framebuffer"); return 1; }
    if (SDL_Init(SDL_INIT_VIDEO)) goto fail;
    SDL_Window *window = SDL_CreateWindow("VioletOS · seL4 on QEMU",
        SDL_WINDOWPOS_CENTERED, SDL_WINDOWPOS_CENTERED, WIDTH/2, HEIGHT/2,
        SDL_WINDOW_RESIZABLE | SDL_WINDOW_ALLOW_HIGHDPI);
    if (!window) goto fail;
    SDL_Renderer *renderer = SDL_CreateRenderer(window, -1, 0);
    if (!renderer || SDL_RenderSetLogicalSize(renderer, WIDTH, HEIGHT)) goto fail;
    SDL_Texture *texture = SDL_CreateTexture(renderer, SDL_PIXELFORMAT_ARGB8888,
        SDL_TEXTUREACCESS_STREAMING, WIDTH, HEIGHT);
    if (!texture) goto fail;
    int running = 1, held = 0;
    while (running) {
        SDL_Event e;
        while (SDL_PollEvent(&e)) {
            if (e.type == SDL_QUIT || (e.type == SDL_KEYDOWN && e.key.keysym.sym == SDLK_ESCAPE))
                running = 0;
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
    SDL_Quit();
    munmap(frame, length);
    return 0;
fail:
    fprintf(stderr, "VioletOS display: %s\n", SDL_GetError());
    SDL_Quit();
    munmap(frame, length);
    return 1;
}
