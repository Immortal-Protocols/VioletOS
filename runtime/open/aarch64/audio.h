// Same speaker control page and 48 kHz stereo s16 output ring as targets/qemu/audio.rs.
enum { AUDIO_FRAMES = 8192, AUDIO_RATE = 48000, AUDIO_LEAD = 1920 };
struct audio_card {
    volatile uint32_t control[1024];
    volatile int16_t samples[AUDIO_FRAMES][2];
    int16_t microphone[4096];
};
static struct audio_card audio_bridge __attribute__((aligned(16384)));
struct audio_stream {
    struct file_cursor cursor;
    uint8_t partial[4], used, channels;
    bool nonblock, background;
};
static struct audio_stream speakers[32];
static uint64_t audio_deadline;

static long open_sound(unsigned flags) {
    if ((flags & 3) == 0 || (flags & 3) == 3) return -13;
    int fd = free_fd();
    if (fd < 0) return fd;
    for (unsigned i = 0; i < sizeof(speakers)/sizeof(speakers[0]); i++) {
        struct audio_stream *s = &speakers[i];
        if (s->cursor.refs) continue;
        *s = (struct audio_stream){.cursor.refs = 1, .channels = 2, .nonblock = !!(flags & 0x800)};
        fds[fd] = (struct descriptor){.file=&audio_node, .cursor=&s->cursor, .cloexec=!!(flags & 0x80000)};
        volatile uint32_t *reg = audio_bridge.control;
        reg[3] = 1 | (reg[1] & 1) << 1;
        reg[4] = 100;
        reg[6] = AUDIO_RATE;
        reg[7] = AUDIO_FRAMES;
        reg[2] = 0x31445541;
        return fd;
    }
    return -23;
}

static long configure_sound(struct descriptor *d, uint64_t cmd, uintptr_t addr) {
    uint32_t config[4];
    if (cmd != 0x5610) return -25;
    if (!transfer(addr, config, sizeof(config), false)) return -14;
    if (config[0] != AUDIO_RATE || config[1] < 1 || config[1] > 2 || config[2] != 1) return -22;
    struct audio_stream *s = (struct audio_stream *)d->cursor;
    s->channels = config[1];
    s->background = config[3] != 0;
    s->used = 0;
    return 0;
}

static long write_sound(struct descriptor *d, uintptr_t addr, size_t len) {
    if (!len) return 0;
    struct audio_stream *s = (struct audio_stream *)d->cursor;
    if (!s->background && foreground_pid && foreground_pid != active_process->pid) {
        s->used = 0;
        return len;
    }
    // Throttle against the guest's monotonic clock even with no host speaker attached.
    uint64_t now = ticks_now(), ahead = audio_deadline > now ? audio_deadline-now : 0;
    unsigned queued = (ahead * AUDIO_RATE + timer_hz - 1) / timer_hz;
    if (queued >= AUDIO_LEAD) return s->nonblock ? -11 : retry_io(now + timer_hz/1000);
    unsigned frame_bytes = 2 * s->channels;
    size_t room = (AUDIO_LEAD - queued) * frame_bytes - s->used;
    uint8_t buf[2048];
    if (len > sizeof(buf) - s->used) len = sizeof(buf) - s->used;
    if (len > room) len = room;
    memcpy(buf, s->partial, s->used);
    if (!transfer(addr, buf+s->used, len, false)) return -14;
    unsigned bytes = s->used + len, frames = bytes / frame_bytes;
    volatile uint32_t *reg = audio_bridge.control;
    uint32_t written = reg[5], peak = reg[10];
    for (unsigned i = 0; i < frames; i++) {
        const uint8_t *src = buf + i * frame_bytes;
        for (unsigned ch = 0; ch < 2; ch++) {
            unsigned at = s->channels == 2 ? ch*2 : 0;
            int16_t sample = (int16_t)(src[at] | src[at+1] << 8);
            audio_bridge.samples[written % AUDIO_FRAMES][ch] = sample;
            uint32_t magnitude = sample < 0 ? -(int)sample : sample;
            if (magnitude > peak) peak = magnitude;
        }
        written++;
    }
    s->used = bytes % frame_bytes;
    memcpy(s->partial, buf + bytes - s->used, s->used);
    audio_deadline = now + ahead + (uint64_t)frames * timer_hz / AUDIO_RATE;
    reg[10] = peak;
    reg[11] += frames;
    __atomic_store_n(&reg[5], written, __ATOMIC_RELEASE);
    return len;
}
