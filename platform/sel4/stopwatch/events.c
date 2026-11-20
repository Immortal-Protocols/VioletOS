#include <stddef.h>
#include "tasks.h"

enum { STATUS, SAMPLE, STATUS_BYTES = 40 };
static seL4_MessageInfo_t message(unsigned words) {
    return seL4_MessageInfo_new(0,0,0,words);
}
void touch_task(void) {
    clock_init();
    watchdog_attach((void *)0x09005000);
    char error[STATUS_BYTES];
    touch_initialize(error);
    for (;;) {
        seL4_Recv(TOUCH_EP, NULL);
        watchdog_progress();
        if (seL4_GetMR(0) == STATUS) {
            unsigned i = 0;
            for (; error[i] && i < STATUS_BYTES-1; ++i) seL4_SetMR(i,error[i]);
            seL4_SetMR(i,0);
            seL4_Reply(message(i+1));
        } else {
            unsigned x=0,y=0; bool down=false;
            bool valid = !error[0] && touch_sample(&x,&y,&down);
            seL4_SetMR(0, valid | ((unsigned)down << 1));
            seL4_SetMR(1,x);
            seL4_SetMR(2,y);
            seL4_Reply(message(3));
        }
    }
}
const char *touch_status(void) {
    static char status[STATUS_BYTES];
    seL4_SetMR(0,STATUS);
    unsigned length=seL4_MessageInfo_get_length(seL4_Call(TOUCH_EP,message(1)));
    if (!length || length>sizeof(status)) return "TOUCH IPC ERROR";
    for (unsigned i=0;i<length;i++) status[i]=seL4_GetMR(i);
    status[length-1]=0;
    return status[0]?status:NULL;
}
bool touch_event(unsigned *x,unsigned *y,bool *down) {
    seL4_SetMR(0,SAMPLE);
    if (seL4_MessageInfo_get_length(seL4_Call(TOUCH_EP,message(1)))!=3) return false;
    unsigned flags=seL4_GetMR(0);
    *down=(flags&2)!=0; *x=seL4_GetMR(1); *y=seL4_GetMR(2);
    return flags&1;
}
