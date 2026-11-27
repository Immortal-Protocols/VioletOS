// VioletOS backend for the upstream AudioServer platform device.
#include <AK/Array.h>
#include <AudioServer/PlatformAudio.h>
#include <LibCore/EventLoop.h>
#include <LibCore/Timer.h>
#include <errno.h>
#include <fcntl.h>
#include <math.h>
#include <sys/ioctl.h>
#include <unistd.h>

namespace Audio {

class VioletPlaybackStream final : public PlaybackStream {
public:
    VioletPlaybackStream(int fd, u32 latency_ms, AudioDataRequestCallback callback)
        : m_fd(fd)
        , m_lookahead(AK::Duration::from_milliseconds(clamp(latency_ms, 5u, 30u)))
        , m_pull(move(callback))
        , m_loop(Core::EventLoop::current())
        , m_timer(Core::Timer::create_repeating(AK::Duration::from_milliseconds(5), [this] { pump(); }))
    {
    }

    ~VioletPlaybackStream() override
    {
        m_timer->stop();
        ::close(m_fd);
    }

    SampleSpecification sample_specification() const override { return { 48000, ChannelMap::stereo() }; }

    void start()
    {
        m_playing = true;
        m_awaiting_data = false;
        m_timer->start();
    }

    using Completion = Core::ThreadedPromise<void>;

    NonnullRefPtr<Completion> resume() override
    {
        return schedule([this](auto& done) {
            finish_drains();
            start();
            done.resolve();
        });
    }

    NonnullRefPtr<Completion> drain_buffer_and_suspend() override
    {
        return schedule([this](auto& done) {
            m_playing = false;
            m_drains.append(done);
            m_timer->start();
        });
    }

    NonnullRefPtr<Completion> discard_buffer_and_suspend() override
    {
        return schedule([this](auto& done) {
            m_playing = false;
            m_pending = 0;
            m_timer->stop();
            finish_drains();
            done.resolve();
        });
    }

    void notify_data_available() override
    {
        m_loop.deferred_invoke([self = NonnullRefPtr(*this)] {
            self->m_awaiting_data = false;
            if (self->m_playing)
                self->m_timer->start();
        });
    }

    NonnullRefPtr<Completion> set_volume(double volume) override
    {
        return schedule([this, volume](auto& done) {
            m_volume = isfinite(volume) ? clamp(volume, 0., 1.) : 0.;
            done.resolve();
        });
    }

private:
    template<class F>
    NonnullRefPtr<Completion> schedule(F operation)
    {
        auto promise = Completion::create();
        m_loop.deferred_invoke([self = NonnullRefPtr(*this), promise, operation = move(operation)]() mutable {
            if (self->m_failed)
                promise->reject(Error::from_string_literal("VioletOS sound device closed"));
            else
                operation(*promise);
        });
        return promise;
    }

    void finish_drains()
    {
        auto drains = move(m_drains);
        for (auto& done : drains)
            done->resolve();
    }

    void lost(int error)
    {
        m_failed = true;
        m_timer->stop();
        auto drains = move(m_drains);
        for (auto& done : drains)
            done->reject(Error::from_errno(error));
        if (on_output_lost)
            on_output_lost();
    }

    void pump()
    {
        auto alive = NonnullRefPtr(*this);
        auto now = MonotonicTime::now();
        // Fill at most the selected lookahead. Each nonblocking device write
        // leaves the control loop free to service other tabs and IPC clients.
        while (m_pending || (m_playing && !m_awaiting_data && m_played_by < now + m_lookahead)) {
            if (!m_pending) {
                auto start = max(now, m_played_by);
                auto samples = m_pull(m_request.span(), start);
                auto n = min(samples.size(), m_pcm.size()) & ~size_t(1);
                if (!n) {
                    m_awaiting_data = true;
                    break;
                }
                for (size_t i = 0; i < n; ++i) {
                    double sample = samples[i] * m_volume;
                    m_pcm[i] = isfinite(sample) ? static_cast<i16>(clamp(sample * 32768., -32768., 32767.)) : 0;
                }
                m_offset = 0;
                m_pending = n * sizeof(i16);
            }
            auto rc = ::write(m_fd, reinterpret_cast<u8 const*>(m_pcm.data()) + m_offset, m_pending);
            if (rc < 0 && errno == EINTR)
                continue;
            if (rc < 0 && (errno == EAGAIN || errno == EWOULDBLOCK))
                return;
            if (rc <= 0) {
                lost(rc < 0 ? errno : EIO);
                return;
            }
            auto old_frames = m_offset / 4;
            m_offset += static_cast<size_t>(rc);
            m_pending -= static_cast<size_t>(rc);
            m_played_by = max(m_played_by, now) + AK::Duration::from_time_units(m_offset / 4 - old_frames, 1, 48000);
        }
        if (!m_pending && !m_playing && m_played_by <= now)
            finish_drains();
        if (!m_pending && (!m_playing || m_awaiting_data) && m_drains.is_empty())
            m_timer->stop();
    }

    int m_fd;
    AK::Duration m_lookahead;
    AudioDataRequestCallback m_pull;
    Core::EventLoop& m_loop;
    NonnullRefPtr<Core::Timer> m_timer;
    Vector<NonnullRefPtr<Completion>> m_drains;
    Array<float, 480> m_request; // 5 ms at 48 kHz, interleaved stereo
    Array<i16, 480> m_pcm;
    size_t m_offset { 0 };
    size_t m_pending { 0 };
    MonotonicTime m_played_by { MonotonicTime::now() };
    double m_volume { 1 };
    bool m_playing { false };
    bool m_awaiting_data { false };
    bool m_failed { false };
};

// Keep one configured speaker handle for AudioServer's lifetime. The service opens
// it before installing its filesystem and syscall filters. Individual playback
// streams only duplicate this descriptor and write mixed PCM.
static int sound_device = -1;
static int initialization_error;

ErrorOr<void> prepare_violet_playback_device()
{
    if (sound_device >= 0)
        return {};
    if (initialization_error)
        return Error::from_errno(initialization_error);
    int fd = ::open("/dev/violet/audio", O_WRONLY | O_NONBLOCK | O_CLOEXEC);
    if (fd < 0) {
        initialization_error = errno;
        return Error::from_errno(initialization_error);
    }
    // WebContent is foreground, while the mixer enforces authorization and mute
    // for each tab's background playback group.
    u32 config[] = { 48000, 2, 1, 1 };
    if (::ioctl(fd, 0x5610, config) < 0) {
        initialization_error = errno;
        ::close(fd);
        return Error::from_errno(initialization_error);
    }
    sound_device = fd;
    return {};
}

NonnullRefPtr<PlaybackStream::CreatePromise> create_platform_playback_stream(OutputState state, u32 latency_ms, PlaybackStream::AudioDataRequestCallback callback)
{
    if (auto result = prepare_violet_playback_device(); result.is_error())
        return PlaybackStream::CreatePromise::rejected(result.release_error());
    int fd = ::dup(sound_device);
    if (fd < 0)
        return PlaybackStream::CreatePromise::rejected(Error::from_errno(errno));
    auto stream = make_ref_counted<VioletPlaybackStream>(fd, latency_ms, move(callback));
    if (state == OutputState::Playing)
        stream->start();
    return PlaybackStream::CreatePromise::resolved(NonnullRefPtr<PlaybackStream>(move(stream)));
}

}
