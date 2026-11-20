// MT6765 SPI3, chip select 0. Use programmed I/O with bounded transfers.
#include "spi.h"
#define REG(base,offset) ((base)[(offset)/4])
#define top ((volatile uint32_t *)0x09000000)
#define infra ((volatile uint32_t *)0x09001000)
#define pad ((volatile uint32_t *)0x09004000)
#define gpio ((volatile uint32_t *)0x09002000)
#define spi ((volatile uint32_t *)0x09003000)
void touch_map(void) {
    map_device(0x10000000,0x09000000,12);
    map_device(0x10001000,0x09001000,12);
    map_device(0x10002000,0x09004000,12);
    map_device(0x10005000,0x09002000,12);
    map_device(0x10007000,0x09005000,12);
    map_device(0x11013000,0x09003000,12);
}
bool spi_clock_start(void) {
    // Use the 26 MHz oscillator for SPI and ungate only SPI3.
    REG(top,0x68)=(3u<<24)|(1u<<31);
    REG(top,0x04)=1u<<11;
    REG(infra,0xa8)=1u<<10;
    __asm__ volatile("dsb sy" ::: "memory");
    if((REG(infra,0xac)&(1u<<10)) || (REG(top,0x60)&0x83000000u)) return false;
    for(unsigned pin=21;pin<=24;pin++) {
        unsigned index=(0x300+16*(pin/8))/4,shift=(pin%8)*4;
        gpio[index]=(gpio[index]&~(15u<<shift))|(1u<<shift);
    }
    // IO_CFG_LB input enables for SPI3 pads 21 through 24.
    REG(pad,0x424)=15u<<20;
    __asm__ volatile("dsb sy" ::: "memory");
    return true;
}
unsigned spi_read(unsigned offset) { return REG(spi,offset); }
void spi_write(unsigned offset,unsigned value) { REG(spi,offset)=value; }
