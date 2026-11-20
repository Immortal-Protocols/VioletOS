// Protocol for the measured NT36525B. Firmware loads into controller SRAM.
#include "spi.h"
#include "firmware.h"
extern const uint8_t touch_firmware[];
static bool page(unsigned address) {
    uint8_t cmd[]={0xff,address>>15,address>>7};
    return exchange(cmd,sizeof(cmd));
}
enum write_result { WRITE_OK, PAGE_TIMEOUT, DATA_TIMEOUT };

static enum write_result write_chunk(unsigned address,const uint8_t *data,unsigned count) {
    uint8_t buf[32];
    if (!page(address)) return PAGE_TIMEOUT;
    buf[0]=0x80|(address&127);
    for (unsigned i=0;i<count;i++) buf[i+1]=data[i];
    return exchange(buf,count+1) ? WRITE_OK : DATA_TIMEOUT;
}
static bool write_at(unsigned address,const uint8_t *data,unsigned size) {
    // Scalar control writes never cross a page.
    return write_chunk(address,data,size)==WRITE_OK;
}
static bool read_at(unsigned address,uint8_t *data,unsigned count);

// After a lost completion, SRAM may already contain the intended bytes.
static bool matches_sram(unsigned address,const uint8_t *expected,unsigned count) {
    uint8_t actual[30];
    while (count) {
        unsigned n=count>sizeof(actual)?sizeof(actual):count;
        if (!read_at(address,actual,n)) return false;
        for (unsigned j=0;j<n;j++) if (actual[j]!=expected[j]) return false;
        address+=n; expected+=n; count-=n;
    }
    return true;
}
static void hex_value(char *dst,unsigned value,unsigned digits) {
    while (digits) {
        dst[--digits]="0123456789ABCDEF"[value&15];
        value>>=4;
    }
}
static const char *upload_firmware(void) {
    unsigned limit=31;
    for (unsigned i=0;i<sizeof(parts)/sizeof(*parts);i++) {
        unsigned offset=0;
        while (offset<parts[i].size) {
            unsigned address=parts[i].addr+offset;
            unsigned count=parts[i].size-offset;
            unsigned room=128-(address&127);
            if (count>room) count=room;
            if (count>limit) count=limit;
            enum write_result result=DATA_TIMEOUT;
            unsigned timeout_state=0;
            const uint8_t *expected=touch_firmware+parts[i].offset+offset;
            for (unsigned attempt=0;attempt<5;attempt++) {
                result=write_chunk(address,expected,count);
                if (result==WRITE_OK) break;
                timeout_state=spi_timeout_state();
                if (result==DATA_TIMEOUT && matches_sram(address,expected,count)) {
                    result=WRITE_OK;
                    break;
                }
                watchdog_progress();
                delay_ms(1);
                if (attempt==1 && result==DATA_TIMEOUT && count>3) {
                    // One FIFO word including the command byte.
                    count=limit=3;
                }
            }
            if (result!=WRITE_OK) {
                static char failure[]="FW DATA 000000 S1 00000000";
                const char *phase=result==PAGE_TIMEOUT ? "PAGE" : "DATA";
                for (unsigned j=0;j<4;j++) failure[3+j]=phase[j];
                hex_value(failure+8,address,6);
                hex_value(failure+18,timeout_state,8);
                return failure;
            }
            offset+=count;
            watchdog_progress();
        }
    }
    return 0;
}
static bool put(unsigned address,unsigned value,unsigned bytes) {
    uint8_t data[]={value,value>>8,value>>16,value>>24};
    return write_at(address,data,bytes);
}
static bool read_at(unsigned address,uint8_t *data,unsigned count) {
    uint8_t buf[32]={0};
    if(count>30 || !page(address)) return false;
    buf[0]=address&127;
    if(!exchange(buf,count+2)) return false;
    for(unsigned i=0;i<count;i++) data[i]=buf[i+2];
    return true;
}
static const char *reset_write(unsigned addr, uint8_t value, const char *page_error, const char *data_error) {
    const char *error=page_error;
    for (unsigned i=0;i<5;i++) {
        if (page(addr)) {
            uint8_t data[]={0x80|(addr&127),value};
            if (exchange(data,sizeof(data))) return 0;
            error=data_error;
        } else {
            error=page_error;
        }
        delay_ms(1);
    }
    return error;
}
static const char *boot_reset(unsigned pass) {
    const char *error=reset_write(0x3f0fe,0x69,"RESET PAGE","RESET DATA");
    if (!error) {
        delay_ms(5);
        error=reset_write(0x3f310,0,"FAST PAGE","FAST DATA");
    }
    if (!error) return 0;
    static char message[38];
    unsigned n=0;
    message[n++]='R'; message[n++]='0'+pass; message[n++]=' ';
    while (*error) message[n++]=*error++;
    message[n++]=' '; message[n++]='S'; hex_value(message+n,spi_timeout_state(),8); n+=8;
    message[n++]=' '; message[n++]='C'; hex_value(message+n,spi_timeout_command(),8); n+=8;
    message[n]=0;
    return message;
}
static bool identified(unsigned addr) {
    uint8_t id[6];
    return read_at(addr,id,6) && id[0]==11 && id[3]==0x25 && id[4]==0x65 && id[5]==3;
}
const char *touch_init(void) {
    if(!bus_start()) return "SPI CLOCK FAILED";
    const char *error=boot_reset(1);
    if(error) return error;
    if(!identified(0x3f004) && !identified(0x1f64e)) return "TOUCH ID FAILED";
    error=boot_reset(2);
    if(error) return error;
    for (unsigned bank=0; bank<2; bank++) {
        const unsigned addresses[] = {bank?0x3f12c:0x3f128, bank?0x3f130:0x3f118, bank?0x3f104:0x3f100};
        const unsigned values[] = {parts[bank].addr, parts[bank].size-1, parts[bank].crc};
        const unsigned widths[] = {3,2,4};
        static const char *const failures[2][3] = {
            {"ILM ADDRESS TIMEOUT","ILM LENGTH TIMEOUT","ILM CRC TIMEOUT"},
            {"DLM ADDRESS TIMEOUT","DLM LENGTH TIMEOUT","DLM CRC TIMEOUT"}
        };
        for (unsigned field=0; field<3; field++) {
            unsigned attempt=0;
            while (!put(addresses[field],values[field],widths[field])) {
                if (++attempt==5) return failures[bank][field];
                delay_ms(1);
            }
        }
    }
    error=upload_firmware();
    if(error) return error;
    uint8_t crc=0;
    if(!read_at(0x3f30e,&crc,1) || !put(0x3f30e,crc|128,1) ||
       !put(0x21c60,0,1) || !put(0x21c50,0xae,1) || !put(0x3f10d,1,1)) return "FW START FAILED";
    for(unsigned i=0;i<100;i++) {
        uint8_t state;
        delay_ms(10);
        if(read_at(0x21c60,&state,1) && state>=0xa1 && state<=0xaf) return 0;
    }
    return "FW READY TIMEOUT";
}
bool touch_sample(unsigned *x,unsigned *y,bool *down) {
    uint8_t first[6];
    if(!read_at(0x21c00,first,6)) return false;
    unsigned id=first[0]>>3,state=first[0]&7;
    *down=id>=1 && id<=10 && (state==1 || state==2);
    if(*down) {
        *x=(first[1]<<4)|(first[3]>>4);
        *y=(first[2]<<4)|(first[3]&15);
        if(*x>=720 || *y>=1600) return false;
    }
    return true;
}
