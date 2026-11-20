#include "screen.h"
#include <errno.h>
#include <fcntl.h>
#include <linux/fb.h>
#include <linux/input.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/reboot.h>
#include <sys/mman.h>
#include <unistd.h>

struct pointer {
  struct input_absinfo horizontal, vertical;
  int x_axis, y_axis, x, y, down, release;
  int novatek;
  unsigned reports;
};
static int open_pointer(struct pointer *p, int n) {
  char path[64];
  {
    snprintf(path, sizeof path, "/dev/input/event%d", n);
    int fd = open(path, O_RDONLY | O_NONBLOCK);
    if (fd < 0)
      return -1;
    const int axes[][2] = {{ABS_MT_POSITION_X, ABS_MT_POSITION_Y},
                           {ABS_X, ABS_Y}};
    for (unsigned i = 0; i < 2; i++) {
      p->x_axis = axes[i][0];
      p->y_axis = axes[i][1];
      if (ioctl(fd, EVIOCGABS(p->x_axis), &p->horizontal) == 0 &&
          ioctl(fd, EVIOCGABS(p->y_axis), &p->vertical) == 0 &&
          p->horizontal.maximum > p->horizontal.minimum &&
          p->vertical.maximum > p->vertical.minimum) {
        char name[128] = {0};
        ioctl(fd, EVIOCGNAME(sizeof name - 1), name);
        p->novatek = strcmp(name, "NVTCapacitiveTouchScreen") == 0;
        return fd;
      }
    }
    close(fd);
  }
  return -1;
}
static int coordinate(int value, struct input_absinfo range, int extent) {
  return (long long)(value - range.minimum) * (extent - 1) /
         (range.maximum - range.minimum);
}
static void input(struct pointer *p, struct input_event *e) {
  if (e->type == EV_SYN && e->code == SYN_REPORT)
    p->reports++;
  if (e->type == EV_ABS && e->code == p->x_axis)
    p->x = e->value;
  if (e->type == EV_ABS && e->code == p->y_axis)
    p->y = e->value;
  if ((e->type == EV_KEY && (e->code == BTN_LEFT || e->code == BTN_TOUCH)) ||
      (e->type == EV_ABS && e->code == ABS_MT_TRACKING_ID)) {
    int active = e->type == EV_KEY ? e->value != 0 : e->value >= 0;
    if (!active && p->down)
      p->release = 1;
    p->down = active;
  }
  if (e->type == EV_SYN && e->code == SYN_REPORT && p->release) {
    enum screen_action action = screen_press(
        coordinate(p->x, p->horizontal, WIDTH),
        coordinate(p->y, p->vertical, HEIGHT));
    if (action == SCREEN_POWER_OFF) {
      fprintf(stderr, "VioletOS: powering off\n");
      sync();
      if (reboot(RB_POWER_OFF) < 0)
        perror("power off");
    }
    p->release = 0;
  }
}
static uint32_t channel(unsigned value, struct fb_bitfield field) {
  return field.length ? (value >> (8 - field.length)) << field.offset : 0;
}
int main(int argc, char **argv) {
  int pan_display = argc >= 2 && strcmp(argv[1], "--pan-display") == 0;
  int touch_status = argc >= 3 && strcmp(argv[2], "--touch-status") == 0;
  int fb = open("/dev/fb0", O_RDWR);
  if (fb < 0)
    fb = open("/dev/graphics/fb0", O_RDWR);
  struct fb_fix_screeninfo f;
  struct fb_var_screeninfo v;
  if (fb < 0 || ioctl(fb, FBIOGET_FSCREENINFO, &f) ||
      ioctl(fb, FBIOGET_VSCREENINFO, &v)) {
    perror("framebuffer");
    return 1;
  }
  unsigned bytes = v.bits_per_pixel / 8;
  if ((bytes != 2 && bytes != 4) || v.red.length > 8 || v.green.length > 8 ||
      v.blue.length > 8 || v.transp.length > 8 || !v.xres || !v.yres ||
      ((uint64_t)v.xoffset + v.xres) * bytes > f.line_length ||
      ((uint64_t)v.yoffset + v.yres) * f.line_length > f.smem_len) {
    fprintf(stderr, "Unsupported framebuffer layout\n");
    return 1;
  }
  unsigned char *base =
      mmap(NULL, f.smem_len, PROT_READ | PROT_WRITE, MAP_SHARED, fb, 0);
  if (base == MAP_FAILED) {
    perror("mmap");
    return 1;
  }
  struct pointer pointers[64] = {0};
  int devices[64];
  for (int i = 0; i < 64; i++)
    devices[i] = -1;
  static uint32_t previous[HEIGHT][WIDTH];
  fprintf(stderr, "VioletOS display %ux%u\n", v.xres, v.yres);
  for (unsigned tick = 0;; tick++) {
    for (int i = 0; i < 64; i++) {
      if (devices[i] < 0 && tick % 20 == 0) {
        devices[i] = open_pointer(&pointers[i], i);
        if (devices[i] >= 0)
          fprintf(stderr, "VioletOS pointer ready on event%d\n", i);
      }
      if (devices[i] < 0)
        continue;
      struct input_event e;
      while (read(devices[i], &e, sizeof e) == sizeof e)
        input(&pointers[i], &e);
      if (errno == ENODEV) {
        close(devices[i]);
        devices[i] = -1;
      }
    }
    screen_draw("LINUX");
    if (touch_status) {
      char status[48] = "NO NOVATEK INPUT";
      for (int i = 0; i < 64; i++) {
        if (devices[i] >= 0 && pointers[i].novatek) {
          snprintf(status, sizeof status, "NVT EVENT%d REPORTS %u", i,
                   pointers[i].reports);
          break;
        }
      }
      FILE *errors = fopen("/tmp/touch-module-errors", "r");
      if (errors) {
        char module[64];
        if (fgets(module, sizeof module, errors))
          snprintf(status, sizeof status, "%s DRIVER LOAD FAILED",
                   strncmp(module, "mtk", 3) == 0 ? "MTK" : "NVT");
        fclose(errors);
      }
      screen_touch_status(status);
    }
    if (!memcmp(previous, pixels, sizeof pixels)) {
      usleep(100000);
      continue;
    }
    memcpy(previous, pixels, sizeof pixels);
    for (unsigned y = 0; y < v.yres; y++) {
      unsigned char *row =
          base + (y + v.yoffset) * f.line_length + v.xoffset * bytes;
      for (unsigned x = 0; x < v.xres; x++) {
        uint32_t rgb = pixels[y * HEIGHT / v.yres][x * WIDTH / v.xres];
        uint32_t color = channel(rgb >> 16, v.red) |
                         channel((rgb >> 8) & 255, v.green) |
                         channel(rgb & 255, v.blue) | channel(255, v.transp);
        memcpy(row + x * bytes, &color, bytes);
      }
    }
    // Some handset fbdev drivers submit the frame on a pan request, even
    // when the visible page stays at the current offset.
    if (pan_display && ioctl(fb, FBIOPAN_DISPLAY, &v) < 0) {
      if (errno == ENOTTY || errno == EINVAL)
        pan_display = 0;
      perror("FBIOPAN_DISPLAY");
    }
    usleep(100000);
  }
}
