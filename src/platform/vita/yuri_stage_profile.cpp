#include "mofa/vita_stage_profile.hpp"

#include "mofa/retail_bootstrap.hpp"
#include "mofa/tvpgl_pixel_meter.hpp"

#include <psp2/kernel/processmgr.h>

#include <cstdint>
#include <cstdio>

namespace {

// One line per second at the engine's 60 Hz deadline, matching the presenter's
// existing [mofa-perf] cadence so two builds can be compared window by window.
constexpr std::uint32_t kReportFrames = 60;

std::uint32_t window_frames = 0;
std::uint64_t window_loop_us = 0;
std::uint64_t window_input_us = 0;
std::uint64_t window_engine_us = 0;
std::uint64_t window_present_us = 0;
std::uint64_t window_recycle_us = 0;
std::uint64_t window_composite_us = 0;
std::uint32_t window_composite_calls = 0;
std::uint64_t window_bucket_us[mofa::kVitaStageBucketCount] = {};
std::uint32_t window_tags = 0;

std::uint64_t now_us() {
    return static_cast<std::uint64_t>(sceKernelGetProcessTimeWide());
}

// One line for the frame-stage split and one for the pixel attribution. Both
// are estimated where they have to be: the bucket times are measured, the
// pixel costs come from the per-family probe of the kernels installed on this
// device.
void flush_stage_window() {
    if (window_frames == 0) return;
    const double frames = static_cast<double>(window_frames);
    const double loop_ms = window_loop_us / 1.0e3 / frames;
    const double engine_ms = window_engine_us / 1.0e3 / frames;
    const double composite_ms = window_composite_us / 1.0e3 / frames;
    const double present_ms = window_present_us / 1.0e3 / frames;
    const double input_ms = window_input_us / 1.0e3 / frames;
    const double recycle_ms = window_recycle_us / 1.0e3 / frames;
    // The engine stage owns script dispatch, timers, layer updates and the
    // compositor; subtracting the measured compositor leaves the script and
    // event work a KAG frame really spends on the CPU.
    const double script_ms = engine_ms > composite_ms ? engine_ms - composite_ms : 0.0;
    const double events_ms =
        window_bucket_us[mofa::kVitaStageEvents] / 1.0e3 / frames;
    const double timer_ms =
        window_bucket_us[mofa::kVitaStageTimer] / 1.0e3 / frames;
    const double kag_parse_ms =
        window_bucket_us[mofa::kVitaStageKagParse] / 1.0e3 / frames;
    const double continuous_ms =
        window_bucket_us[mofa::kVitaStageContinuous] / 1.0e3 / frames;
    const double accounted_ms = events_ms + timer_ms + continuous_ms;
    const double rest_ms =
        script_ms > accounted_ms ? script_ms - accounted_ms : 0.0;
    const double busy_ms = input_ms + engine_ms + present_ms + recycle_ms;
    const double idle_ms = loop_ms > busy_ms ? loop_ms - busy_ms : 0.0;
    const double composite_share =
        loop_ms > 0.0 ? 100.0 * composite_ms / loop_ms : 0.0;
    // mofa_boot_trace does not copy the string, so the buffer outlives it.
    static char line[512];
    std::snprintf(
        line, sizeof line,
        "[mofa-stage] frames=%u loop=%.2fms busy=%.2fms engine=%.2fms "
        "script=%.2fms(events=%.2f timer=%.2f kag=%.2f cont=%.2f tags=%u "
        "rest=%.2f) composite=%.2fms(%.0f%%,%u calls) present=%.2fms "
        "input=%.2fms recycle=%.2fms idle=%.2fms",
        window_frames, loop_ms, busy_ms, engine_ms, script_ms, events_ms,
        timer_ms, kag_parse_ms, continuous_ms, window_tags, rest_ms,
        composite_ms, composite_share, window_composite_calls, present_ms,
        input_ms, recycle_ms, idle_ms);
    mofa_boot_trace(line);

    std::uint64_t pixels[mofa::kTvpgPixelFamilyCount] = {};
    std::uint64_t calls[mofa::kTvpgPixelFamilyCount] = {};
    mofa::tvpgl_pixel_meter_take(pixels, calls);
    const mofa::TvpgPixelFamilyCosts& costs = mofa::tvpgl_pixel_costs();
    std::uint64_t estimate_us[mofa::kTvpgPixelFamilyCount] = {};
    std::uint64_t total_estimate_us = 0;
    std::uint64_t total_calls = 0;
    for (int family = 0; family < mofa::kTvpgPixelFamilyCount; ++family) {
        estimate_us[family] =
            mofa::estimate_family_us(pixels[family], costs.ns_per_pixel[family]);
        total_estimate_us += estimate_us[family];
        total_calls += calls[family];
    }
    // Every figure on this line is per frame, so it can be read directly
    // against the stage line's composite/present numbers. The k-suffixed pixel
    // counts are thousands of destination pixels per frame.
    const auto per_frame = [&](std::uint64_t value) {
        return static_cast<unsigned long long>(value / window_frames);
    };
    std::snprintf(
        line, sizeof line,
        "[mofa-pixels] frames=%u blend=%lluk stretch=%lluk add=%lluk "
        "adddest=%lluk sadd=%lluk affine=%lluk copy=%lluk cmap=%lluk calls=%llu "
        "est=%.2fms (blend %.2f stretch %.2f add %.2f adddest %.2f sadd %.2f "
        "affine %.2f copy %.2f cmap %.2f)",
        window_frames, per_frame(pixels[mofa::kTvpgPixelBlend] / 1000),
        per_frame(pixels[mofa::kTvpgPixelStretch] / 1000),
        per_frame(pixels[mofa::kTvpgPixelAdditive] / 1000),
        per_frame(pixels[mofa::kTvpgPixelAdditiveDest] / 1000),
        per_frame(pixels[mofa::kTvpgPixelStretchAdditive] / 1000),
        per_frame(pixels[mofa::kTvpgPixelAffine] / 1000),
        per_frame(pixels[mofa::kTvpgPixelCopyFill] / 1000),
        per_frame(pixels[mofa::kTvpgPixelColorMap] / 1000),
        per_frame(total_calls), total_estimate_us / 1.0e3 / frames,
        estimate_us[mofa::kTvpgPixelBlend] / 1.0e3 / frames,
        estimate_us[mofa::kTvpgPixelStretch] / 1.0e3 / frames,
        estimate_us[mofa::kTvpgPixelAdditive] / 1.0e3 / frames,
        estimate_us[mofa::kTvpgPixelAdditiveDest] / 1.0e3 / frames,
        estimate_us[mofa::kTvpgPixelStretchAdditive] / 1.0e3 / frames,
        estimate_us[mofa::kTvpgPixelAffine] / 1.0e3 / frames,
        estimate_us[mofa::kTvpgPixelCopyFill] / 1.0e3 / frames,
        estimate_us[mofa::kTvpgPixelColorMap] / 1.0e3 / frames);
    mofa_boot_trace(line);

    window_frames = 0;
    window_loop_us = 0;
    window_input_us = 0;
    window_engine_us = 0;
    window_present_us = 0;
    window_recycle_us = 0;
    window_composite_us = 0;
    window_composite_calls = 0;
    for (int bucket = 0; bucket < mofa::kVitaStageBucketCount; ++bucket)
        window_bucket_us[bucket] = 0;
    window_tags = 0;
}

} // namespace

extern "C" std::uint64_t mofa_yuri_stage_ticks() {
    return now_us();
}

extern "C" void mofa_yuri_stage_composite(std::uint64_t started_at) {
    const std::uint64_t finished = now_us();
    if (finished <= started_at) return;
    window_composite_us += finished - started_at;
    ++window_composite_calls;
}

extern "C" void mofa_yuri_stage_bucket(int bucket, std::uint64_t started_at) {
    if (bucket < 0 || bucket >= mofa::kVitaStageBucketCount) return;
    const std::uint64_t finished = now_us();
    if (finished <= started_at) return;
    window_bucket_us[bucket] += finished - started_at;
}

extern "C" void mofa_yuri_stage_note_tag() {
    ++window_tags;
}

namespace mofa {

void vita_stage_record_frame(const VitaFrameStages& stages) {
    window_loop_us += stages.loop_us;
    window_input_us += stages.input_us;
    window_engine_us += stages.engine_us;
    window_present_us += stages.present_us;
    window_recycle_us += stages.recycle_us;
    ++window_frames;
    if (window_frames >= kReportFrames) flush_stage_window();
}

} // namespace mofa
