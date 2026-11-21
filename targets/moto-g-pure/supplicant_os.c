// Freestanding hostap support for the single-threaded Radio process.
#include "utils/includes.h"

#include "utils/common.h"
#include "utils/eloop.h"

extern unsigned ticks(void), ticks_per_ms;
extern int radio_random(unsigned char *, size_t);

struct block {
  size_t capacity;
  unsigned used;
};
static union {
  uint64_t align;
  unsigned char data[16384];
} arena;
void *malloc(size_t size) {
  if (!size || size > sizeof(arena.data) - sizeof(struct block))
    return NULL;
  size = (size + 7) & ~(size_t)7;
  struct block *first = (void *)arena.data;
  if (!first->capacity)
    first->capacity = sizeof(arena.data) - sizeof(*first);
  for (unsigned char *pos = arena.data;
       pos < arena.data + sizeof(arena.data);) {
    struct block *b = (void *)pos;
    if (!b->used && size <= b->capacity) {
      if (b->capacity >= size + sizeof(*b) + 8) {
        struct block *next = (void *)(pos + sizeof(*b) + size);
        next->capacity = b->capacity - size - sizeof(*b);
        next->used = 0;
        b->capacity = size;
      }
      b->used = 1;
      return b + 1;
    }
    pos += sizeof(*b) + b->capacity;
  }
  return NULL;
}
void free(void *p) {
  if (!p)
    return;
  struct block *b = (struct block *)p - 1;
  for (size_t i = 0; i < b->capacity; ++i)
    ((volatile unsigned char *)p)[i] = 0;
  b->used = 0;
  for (unsigned char *pos = arena.data;
       pos < arena.data + sizeof(arena.data);) {
    b = (void *)pos;
    unsigned char *end = pos + sizeof(*b) + b->capacity;
    if (!b->used && end < arena.data + sizeof(arena.data) &&
        !((struct block *)end)->used) {
      b->capacity += sizeof(*b) + ((struct block *)end)->capacity;
    } else
      pos = end;
  }
}
void *os_zalloc(size_t size) {
  void *p = malloc(size);
  if (p)
    memset(p, 0, size);
  return p;
}
void *os_memdup(const void *src, size_t size) {
  void *p = malloc(size);
  return p ? memcpy(p, src, size) : NULL;
}
int os_get_random(unsigned char *out, size_t len) {
  return radio_random(out, len);
}

static uint64_t millis(void) {
  static unsigned previous, residue;
  static uint64_t total;
  unsigned current = ticks(), delta = current - previous;
  previous = current;
  total += delta / ticks_per_ms;
  residue += delta % ticks_per_ms;
  if (residue >= ticks_per_ms) {
    residue -= ticks_per_ms;
    ++total;
  }
  return total;
}
int os_get_reltime(struct os_reltime *t) {
  uint64_t ms = millis();
  t->sec = (long)(ms / 1000);
  t->usec = (long)(ms % 1000) * 1000;
  return 0;
}
struct timeout {
  eloop_timeout_handler fn;
  void *ctx, *data;
  uint64_t at;
};
static struct timeout timers[12];
int eloop_register_timeout(unsigned secs, unsigned usecs,
                           eloop_timeout_handler fn, void *ctx, void *data) {
  for (unsigned i = 0; i < 12; i++)
    if (!timers[i].fn) {
      timers[i] = (struct timeout){
          fn, ctx, data, millis() + 1000ULL * secs + (usecs + 999ULL) / 1000};
      return 0;
    }
  return -1;
}
int eloop_cancel_timeout(eloop_timeout_handler fn, void *ctx, void *data) {
  int count = 0;
  for (unsigned i = 0; i < 12; i++)
    if (timers[i].fn == fn && (ctx == ELOOP_ALL_CTX || timers[i].ctx == ctx) &&
        (data == ELOOP_ALL_CTX || timers[i].data == data)) {
      timers[i].fn = NULL;
      ++count;
    }
  return count;
}
void station_wpa_poll(void) {
  uint64_t now = millis();
  for (unsigned i = 0; i < 12; i++)
    if (timers[i].fn && timers[i].at <= now) {
      struct timeout t = timers[i];
      timers[i].fn = NULL;
      t.fn(t.ctx, t.data);
    }
}

int os_memcmp_const(const void *a, const void *b, size_t n) {
  const unsigned char *left = a, *right = b;
  volatile unsigned char diff = 0;
  for (size_t i = 0; i < n; i++)
    diff |= left[i] ^ right[i];
  return diff;
}
// Only optional diagnostic labels use formatting in this hostap configuration.
int station_format_disabled(char *out, size_t n, const char *fmt, ...) {
  if (n)
    out[0] = 0;
  return -1;
}
unsigned radio_monotonic_ms(void) { return (unsigned)millis(); }
