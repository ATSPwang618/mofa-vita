#pragma once

#include <cstdint>

// Per-frame stage accounting for the Vita event loop.
//
// A 444 MHz Cortex-A9 has no headroom to hide a frame stage that a desktop
// host absorbs, so the loop reports what each stage of one iteration actually
// costs on the device it is running on: the KAG/script engine (which includes
// the software compositor), the compositor itself, the VitaGL upload and
// present, input polling, texture recycling and the idle remainder.
//
// The compositor figure is produced by the generated layer manager, the only
// place that knows when Yuri stops writing the framebuffer, so the split
// between script work and pixel work is measured rather than inferred.
namespace mofa {

// Buckets the engine reports from inside its own frame. The events bucket is
// the message loop (input and window-update delivery), the timer bucket is
// TVPTimer::ProgressAllTimer, which is what KAG's Conductor uses to advance
// the scenario, run keyframes and reveal text, and the tag bucket is the
// native KAG tag parser called from those timer callbacks.
enum VitaStageBucket : int {
    kVitaStageEvents = 0,
    kVitaStageTimer,
    kVitaStageKagParse,
    // Continuous-event delivery: this is what drives KAG's Conductor, so it
    // carries tag dispatch, keyframe evaluation and the layer property writes
    // those callbacks perform.
    kVitaStageContinuous,
    // Reading and splitting a scenario file. The first load of a .ks is the
    // expensive one; later loads of the same storage come from the scenario
    // cache.
    kVitaStageKagLoad,
    // One-pass label cache build for a freshly loaded scenario.
    kVitaStageKagLabels,
    // The game's own onScenarioLoad / onScenarioLoaded callbacks. Their cost is
    // the title's, not ours, so it has to be visible separately.
    kVitaStageKagHooks,
    kVitaStageBucketCount
};

struct VitaFrameStages {
    std::uint64_t loop_us = 0;
    std::uint64_t input_us = 0;
    std::uint64_t engine_us = 0;
    std::uint64_t present_us = 0;
    std::uint64_t recycle_us = 0;
};

// Records one event-loop iteration. Every reporting window the accumulator
// writes a single [mofa-stage] line to the boot trace.
void vita_stage_record_frame(const VitaFrameStages& stages);

} // namespace mofa

// Called from the generated compositor around the layer-tree completion.
extern "C" std::uint64_t mofa_yuri_stage_ticks();
extern "C" void mofa_yuri_stage_composite(std::uint64_t started_at);
// Called from the generated engine around one VitaStageBucket region.
extern "C" void mofa_yuri_stage_bucket(int bucket, std::uint64_t started_at);
// One KAG tag parsed by the native parser.
extern "C" void mofa_yuri_stage_note_tag();
