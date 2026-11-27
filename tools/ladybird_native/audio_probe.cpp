// Exercise the upstream AudioServer mixer, its shared float ring, and the real device backend.
#include <AK/Array.h>
#include <AudioServer/PlatformAudio.h>
#include <AudioServer/PlaybackStreamMixer.h>
#include <LibCore/EventLoop.h>
#include <LibCore/Timer.h>
#include <math.h>
#include <stdio.h>

int main()
{
    auto& loop = Core::EventLoop::initialize_for_current_thread();
    auto mixer = Audio::PlaybackStreamMixer::create(loop, 15, Audio::create_platform_playback_stream);
    auto spec = mixer->device_sample_specification();
    if (!spec.has_value() || spec->sample_rate() != 48000 || spec->channel_count() != 2)
        return 1;
    auto ring = MUST(Audio::SharedAudioFrameRing::create(48000, 2, 65536));
    auto consumer = MUST(Audio::SharedAudioFrameRing::create(ring.buffer()));
    Audio::PlaybackGroupId group { 1 };
    mixer->set_group_authorization(group, Audio::PlaybackGroupAuthorization::Authorized);
    mixer->add_client(1, move(consumer), 1.f, group);
    Array<float, 960> signal;
    for (unsigned chunk = 0; chunk < 100; chunk++) {
        for (unsigned i = 0; i < 480; i++) {
            auto sample = static_cast<float>(sin((chunk * 480 + i) * 440. * 2. * 3.141592653589793 / 48000.) * .5);
            signal[i*2] = signal[i*2+1] = sample;
        }
        if (ring.try_push(signal.span()) != 480)
            return 2;
    }
    bool drained = false;
    mixer->resume_client(1);
    mixer->drain_client(1, [&] { drained = true; });
    auto watchdog = Core::Timer::create_single_shot(AK::Duration::from_seconds(10), [&] { loop.quit(3); });
    watchdog->start();
    loop.spin_until([&] { return drained || loop.was_exit_requested(); });
    if (!drained || ring.frames_available() != 0)
        return 3;
    puts("upstream AudioServer mixed and drained 48000 frames");
    fflush(stdout);
    return 0;
}
