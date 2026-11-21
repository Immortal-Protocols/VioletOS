// QEMU substitutes the USB host and Moto eMMC at their IPC boundaries.
// The production Moto supervisor still selects, confirms and rebuilds the catalog.
extern void ipc_call(unsigned slot, unsigned *words);
extern unsigned ipc_receive(unsigned slot, unsigned *words);
extern void ipc_reply(unsigned *words);
extern void delay_ms(unsigned ms);
void component_main(unsigned id) {
    unsigned words[14] = {0};
    if (id == 6) {
        for (;;) {
            ipc_receive(2, words);
            if (words[0] == 0x56434d54) {
                volatile unsigned *catalog = (void *)0x0d100000;
                words[0] = catalog[0] == 0x31544356 && catalog[1] + 16 == words[2] ? 0 : 2;
                words[2] = 1;
            } else {
                words[0] = 2;
            }
            ipc_reply(words);
        }
    }
    words[0] = 3;
    words[1] = 0x6d;
    ipc_call(7, words);
    if (!words[0] && words[3]) {
        words[2] = words[3];
        words[1] = 0x6e;
        words[0] = 3;
        ipc_call(7, words);
    }
    for (;;) delay_ms(10);
}
