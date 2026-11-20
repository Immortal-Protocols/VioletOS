#include <stddef.h>
void *memcpy(void *v,const void *s,size_t n) { unsigned char *d=v; const unsigned char *p=s; while(n--) *d++=*p++; return v; }
void *memset(void *v,int c,size_t n) { unsigned char *d=v; while(n--) *d++=c; return v; }
int memcmp(const void *v,const void *s,size_t n) { const unsigned char *a=v,*b=s; while(n--) { if(*a!=*b) return *a-*b; ++a; ++b; } return 0; }
void *memchr(const void *v,int c,size_t n) { const unsigned char *p=v; while(n--) { if(*p==(unsigned char)c) return (void *)p; ++p; } return NULL; }
size_t strlen(const char *s) { size_t n=0; while(s[n]) ++n; return n; }
size_t strnlen(const char *s,size_t max) { size_t n=0; while(n<max && s[n]) ++n; return n; }
int strcmp(const char *a,const char *b) { while(*a && *a==*b) { ++a; ++b; } return (unsigned char)*a-(unsigned char)*b; }
char *strrchr(const char *s,int c) { const char *found=NULL; do { if (*s==c) found=s; } while(*s++); return (char *)found; }
void __aeabi_memcpy(void *d,const void *s,size_t n) { memcpy(d,s,n); }
void __aeabi_memclr(void *d,size_t n) { memset(d,0,n); }
