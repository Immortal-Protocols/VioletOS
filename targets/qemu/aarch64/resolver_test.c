#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>
#define REQUIRE(expr) do { if (!(expr)) { dprintf(2,"line %d errno %d\n",__LINE__,errno); return 1; } } while (0)
int main(void) {
    struct timeval tv;
    REQUIRE(!gettimeofday(&tv, NULL) && tv.tv_sec > 1609459200 && tv.tv_usec >= 0 && tv.tv_usec < 1000000);
    int fd = socket(AF_INET, SOCK_DGRAM, 0), enabled = 1;
    REQUIRE(fd >= 0 && !setsockopt(fd, IPPROTO_IP, IP_RECVERR, &enabled, sizeof(enabled)));
    socklen_t size = sizeof(enabled);
    enabled = 0;
    REQUIRE(!getsockopt(fd, IPPROTO_IP, IP_RECVERR, &enabled, &size) && enabled);
    struct sockaddr_in resolver = {.sin_family=AF_INET, .sin_port=htons(53)};
    REQUIRE(inet_pton(AF_INET, "10.0.0.53", &resolver.sin_addr)==1 && !connect(fd,(void *)&resolver,sizeof(resolver)));
    uint8_t a[]={0xaa,1,1,0,0,1,0,0,0,0,0,0,7,'e','x','a','m','p','l','e',3,'c','o','m',0,0,1,0,1}, b[sizeof(a)];
    memcpy(b,a,sizeof(a)); b[1]=2; b[sizeof(b)-3]=28;
    struct iovec vectors[2]={{a,sizeof(a)}, {b,sizeof(b)}};
    struct mmsghdr batch[2]={0};
    for (int i=0;i<2;i++) { batch[i].msg_hdr.msg_iov=&vectors[i]; batch[i].msg_hdr.msg_iovlen=1; }
    REQUIRE(sendmmsg(fd,batch,2,0)==2 && batch[0].msg_len==sizeof(a) && batch[1].msg_len==sizeof(b));
    uint8_t answers[2][512];
    ssize_t n=recv(fd,answers[0],2,MSG_PEEK|MSG_TRUNC);
    REQUIRE(n>12 && n < 512 && answers[0][0]==0xaa);
    REQUIRE(recv(fd,answers[0],512,0)==n && answers[0][1]==1 && answers[0][7]==1);
    REQUIRE(recv(fd,answers[1],512,0)>12 && answers[1][1]==2 && answers[1][7]==0);
    puts("Violet resolver: dual DNS queries, datagram peek and wall clock passed");
    return 0;
}
