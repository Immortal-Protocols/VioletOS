// Developer handset package transfer, signed Radio activation and diagnostics.
#include <libusb.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <time.h>
#include <unistd.h>
#include <string.h>

static uint32_t word(const unsigned char *p) {
    return (uint32_t)p[0] | (uint32_t)p[1]<<8 | (uint32_t)p[2]<<16 | (uint32_t)p[3]<<24;
}
static uint32_t crc(const unsigned char *p,size_t n) {
    uint32_t v=~0u;
    while(n--){ v^=*p++; for(unsigned b=0;b<8;b++)v=(v>>1)^((0u-(v&1u))&0xedb88320u); }
    return ~v;
}
static int transfer(libusb_device_handle *h, uint8_t dir, uint8_t op, uint32_t address,
                    unsigned char *data,uint16_t length) {
    int rc=libusb_control_transfer(h,dir,op,address&65535,address>>16,data,length,3000);
    if(rc==length)return 0;
    fprintf(stderr,"USB request %02x at %u: %s (%d of %u)\n",op,address,
            rc<0?libusb_error_name(rc):"short transfer",rc,length);
    return 1;
}
static int status(libusb_device_handle *h,unsigned char b[24]) {
    if(transfer(h,0xc0,0x62,0,b,24)||memcmp(b,"VPKS",4))return 1;
    printf("Radio CRC %08x, catalog bank %u, inbox ID %u, %u/%u bytes\n",
           word(b+12),word(b+20),word(b+16),word(b+8),word(b+4));
    return 0;
}
static int stage(libusb_device_handle *h,const char *file) {
    FILE *f=fopen(file,"rb");
    if(!f){perror(file);return 1;}
    unsigned char bytes[0x3e000+25];
    size_t n=fread(bytes,1,sizeof bytes,f);
    int bad=ferror(f);fclose(f);
    if(bad || n<24 || n==sizeof bytes || memcmp(bytes,"VPK1",4) || word(bytes+16)!=n-24
       || word(bytes+20)!=crc(bytes+24,n-24)) {
        fprintf(stderr,"Invalid VioletOS package\n");return 1;
    }
    uint32_t sum=crc(bytes,n);
    unsigned char digest[]={sum,sum>>8,sum>>16,sum>>24};
    if(transfer(h,0x40,0x60,(uint32_t)n,digest,4))return 1;
    for(size_t pos=0;pos<n;pos+=40){
        uint16_t count=n-pos<40?(uint16_t)(n-pos):40;
        if(transfer(h,0x40,0x61,(uint32_t)pos,bytes+pos,count))return 1;
    }
    unsigned char reply[24];
    if(status(h,reply)||word(reply+4)!=n||word(reply+8)!=n||word(reply+16)!=word(bytes+8))return 1;
    printf("Staged package CRC %08x.\n",word(bytes+20));
    return 0;
}
static int activate_signed(libusb_device_handle *h, const char *signature) {
    FILE *file = fopen(signature, "rb");
    if (!file) { perror(signature); return 1; }
    unsigned char der[73];
    size_t n = fread(der, 1, sizeof der, file);
    int failed = ferror(file);
    fclose(file);
    if (failed || n <= 40 || n > 72) {
        fputs("Invalid P-256 signature length\n", stderr);
        return 1;
    }
    if (transfer(h,0x40,0x66,0,der,40) ||
        transfer(h,0x40,0x67,0,der+40,(uint16_t)(n-40))) return 1;
    return transfer(h,0x40,0x68,0,NULL,0);
}
int main(int argc,char **argv) {
    int radio = argc == 3 && !strcmp(argv[1], "activate-radio");
    int diagnostics = argc == 3 && !strcmp(argv[1], "activate-diagnostics");
    int confirm = argc == 3 && !strcmp(argv[1], "confirm-diagnostics");
    int activate = radio || diagnostics;
    if((argc!=2 && !activate && !confirm) || (!strcmp(argv[1],"--help"))){fputs("moto-package status | stopwatch | provision-wpa2 | join-wifi | weather=CITY | scan-wifi | reboot | reboot-bootloader | FILE.vpk | activate-radio SIGNATURE | activate-diagnostics SIGNATURE | diagnostics-status | diagnostics-recovery | confirm-diagnostics GENERATION\n",stderr);return 2;}
    libusb_context *ctx;
    if(libusb_init(&ctx)<0)return 1;
    libusb_device **devices;
    ssize_t count=libusb_get_device_list(ctx,&devices);
    int rc=1; libusb_device_handle *chosen=NULL;
    for(ssize_t i=0;i<count;i++) {
        struct libusb_device_descriptor d;
        libusb_device_handle *h=NULL;
        if(libusb_get_device_descriptor(devices[i],&d)||d.idVendor!=0x1209||d.idProduct!=1||
           libusb_open(devices[i],&h)<0)continue;
        unsigned char name[64];
        if(libusb_get_string_descriptor_ascii(h,d.iProduct,name,sizeof name)>0 &&
           !strcmp((char*)name,"VioletOS Diagnostics")){
            if(chosen){libusb_close(h);libusb_close(chosen);chosen=NULL;break;}
            chosen=h;
        }else libusb_close(h);
    }
    if(count>=0)libusb_free_device_list(devices,1);
    if(chosen){
        unsigned char b[24];
        if (activate) {
            if (!status(chosen,b) && word(b+16) == (radio ? 7u : 21u))
                rc = activate_signed(chosen, argv[2]);
        } else if (!strcmp(argv[1], "diagnostics-status")) {
            rc = transfer(chosen, 0xc0, 0x6d, 0, b, 12) || memcmp(b,"VUD1",4);
            if (!rc) printf("Diagnostics CRC %08x, trial %u\n", word(b+4), word(b+8));
        } else if (!strcmp(argv[1], "diagnostics-recovery")) {
            rc = transfer(chosen, 0xc0, 0x6d, 0, b, 16) || memcmp(b,"VUD1",4);
            if (!rc) {
                const char *reasons[] = {"none", "Diagnostics fault", "confirmation timeout", "dependency fault"};
                uint32_t reason = word(b+12);
                printf("Last trial failure: %s\n", reason < 4 ? reasons[reason] : "unknown");
            }
        } else if (confirm) {
            char *end = NULL;
            unsigned long token = strtoul(argv[2], &end, 10);
            if (token && token <= UINT32_MAX && end && !*end)
                rc = transfer(chosen, 0x40, 0x6e, (uint32_t)token, NULL, 0);
        } else if (!strcmp(argv[1],"provision-wpa2")) {
            unsigned char payload[73];
            // nonce (4), SSID length (1), padded SSID (32), PMK (32), UTC seconds (4).
            if (fread(payload,1,sizeof payload,stdin)==sizeof payload &&
                fgetc(stdin)==EOF && payload[4] && payload[4]<=32 && word(payload)) {
                rc=transfer(chosen,0x40,0x6a,word(payload),payload+5,payload[4]) ||
                   transfer(chosen,0x40,0x6b,word(payload),payload+37,36);
            }
            for (size_t i=0;i<sizeof payload;i++) ((volatile unsigned char *)payload)[i]=0;
        } else if (!strcmp(argv[1],"stopwatch")) {
            rc=transfer(chosen,0xc0,0x64,0,b,8) || memcmp(b,"VSW1",4);
            if (!rc) printf("Saved stopwatch: %u.%u seconds\n",word(b+4)/10,word(b+4)%10);
        } else if (!strcmp(argv[1],"join-wifi") || !strncmp(argv[1],"weather=",8)) {
            // A weather job sets the token's top bit and carries the city in its low byte.
            int weather=!strncmp(argv[1],"weather=",8);
            uint32_t token=arc4random_uniform(UINT32_MAX)+1;
            token&=0x7fffffffu;
            if (!token) token=1;
            if (weather) token=0x80000000u|(token&0x7fffff00u)|((uint32_t)atoi(argv[1]+8)&0xff);
            unsigned char result[44];
            if (!transfer(chosen,0x40,0x6c,token,NULL,0)) {
                time_t until=time(NULL)+(getenv("MOTO_WIFI_WAIT") ? atoi(getenv("MOTO_WIFI_WAIT")) : 85);
                while(time(NULL)<until) {
                    if(transfer(chosen,0xc0,0x58,0,result,sizeof result)) break;
                    unsigned state=word(result+4);
                    if(word(result)==token && state) {
                        if(state==1) {printf(weather ? "Moto weather: %.36s\n" : "Moto HTTPS UUID: %.36s\n",(char *)result+8);rc=0;}
                        else fprintf(stderr,"Moto Wi-Fi step failed (%u)\n",state);
                        break;
                    }
                    usleep(250000);
                }
                if(time(NULL)>=until) fputs("Timed out waiting for the handset Wi-Fi result\n",stderr);
            }
        } else if (!strcmp(argv[1],"scan-wifi")) {
            rc=transfer(chosen,0x40,0x69,0,NULL,0);
            if (!rc) puts("Scan requested. Read the result with moto-debug.");
        } else if (!strcmp(argv[1],"reboot-bootloader")) {
            rc=transfer(chosen,0x40,0x70,0,NULL,0);
            if (!rc) puts("Fastboot reboot requested. Wait for the bootloader with fastboot devices.");
        } else if (!strcmp(argv[1],"reboot")) {
            rc=transfer(chosen,0x40,0x65,0,NULL,0);
            if (!rc) puts("Reboot requested. Verify USB reconnect and check the active catalog.");
        } else {
            rc=!strcmp(argv[1],"status")?status(chosen,b):stage(chosen,argv[1]);
        }
        libusb_close(chosen);
    }else fprintf(stderr,"Connect one VioletOS handset with USB diagnostics.\n");
    libusb_exit(ctx);return rc;
}
