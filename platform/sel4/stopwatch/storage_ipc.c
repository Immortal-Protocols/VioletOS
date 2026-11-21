#include "tasks.h"
unsigned storage_receive(unsigned *value) {
    unsigned length=seL4_MessageInfo_get_length(seL4_Recv(STORAGE_EP,0));
    if(length!=2) return ~0u;
    *value=seL4_GetMR(1);
    return seL4_GetMR(0);
}
void storage_reply(bool ok,unsigned value) {
    seL4_SetMR(0,ok); seL4_SetMR(1,value);
    seL4_Reply(seL4_MessageInfo_new(0,0,0,2));
}
bool app_checkpoint(unsigned region,bool save,unsigned *value) {
#if defined(VIOLET_COMPONENT) || defined(VIOLET_QEMU)
    seL4_SetMR(0,region*2+save); seL4_SetMR(1,*value);
    if(seL4_MessageInfo_get_length(seL4_Call(STORAGE_CLIENT_EP,seL4_MessageInfo_new(0,0,0,2)))!=2 || !seL4_GetMR(0)) return false;
    *value=seL4_GetMR(1);
    return true;
#else
    return false;
#endif
}

bool checkpoint(bool save,unsigned *value) { return app_checkpoint(0,save,value); }
