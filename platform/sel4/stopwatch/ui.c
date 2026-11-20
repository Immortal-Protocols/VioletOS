#include "platform.h"
#include "../../../src/glyphs.h"
// Bootloader scanout pitch inferred from the physical flash 3 photograph.
enum { scanout_width = 720, scanout_height = 1600 };
static volatile uint32_t *const pixels=(void *)0x08000000;
static const uint32_t bg=0xff191620, panel=0xff302a39, ink=0xffeeeaf8, accent=0xffd1b0fe;
uint32_t ticks_per_ms;
uint32_t ticks(void) {
    uint32_t lo,hi;
    __asm__ volatile("isb; mrrc p15,1,%0,%1,c14":"=r"(lo),"=r"(hi));
    return lo;
}
void delay_ms(unsigned ms) {
    uint32_t start=ticks();
    while(ticks()-start<ms*ticks_per_ms) {}
}
static void rect(unsigned x,unsigned y,unsigned w,unsigned h,uint32_t color) {
    for(unsigned row=y*4;row<(y+h)*4;row++)
        for(unsigned col=x*3;col<(x+w)*3;col++) pixels[row*scanout_width+col]=color;
}
static void label(unsigned x,unsigned y,const char *s,unsigned scale,uint32_t color) {
    for(;*s;s++,x+=6*scale) {
        unsigned k=0;
        while(alphabet[k] && alphabet[k]!=*s) k++;
        if(!alphabet[k]) continue;
        for(unsigned i=0;i<5;i++) for(unsigned j=0;j<7;j++)
            if(glyph[k][i]&(1u<<j)) rect(x+i*scale,y+j*scale,scale,scale,color);
    }
}
static void button(unsigned y,const char *s) { rect(14,y,212,50,panel); label(26,y+18,s,2,ink); }
static void digits(unsigned tenths) {
    unsigned sec=tenths/10, min=(sec/60)%100;
    char s[]="00:00.0";
    s[0]+=(min/10); s[1]+=min%10; s[3]+=(sec%60)/10; s[4]+=sec%10; s[6]+=tenths%10;
    rect(14,139,170,29,bg); label(14,139,s,4,accent);
}
static void draw(bool watch,bool running,unsigned tenth,const char *status) {
    rect(0,0,240,400,bg);
    label(14,20,"VIOLET OS",1,accent); label(14,32,status,1,ink);
    if(watch) {
        label(14,68,"STOPWATCH",3,ink); digits(tenth);
        button(206,running?"PAUSE":"START"); button(266,"RESET"); button(335,"HOME");
    } else {
        label(14,76,"VIOLET",5,ink); button(206,"STOPWATCH");
    }
}
void stopwatch(void) {
    uint32_t freq;
    __asm__ volatile("mrc p15,0,%0,c14,c0,0":"=r"(freq));
    ticks_per_ms=freq/1000;
    // Reserve the low-address device mappings before allocating the display.
    extern void touch_map(void);
    touch_map();
    for(unsigned offset=0;offset<sizeof(*pixels)*scanout_width*scanout_height;offset+=65536)
        map_device(0x79450000+offset,0x08000000+offset,16);
    draw(false,false,0,"STARTING TOUCH");
    const char *status=touch_init();
    bool ready=status==0,watch=false,running=false,held=false;
    if(ready) status="TOUCH READY";
    draw(watch,running,0,status);
    unsigned tenths=0, leftover=0, previous=ticks();
    for(;;) {
        watchdog_progress();
        unsigned now=ticks(), delta=now-previous;
        previous=now;
        if(running) {
            leftover+=delta;
            unsigned interval=ticks_per_ms*100;
            if(leftover>=interval) {
                tenths+=leftover/interval; leftover%=interval;
                if(watch) digits(tenths);
            }
        }
        unsigned x=0,y=0; bool down=false;
        if(ready && touch_sample(&x,&y,&down)) {
            if(down && !held && x>=42 && x<678) {
                if(y>=824 && y<1024) {
                    if(watch) running=!running; else watch=true;
                } else if(watch && y>=1064 && y<1264) {
                    running=false; tenths=0; leftover=0;
                } else if(watch && y>=1340 && y<1540) watch=false;
                draw(watch,running,tenths,status);
            }
            held=down;
        }
        delay_ms(8);
    }
}
