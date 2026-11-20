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
static const char alphabet[] = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ:-. /";
static const unsigned char glyph[][5] = {
    {62, 81, 73, 69, 62},  {0, 66, 127, 64, 0},    {98, 81, 73, 73, 70},
    {34, 65, 73, 73, 54},  {24, 20, 18, 127, 16},  {39, 69, 69, 69, 57},
    {60, 74, 73, 73, 48},  {1, 113, 9, 5, 3},      {54, 73, 73, 73, 54},
    {6, 73, 73, 41, 30},   {126, 17, 17, 17, 126}, {127, 73, 73, 73, 54},
    {62, 65, 65, 65, 34},  {127, 65, 65, 34, 28},  {127, 73, 73, 73, 65},
    {127, 9, 9, 9, 1},     {62, 65, 73, 73, 122},  {127, 8, 8, 8, 127},
    {0, 65, 127, 65, 0},   {32, 64, 65, 63, 1},    {127, 8, 20, 34, 65},
    {127, 64, 64, 64, 64}, {127, 2, 12, 2, 127},   {127, 4, 8, 16, 127},
    {62, 65, 65, 65, 62},  {127, 9, 9, 9, 6},      {62, 65, 81, 33, 94},
    {127, 9, 25, 41, 70},  {38, 73, 73, 73, 50},   {1, 1, 127, 1, 1},
    {63, 64, 64, 64, 63},  {31, 32, 64, 32, 31},   {127, 32, 24, 32, 127},
    {99, 20, 8, 20, 99},   {3, 4, 120, 4, 3},      {97, 81, 73, 69, 67},
    {0, 54, 54, 0, 0},     {8, 8, 8, 8, 8},        {0, 96, 96, 0, 0},
    {0, 0, 0, 0, 0},       {32, 16, 8, 4, 2}};
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
