#define _POSIX_C_SOURCE 200809L
#include "screen.h"
#include <stdio.h>
#include <string.h>
#include <time.h>

uint32_t pixels[HEIGHT][WIDTH];
static int page, running;
static double accumulated, started;
static const uint32_t ink = 0xeeeaf8, muted = 0xc1b8cf, accent = 0xd1b0fe,
                      bg = 0x191620, panel = 0x302a39;
#include "glyphs.h"
static double now(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return t.tv_sec + t.tv_nsec / 1e9;
}
static void rect(int x, int y, int w, int h, uint32_t color) {
  for (int j = y; j < y + h && j < HEIGHT; j++)
    for (int i = x; i < x + w && i < WIDTH; i++)
      pixels[j][i] = color;
}
static void label(int x, int y, const char *s, int scale, uint32_t color) {
  for (; *s; s++, x += 6 * scale) {
    const char *g = strchr(alphabet, *s);
    if (!g)
      continue;
    for (int i = 0; i < 5; i++)
      for (int j = 0; j < 7; j++)
        if (glyph[g - alphabet][i] & (1 << j))
          rect(x + i * scale, y + j * scale, scale, scale, color);
  }
}
static void button(int y, const char *s) {
  rect(14, y, 212, 50, panel);
  label(26, y + 18, s, 2, ink);
}
void screen_touch_status(const char *status) {
  label(14, 32, status, 1, muted);
}
void screen_draw(const char *platform) {
  char value[48];
  time_t wall = time(NULL);
  rect(0, 0, WIDTH, HEIGHT, bg);
  label(14, 20, "VIOLET OS", 1, accent);
  rect(14, 42, 212, 1, panel);
  if (page == 0) {
    strftime(value, sizeof value, "%H:%M", localtime(&wall));
    label(14, 76, value, 6, ink);
    strftime(value, sizeof value, "%Y-%m-%d", localtime(&wall));
    label(15, 134, value, 2, muted);
    button(206, "STOPWATCH");
    button(266, "SYSTEM");
    button(335, "POWER OFF");
  } else if (page == 1) {
    label(14, 68, "STOPWATCH", 3, ink);
    double elapsed = accumulated + (running ? now() - started : 0);
    snprintf(value, sizeof value, "%02lu:%02lu.%u", (unsigned long)elapsed / 60,
             (unsigned long)elapsed % 60, (unsigned)(elapsed * 10) % 10);
    label(14, 139, value, 4, accent);
    button(206, running ? "PAUSE" : "START");
    button(266, "RESET");
    button(335, "HOME");
  } else if (page == 2) {
    label(14, 68, "SYSTEM", 3, ink);
    label(14, 138, "RUNTIME", 1, muted);
    label(14, 158, platform, 2, ink);
    label(14, 206, "INTERFACE", 1, muted);
    label(14, 226, "VIOLET 0.1", 2, ink);
    button(335, "HOME");
  } else {
    label(14, 68, "POWER OFF", 3, ink);
    button(206, "SHUT DOWN");
    button(266, "CANCEL");
  }
}
enum screen_action screen_press(int x, int y) {
  if (x < 14 || x >= 226)
    return SCREEN_CONTINUE;
  if (page == 3) {
    if (y >= 206 && y < 256)
      return SCREEN_POWER_OFF;
    if (y >= 266 && y < 316)
      page = 0;
    return SCREEN_CONTINUE;
  }
  if (y >= 335 && y < 385) {
    page = page == 0 ? 3 : 0;
    return SCREEN_CONTINUE;
  }
  if (y >= 206 && y < 256) {
    if (!page)
      page = 1;
    else if (page == 1) {
      if (running)
        accumulated += now() - started;
      else
        started = now();
      running = !running;
    }
  } else if (y >= 266 && y < 316) {
    if (!page)
      page = 2;
    else if (page == 1) {
      running = 0;
      accumulated = 0;
    }
  }
  return SCREEN_CONTINUE;
}
