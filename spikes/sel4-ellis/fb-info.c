#include <fcntl.h>
#include <linux/fb.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <unistd.h>

int main(void) {
    int fd = open("/dev/fb0", O_RDONLY);
    if (fd < 0) fd = open("/dev/graphics/fb0", O_RDONLY);
    if (fd < 0) { perror("framebuffer"); return 1; }
    struct fb_fix_screeninfo f;
    struct fb_var_screeninfo v;
    if (ioctl(fd, FBIOGET_FSCREENINFO, &f) || ioctl(fd, FBIOGET_VSCREENINFO, &v)) {
        perror("framebuffer query"); close(fd); return 1;
    }
    printf("FB phys=0x%lx len=0x%x stride=%u x=%u y=%u xoff=%u yoff=%u bpp=%u\n",
           f.smem_start, f.smem_len, f.line_length, v.xres, v.yres, v.xoffset, v.yoffset, v.bits_per_pixel);
    close(fd);
    return 0;
}
