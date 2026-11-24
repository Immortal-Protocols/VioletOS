#include <stddef.h>
#include "tasks.h"
#include "spi.h"
#include "mcs.h"

enum { STATUS, SAMPLE, STATUS_BYTES = 40 };
static seL4_MessageInfo_t message(unsigned words) {
    return seL4_MessageInfo_new(0,0,0,words);
}
/* Interrupt-driven touch (docs/8ms-frame-pipeline-rfc.md). On the handset
   the supervisor binds the controller's interrupt to this thread, maps a
   ring shared read-only with the Open Runtime, and gives a capability that
   signals the Open Runtime: each report is read once, as it arrives, and
   the Open Runtime wakes for it. Runtime's polled samples still work. */
enum { IRQ_SLOT = 9, OPEN_SLOT = 10, TCB_SLOT = 11, EVENT_PRIORITY = 150, IRQ_BADGE = 1u << 20, RING_CAP = 64,
       RING_GIVEN = 0x31515249 /* "IRQ1" */, RING_LIVE = 0x4556494c /* "LIVE" */ };
#define ring ((volatile unsigned *)0x09007000)
static void push(unsigned x, unsigned y, bool down) {
    /* Word 0: the supervisor's mark, 1: LIVE, 2: samples written; then
       4 words a sample: x, y, down, the counter's low word when read. */
    unsigned n = ring[2], *e = (unsigned *)&ring[4 + 4 * (n % RING_CAP)];
    e[0] = x; e[1] = y; e[2] = down; e[3] = ticks();
    __sync_synchronize();
    ring[2] = n + 1;
}
void touch_task(void) {
    clock_init();
    watchdog_attach((void *)0x09005000);
    char error[STATUS_BYTES];
    touch_initialize(error);
    bool irq = false, was_down = false;
    unsigned idle_irqs = 0, window_irqs = 0, window_start = ticks();
    unsigned last_x = ~0u, last_y = ~0u;
#ifdef VIOLET_MOTO
    irq = !error[0] && ring[0] == RING_GIVEN && touch_irq_start() && !seL4_IRQHandler_Ack(IRQ_SLOT);
    if (irq) {
        ring[2] = 0;
        __sync_synchronize();
        ring[1] = RING_LIVE;
        seL4_TCB_SetPriority(TCB_SLOT, TCB_SLOT, EVENT_PRIORITY);
    }
#endif
    for (;;) {
        seL4_Word badge = 0;
        IPC_RECV(TOUCH_EP, &badge, REPLY_SLOT);
        watchdog_progress();
        if (irq && (badge & IRQ_BADGE)) {
            bool pushed = false;
            /* Word 3 counts interrupts. One that fires 2,000 times without a
               report is stuck: stop it, and the Open Runtime polls again. */
            ring[3] = ring[3] + 1;
            /* Also stuck: more than 1,000 in a second (the controller reports
               at most a few hundred times a second). */
            if (ticks() - window_start > ticks_per_ms * 1000) { window_start = ticks(); window_irqs = 0; }
            bool mine = touch_irq_take();
            if (!mine || ++window_irqs > 1000) {
                if (++idle_irqs > 2000 || window_irqs > 1000) {
                    touch_irq_stop();
                    ring[1] = 0;
                    irq = false;
                }
                seL4_IRQHandler_Ack(IRQ_SLOT);
                continue;
            }
            idle_irqs = 0;
            for (unsigned round = 0; round < 4 && (round == 0 || touch_irq_take()); round++) {
                unsigned x = 0, y = 0; bool down = false;
                if (!touch_sample(&x, &y, &down)) continue;
                /* Only changes: a press, a finger that moved, a release. */
                if (down == was_down && (!down || (x == last_x && y == last_y))) continue;
                push(x, y, down);
                was_down = down; last_x = x; last_y = y; pushed = true;
            }
            seL4_IRQHandler_Ack(IRQ_SLOT);
            if (pushed) seL4_Signal(OPEN_SLOT);
            continue;
        }
        if (seL4_GetMR(0) == STATUS) {
            unsigned i = 0;
            for (; error[i] && i < STATUS_BYTES-1; ++i) seL4_SetMR(i,error[i]);
            seL4_SetMR(i,0);
            IPC_REPLY(REPLY_SLOT, message(i+1));
        } else {
            unsigned x=0,y=0; bool down=false;
            bool valid = !error[0] && touch_sample(&x,&y,&down);
            seL4_SetMR(0, valid | ((unsigned)down << 1));
            seL4_SetMR(1,x);
            seL4_SetMR(2,y);
            IPC_REPLY(REPLY_SLOT, message(3));
        }
    }
}
const char *touch_status(void) {
    static char status[STATUS_BYTES];
    seL4_SetMR(0,STATUS);
    unsigned length=seL4_MessageInfo_get_length(seL4_Call(TOUCH_CLIENT_EP,message(1)));
    if (!length || length>sizeof(status)) return "TOUCH IPC ERROR";
    for (unsigned i=0;i<length;i++) status[i]=seL4_GetMR(i);
    status[length-1]=0;
    return status[0]?status:NULL;
}
bool touch_event(unsigned *x,unsigned *y,bool *down) {
    seL4_SetMR(0,SAMPLE);
    if (seL4_MessageInfo_get_length(seL4_Call(TOUCH_CLIENT_EP,message(1)))!=3) return false;
    unsigned flags=seL4_GetMR(0);
    *down=(flags&2)!=0; *x=seL4_GetMR(1); *y=seL4_GetMR(2);
    return flags&1;
}
