// Shared MT6765 programmed-I/O transfer path.
#include "spi.h"
static unsigned last_timeout_state, last_timeout_command;
unsigned spi_timeout_command(void) { return last_timeout_command; }
unsigned spi_timeout_state(void) { return last_timeout_state; }
static void configure_controller(void) {
    spi_write(0x18,4);
    (void)spi_read(0x18);
    spi_write(0x18,0);
    spi_write(0x24,0);
    spi_write(0,0x00070007);
    spi_write(0x28,0x00030003);
}
bool bus_start(void) {
    if (!spi_clock_start()) return false;
    configure_controller();
    return true;
}
bool exchange(uint8_t *data,unsigned len) {
    if(!len || len>32) return false;
    (void)spi_read(0x1c);
    spi_write(4,((len-1)<<16)|7);
    for(unsigned pos=0;pos<len;pos+=4) {
        unsigned word=0;
        for(unsigned j=0;j<4 && pos+j<len;j++) word|=(unsigned)data[pos+j]<<(8*j);
        spi_write(0x10,word);
    }
    spi_write(0x18,(1u<<16)|(3u<<12)|1); // standalone transfer, finish interrupt
    uint32_t start=ticks();
    // Poll STATUS1 before reading the completion latch (MediaTek polled SPI sequence).
    while (!(spi_read(0x20) & 1u) || !(spi_read(0x1c) & 1u)) {
        if (ticks()-start > ticks_per_ms*10) {
            if ((spi_read(0x20) & 1u) && (spi_read(0x1c) & 1u)) break;
            last_timeout_state=spi_read(0x20);
            last_timeout_command=spi_read(0x18);
            configure_controller();
            return false;
        }
    }
    for(unsigned pos=0;pos<len;pos+=4) {
        unsigned word=spi_read(0x14);
        for(unsigned j=0;j<4 && pos+j<len;j++) data[pos+j]=word>>(8*j);
    }
    return true;
}
