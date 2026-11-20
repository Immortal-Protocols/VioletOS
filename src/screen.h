#pragma once
#include <stdint.h>
enum { WIDTH = 240, HEIGHT = 400 };
extern uint32_t pixels[HEIGHT][WIDTH];
void screen_draw(const char *platform);
void screen_touch_status(const char *status);
enum screen_action { SCREEN_CONTINUE, SCREEN_POWER_OFF };
enum screen_action screen_press(int x, int y);
