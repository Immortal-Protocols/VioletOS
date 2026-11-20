// QEMU register-level fixture. Power, pinmux and electrical SPI timing are external.
#include "spi.h"
#include "firmware.h"
extern const uint8_t touch_firmware[];
static unsigned page_base, uploaded[sizeof(parts)/sizeof(*parts)], crc_fields;
static bool ready, identified;
enum { NORMAL, DELAY, DROP_BEFORE, DROP_AFTER, STUCK, SETUP, RESET1, RESET2 };
enum { MODE=4, INJECTIONS, RECOVERIES, READBACKS, VIOLATIONS, POLLS };
static unsigned cmd,cfg0,cfg1,cfg2,pad,tx_words,rx_pos,rx_len,start,latency;
static bool active, suppress;
static uint8_t fifo[32];

#define uart ((volatile uint32_t *)0x09003000)
#define test ((volatile uint32_t *)0x09005000)
void touch_map(void) {
    map_device(0x09000000,0x09003000,12);
    map_device(0x49000000,0x09005000,12);
    
}
bool spi_clock_start(void) { return true; }

static void contact(uint8_t *data) {
    static uint8_t current[6], pending[6];
    static unsigned count;
    while (!(uart[6]&16)) {
        pending[count++]=uart[0];
        if (count==6) {
            for (unsigned j=0;j<6;j++) current[j]=pending[j];
            count=0;
            break; // Deliver down and up on separate guest polls, even for a fast click.
        }
    }
    for (unsigned j=0;j<6;j++) data[j]=current[j];
}

// Verify writes against the factory image and track the contiguous programmed
// prefix. Reading an unwritten byte gives a deliberately different test value.
static bool chip(unsigned len) {
    if (fifo[0]==255 && len==3) {
        page_base=(fifo[1]<<15)|(fifo[2]<<7);
        return true;
    }
    unsigned addr=page_base+(fifo[0]&127);
    bool writing=(fifo[0]&128)!=0;
    for (unsigned i=0;i<sizeof(parts)/sizeof(*parts);i++) {
        if (addr<parts[i].addr || addr-parts[i].addr>=parts[i].size) continue;
        unsigned off=addr-parts[i].addr, head=writing?1:2;
        if (len<head || len-head>parts[i].size-off || (writing && off>uploaded[i])) return false;
        if (!writing) test[READBACKS]++;
        for (unsigned j=head;j<len;j++,off++) {
            uint8_t expected=touch_firmware[parts[i].offset+off];
            if (writing && fifo[j]!=expected) return false;
            if (!writing) fifo[j]=off<uploaded[i]?expected:(uint8_t)~expected;
        }
        if (writing && off>uploaded[i]) uploaded[i]=off;
        return true;
    }
    if (writing) {
        const unsigned fields[]={0x3f128,0x3f118,0x3f100,0x3f12c,0x3f130,0x3f104};
        const unsigned widths[]={3,2,4};
        for(unsigned i=0;i<6;i++) {
            if(addr!=fields[i]) continue;
            if(i!=crc_fields || len!=widths[i%3]+1) return false;
            unsigned bank=i/3;
            const unsigned values[]={parts[bank].addr,parts[bank].size-1,parts[bank].crc};
            for(unsigned j=1;j<len;j++)
                if(fifo[j]!=(uint8_t)(values[i%3]>>(8*(j-1)))) return false;
            crc_fields++;
        }
        if(addr==0x3f10d) {
            for(unsigned i=0;i<sizeof(parts)/sizeof(*parts);i++)
                if(uploaded[i]!=parts[i].size) return false;
            ready=crc_fields==6;
        }
    } else if(addr==0x3f004 && len==8) {
        identified=true; fifo[2]=11; fifo[5]=0x25; fifo[6]=0x65; fifo[7]=3;
    } else if(addr==0x21c60 && ready) fifo[2]=0xa1;
    else if(addr==0x21c00 && ready) contact(fifo+2);
    return true;
}
static void activate(void) {
    unsigned len=(cfg1>>16)+1;
    if(active || rx_pos<rx_len || cmd!=0x13001 || cfg0!=0x70007 ||
       cfg2!=0x30003 || pad || (cfg1&65535)!=7 || len>32 || tx_words!=(len+3)/4) {
        test[VIOLATIONS]++;
        return;
    }
    active=true; suppress=false; latency=0; start=ticks();
    tx_words=0; rx_pos=0; rx_len=len;
    unsigned addr=page_base+(fifo[0]&127);
    bool target=(fifo[0]&128) && addr==128 && fifo[0]!=255;
    unsigned mode=test[MODE];
    if (((mode==RESET1 && !identified) || (mode==RESET2 && identified)) &&
        len==3 && fifo[0]==255 && fifo[1]==(0x3f0fe>>15) && fifo[2]==((0x3f0fe>>7)&255)) {
        test[INJECTIONS]++;
        suppress=true;
        return;
    }
    if (mode==SETUP && (fifo[0]&128) &&
        ((addr==0x3f0fe && test[INJECTIONS]==0) || (addr==0x3f100 && test[INJECTIONS]==1))) {
        test[INJECTIONS]++;
        suppress=true;
        return;
    }
    if(target && mode!=SETUP && mode!=NORMAL && (mode==STUCK || test[INJECTIONS]<(mode==DROP_BEFORE?2u:1u))) {
        test[INJECTIONS]++;
        if (mode==DELAY) latency=3*ticks_per_ms;
        else suppress=true;
        if(mode==DROP_BEFORE || mode==STUCK) return;
    }
    if(!chip(len)) { test[VIOLATIONS]++; suppress=true; }
}
void spi_write(unsigned offset,unsigned value) {
    switch(offset) {
    case 0: cfg0=value; break;
    case 4: cfg1=value; break;
    case 0x24: pad=value; break;
    case 0x28: cfg2=value; break;
    case 0x10:
        if (tx_words>=8 || active) { test[VIOLATIONS]++; break; }
        for(unsigned j=0;j<4;j++) fifo[4*tx_words+j]=value>>(8*j);
        tx_words++;
        break;
    case 0x18:
        cmd=value;
        if(value&4) {
            if(active) test[RECOVERIES]++;
            active=false; tx_words=rx_len=rx_pos=0;
            cfg0=cfg2=0;
        } else if(value&1) { activate(); cmd &= ~1u; }
        break;
    default: test[VIOLATIONS]++;
    }
}
unsigned spi_read(unsigned offset) {
    if(offset==0x18) return cmd;
    if(offset==0x20) {
        if(active && ticks()-start<latency) { test[POLLS]++; return 0; }
        return 1;
    }
    if(offset==0x1c) {
        if(!active || suppress) return 0;
        if(ticks()-start<latency) { test[VIOLATIONS]++; return 0; }
        active=false;
        return 1;
    }
    if(offset==0x14 && !active && rx_pos<rx_len) {
        unsigned word=0;
        for(unsigned j=0;j<4 && rx_pos<rx_len;j++,rx_pos++) word|=(unsigned)fifo[rx_pos]<<(8*j);
        return word;
    }
    test[VIOLATIONS]++;
    return 0;
}
