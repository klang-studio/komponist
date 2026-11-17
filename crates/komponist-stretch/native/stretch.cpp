// The ABI is private to this crate. All processing, construction and destruction are worker-only.
#include "signalsmith-stretch.h"
#include <array>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <new>

namespace {
constexpr std::size_t chunk = 512;
using Engine = signalsmith::stretch::SignalsmithStretch<float>;
struct Stretch {
    std::uint32_t rate, channels;
    std::unique_ptr<Engine> engine;
    std::array<std::array<float, chunk>, 2> discard{};
    Stretch(std::uint32_t rate, std::uint32_t channels) : rate(rate), channels(channels) {
        reset();
    }
    void reset() {
        // A fresh, fixed seed also resets RNG state, which upstream reset() alone does not do.
        auto next = std::make_unique<Engine>(0);
        next->presetDefault(int(channels), float(rate), false);
        engine.swap(next);
    }
};
// Never allow a C++ exception to unwind through Rust. No callbacks into Rust occur here.
template <class F> int guarded(F &&f) noexcept {
    try {
        f();
        return 0;
    } catch (const std::bad_alloc &) {
        return 1;
    } catch (...) {
        return 2;
    }
}
} // namespace
extern "C" {
int kp_stretch_new(std::uint32_t rate, std::uint32_t channels, void **out,
                   std::size_t *input_latency, std::size_t *output_latency) noexcept {
    if (!out || !input_latency || !output_latency || rate < 8000 || rate > 192000 || channels < 1 ||
        channels > 2)
        return 3;
    *out = nullptr;
    return guarded([&] {
        auto s = std::make_unique<Stretch>(rate, channels);
        *input_latency = std::size_t(s->engine->inputLatency());
        *output_latency = std::size_t(s->engine->outputLatency());
        *out = s.release();
    });
}
int kp_stretch_reset(void *opaque) noexcept {
    if (!opaque)
        return 3;
    return guarded([&] { static_cast<Stretch *>(opaque)->reset(); });
}
int kp_stretch_prime(void *opaque, const float *const *input, std::size_t frames,
                     double ratio) noexcept {
    if (!opaque || !input || !std::isfinite(ratio) || ratio < 0.5 || ratio > 2)
        return 3;
    auto &s = *static_cast<Stretch *>(opaque);
    const auto lookahead = std::size_t(s.engine->inputLatency());
    const auto delay = std::size_t(s.engine->outputLatency());
    if (frames != lookahead + std::size_t(std::llround(double(delay) / ratio)))
        return 3;
    for (std::uint32_t c = 0; c < s.channels; ++c)
        if (!input[c])
            return 3;
    return guarded([&] {
        s.engine->seek(input, int(lookahead), 1 / ratio);
        std::size_t produced = 0;
        std::array<const float *, 2> in{};
        std::array<float *, 2> out{s.discard[0].data(), s.discard[1].data()};
        while (produced < delay) {
            auto n = std::min(chunk, delay - produced);
            auto before = std::size_t(std::llround(double(produced) / ratio));
            auto after = std::size_t(std::llround(double(produced + n) / ratio));
            for (std::uint32_t c = 0; c < s.channels; ++c)
                in[c] = input[c] + lookahead + before;
            s.engine->process(in.data(), int(after - before), out.data(), int(n));
            produced += n;
        }
    });
}
int kp_stretch_process(void *opaque, const float *const *input, std::size_t input_frames,
                       float *const *output, std::size_t output_frames) noexcept {
    if (!opaque || !input || !output || input_frames > chunk * 2 || !output_frames ||
        output_frames > chunk)
        return 3;
    auto &s = *static_cast<Stretch *>(opaque);
    for (std::uint32_t c = 0; c < s.channels; ++c)
        if (!input[c] || !output[c])
            return 3;
    return guarded(
        [&] { s.engine->process(input, int(input_frames), output, int(output_frames)); });
}
void kp_stretch_delete(void *opaque) noexcept { delete static_cast<Stretch *>(opaque); }
}
