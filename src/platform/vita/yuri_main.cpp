#include "tjsCommHead.h"

#include "Application.h"
#include "RenderManager.h"
#include "mofa/engine_tick_pacer.hpp"
#include "mofa/retail_bootstrap.hpp"
#include "mofa/threading_self_test.hpp"
#include "mofa/vita_memory_budget.hpp"
#include "mofa/vita_thread_policy.hpp"
#include "mofa/vitagl_presenter.hpp"
#include "yuri_input.hpp"
#include "yuri_window_layer.hpp"

#include <psp2/kernel/processmgr.h>
#include <psp2/kernel/threadmgr.h>
#include <exception>
#include <cstdint>
#include <string>
#include <thread>

extern int _argc;
extern char** _argv;
extern std::thread::id TVPMainThreadID;
extern bool TVPStartupSuccess;
extern "C" std::uint64_t mofa_yuri_recycled_texture_count();

// VitaSDK otherwise reserves a fixed 128 MiB newlib heap and a 256 KiB main
// stack. ATTRIBUTE2=12 supplies the expanded application budget. Large decoded
// game images use independently reclaimable cached USER_RW memblocks; the
// 80 MiB VitaGL threshold keeps 64 MiB available for them plus 16 MiB for
// non-bitmap allocations outside both fixed pools. The larger stack is a real
// Vita process parameter:
// Kirikiri's nested
// script/render error path exceeded the default stack while reporting an
// allocation failure on hardware.
extern "C" {
unsigned int _newlib_heap_size_user =
    static_cast<unsigned int>(mofa::kVitaNewlibHeapBytes);
unsigned int sceUserMainThreadStackSize = 2u * 1024u * 1024u;
}

namespace {

struct InputShutdownGuard {
    ~InputShutdownGuard() { mofa_yuri_input_shutdown(); }
};

const char* option_value(int argc, char** argv, const char* prefix) {
    const std::string marker(prefix);
    for (int index = 1; index < argc; ++index) {
        if (!argv[index]) continue;
        const std::string argument(argv[index]);
        if (argument.compare(0, marker.size(), marker) == 0)
            return argv[index] + marker.size();
    }
    return nullptr;
}

const char* project_argument(int argc, char** argv) {
    for (int index = argc - 1; index >= 1; --index) {
        if (!argv[index] || argv[index][0] == '-') continue;
        if (std::string(argv[index]).compare(0, 5, "game=") == 0) continue;
        return argv[index];
    }
    return nullptr;
}

} // namespace

int main(int argc, char** argv) {
    mofa_boot_trace("main-entered");
    mofa_boot_trace("vita-newlib-heap-128m");
    mofa_boot_trace("vita-main-thread-stack-2m");
    try {
        mofa::apply_vita_main_thread_policy();
        mofa_boot_trace("vita-main-thread-policy-ready");
        mofa_boot_trace("vita-threading-self-test-entered");
        if (!mofa_vita_threading_self_test()) {
            mofa_report_launch_error(
                "Vita 的 pthread/libstdc++ 同步自检失败。");
            return 1;
        }
        mofa_boot_trace("vita-threading-self-test-passed");

        mofa_resolve_launch(argc, argv);
        mofa_boot_trace("retail-launch-resolved");
        if (mofa_launch_error_reported()) return 1;

        const char* project = project_argument(argc, argv);
        if (!project || !*project) {
            mofa_report_launch_error("没有选中任何游戏工程。");
            return 1;
        }

        _argc = argc;
        _argv = argv;
        TVPMainThreadID = std::this_thread::get_id();
        mofa_yuri_input_initialize(
            option_value(argc, argv, "-krkrprofile="));
        InputShutdownGuard input_shutdown_guard;
        if (!mofa_vitagl_initialize()) return 1;
        mofa_boot_trace("yuri-platform-ready");

        Application->StartApplication(ttstr(project));
        mofa_boot_trace("yuri-start-application-returned");
        if (!TVPStartupSuccess) {
            mofa_report_launch_error(
                "Yuri 没有跑完游戏的启动脚本（启动脚本中途中止）。");
            return 1;
        }

        mofa_boot_trace("yuri-event-loop-entered");
        const std::uint64_t event_loop_started = sceKernelGetProcessTimeWide();
        bool five_second_proof_written = false;
        bool thirty_second_proof_written = false;
        bool texture_recycler_proof_written = false;
        while (!Application->IsTarminate()) {
            const std::uint64_t loop_started = sceKernelGetProcessTimeWide();
            mofa_yuri_input_pump();
            Application->Run();
            mofa_yuri_present_frame();
            const std::uint64_t recycled_before =
                mofa_yuri_recycled_texture_count();
            iTVPTexture2D::RecycleProcess();
            if (!texture_recycler_proof_written &&
                mofa_yuri_recycled_texture_count() > recycled_before) {
                mofa_boot_trace("yuri-texture-recycler-drained");
                texture_recycler_proof_written = true;
            }
            const std::uint64_t elapsed =
                sceKernelGetProcessTimeWide() - event_loop_started;
            if (!five_second_proof_written && elapsed >= 5u * 1000u * 1000u &&
                mofa_vitagl_contentful_frames() > 0) {
                mofa_boot_trace("retail-runtime-5s-stable-with-video");
                five_second_proof_written = true;
            }
            if (!thirty_second_proof_written &&
                elapsed >= 30u * 1000u * 1000u &&
                mofa_vitagl_contentful_frames() > 0) {
                mofa_boot_trace("retail-runtime-30s-stable-with-video");
                thirty_second_proof_written = true;
            }
            // Yuri Android lets the Cocos director impose one 60 Hz deadline
            // over update + draw + swap. Do the same here. VitaGL's swap is
            // deliberately nonblocking so a changed frame does not serialize
            // compositor time with an additional vblank before the next KAG
            // timer can be serviced. A slow frame receives no further delay.
            // This cadence also preserves Yuri's engine-frame-based compressed
            // texture lifetime instead of turning it into a millisecond cache.
            const std::uint32_t delay = mofa::yuri_frame_delay_us(
                loop_started, sceKernelGetProcessTimeWide());
            if (delay != 0) sceKernelDelayThread(delay);
        }
        mofa_boot_trace("yuri-event-loop-exited");
        mofa_yuri_input_shutdown();
        Application->OnExit();
        return 0;
    } catch (const std::exception& error) {
        mofa_report_launch_error(error.what());
    } catch (...) {
        mofa_report_launch_error("Yuri 后端出现未处理的原生异常。");
    }
    return 1;
}
