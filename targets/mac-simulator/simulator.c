#include "screen.h"
#include <SDL.h>
#include <stdio.h>
int main(int argc, char **argv) {
  (void)argc;
  (void)argv;
  if (SDL_Init(SDL_INIT_VIDEO))
    goto fail;
  SDL_Window *w =
      SDL_CreateWindow("VioletOS", SDL_WINDOWPOS_CENTERED,
                       SDL_WINDOWPOS_CENTERED, 360, 600, SDL_WINDOW_RESIZABLE);
  if (!w)
    goto fail;
  SDL_Renderer *r = SDL_CreateRenderer(w, -1, 0);
  if (!r || SDL_RenderSetLogicalSize(r, WIDTH, HEIGHT))
    goto fail;
  SDL_Texture *t = SDL_CreateTexture(
      r, SDL_PIXELFORMAT_RGB888, SDL_TEXTUREACCESS_STREAMING, WIDTH, HEIGHT);
  if (!t)
    goto fail;
  for (;;) {
    SDL_Event e;
    while (SDL_PollEvent(&e)) {
      if (e.type == SDL_QUIT ||
          (e.type == SDL_KEYDOWN && e.key.keysym.sym == SDLK_ESCAPE)) {
        SDL_Quit();
        return 0;
      }
      if (e.type == SDL_MOUSEBUTTONUP && e.button.button == SDL_BUTTON_LEFT &&
          screen_press(e.button.x, e.button.y) == SCREEN_POWER_OFF) {
        SDL_Quit();
        return 0;
      }
    }
    screen_draw("SIMULATOR");
    SDL_UpdateTexture(t, NULL, pixels, WIDTH * 4);
    SDL_RenderClear(r);
    SDL_RenderCopy(r, t, NULL, NULL);
    SDL_RenderPresent(r);
    SDL_Delay(75);
  }
fail:
  fprintf(stderr, "%s\n", SDL_GetError());
  SDL_Quit();
  return 1;
}
