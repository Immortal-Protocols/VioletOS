#include <string.h>
extern unsigned ticks(void), ticks_per_ms;
static volatile unsigned *const rng = (void *)0x09003000;
static volatile unsigned *const dma = (void *)0x09005000;
#define REG(o) rng[(o) / 4]
int radio_entropy(unsigned char seed[32]) {
  if (REG(0) != 0x74726976 || REG(4) != 2 || REG(8) != 4)
    return 0;
  REG(0x70) = 0;
  if (REG(0x70))
    return 0;
  REG(0x70) = 1;
  REG(0x70) = 3;
  REG(0x14) = 1;
  if (!(REG(0x10) & 1))
    return 0;
  REG(0x24) = 1;
  REG(0x20) = 1;
  REG(0x24) = 0;
  REG(0x20) = 0;
  REG(0x70) = 11;
  if (!(REG(0x70) & 8))
    return 0;
  REG(0x30) = 0;
  if (REG(0x34) < 2 || REG(0x44))
    return 0;
  memset((void *)dma, 0, 512);
  const unsigned phys = 0x49202000;
  dma[0] = phys + 256;
  dma[2] = 32;
  dma[3] = 2;
  dma[8] = 1 << 16;
  REG(0x38) = 2;
  REG(0x80) = phys;
  REG(0x84) = 0;
  REG(0x90) = phys + 32;
  REG(0x94) = 0;
  REG(0xa0) = phys + 48;
  REG(0xa4) = 0;
  __asm__ volatile("dmb sy" ::: "memory");
  REG(0x44) = 1;
  REG(0x70) = 15;
  REG(0x50) = 0;
  unsigned start = ticks();
  while (!(dma[12] >> 16)) {
    if (ticks() - start > ticks_per_ms * 1000)
      return 0;
  }
  __asm__ volatile("dmb sy" ::: "memory");
  if (dma[14] != 32)
    return 0;
  memcpy(seed, (const void *)(dma + 64), 32);
  memset((void *)(dma + 64), 0, 32);
  return 1;
}

unsigned radio_unix_time(void) { return *(volatile unsigned *)0x09004000; }
