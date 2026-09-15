#include "mofa/threading_self_test.hpp"

#include "ThreadIntf.h"
#include "ThreadImpl.h"

#include <chrono>
#include <condition_variable>
#include <mutex>
#include <thread>

namespace {

const char* g_reason = nullptr;

bool fail(const char* reason) {
    g_reason = reason;
    return false;
}

class SuspendedThreadProbe final : public tTVPThread {
public:
    SuspendedThreadProbe(std::mutex& mutex, std::condition_variable& condition,
                         bool& ran)
        : tTVPThread(true), mutex_(mutex), condition_(condition), ran_(ran) {}

protected:
    void Execute() override {
        std::lock_guard<std::mutex> lock(mutex_);
        ran_ = true;
        condition_.notify_one();
    }

private:
    std::mutex& mutex_;
    std::condition_variable& condition_;
    bool& ran_;
};

} // namespace

const char* mofa_vita_threading_self_test_reason() { return g_reason; }

bool mofa_vita_threading_self_test() {
    constexpr int kRounds = 128;
    // Vita3K emulates the scheduler, so a round can take far longer than on
    // hardware.  The contract is "the handshake completes", not "it completes
    // within a hardware-sized budget".
    constexpr auto kTimeout = std::chrono::seconds(10);
    constexpr int kEventTimeoutMs = 4000;

    g_reason = nullptr;
    std::mutex mutex;
    std::condition_variable condition;

    // An emulator can starve the freshly created worker for the whole first
    // attempt while the main thread runs the handshake, which says nothing
    // about the platform's synchronization.  Retry with a fresh thread, and
    // only a repeated failure is treated as a real sync problem.
    constexpr int kHandshakeAttempts = 4;
    int acknowledged = 0;
    for (int attempt = 0; attempt < kHandshakeAttempts; ++attempt) {
        acknowledged = 0;
        int offered = 0;
        bool stop = false;
        bool passed = true;

        std::thread worker([&] {
            std::unique_lock<std::mutex> lock(mutex);
            while (!stop) {
                if (!condition.wait_for(lock, kTimeout, [&] {
                        return stop || offered > acknowledged;
                    })) {
                    passed = false;
                    stop = true;
                    condition.notify_one();
                    break;
                }
                if (stop) break;
                acknowledged = offered;
                condition.notify_one();
            }
        });

        {
            std::unique_lock<std::mutex> lock(mutex);
            for (int round = 1; round <= kRounds; ++round) {
                offered = round;
                condition.notify_one();
                if (!condition.wait_for(lock, kTimeout, [&] {
                        return stop || acknowledged == round;
                    }) || stop) {
                    passed = false;
                    break;
                }
            }
            stop = true;
            condition.notify_one();
        }

        worker.join();
        if (passed && acknowledged == kRounds) break;
    }
    if (acknowledged != kRounds)
        return fail("libstdc++ mutex/condition_variable 握手未完成");

    bool passed = true;

    // Verify Yuri's own auto-reset event, including the signal-before-wait
    // case that its old bare-condition-variable implementation lost.
    // A correct auto-reset event must consume the earlier Set(); a lost
    // signal shows up as a timeout, not as latency.
    tTVPThreadEvent event;
    event.Set();
    const auto started = std::chrono::steady_clock::now();
    event.WaitFor(kEventTimeoutMs);
    const auto elapsed = std::chrono::steady_clock::now() - started;
    if (elapsed >= std::chrono::milliseconds(kEventTimeoutMs))
        return fail("tTVPThreadEvent 丢失了先 Set 后 Wait 的信号（等待超时）");

    bool yuri_wait_finished = false;
    std::thread yuri_waiter([&] {
        event.WaitFor(kEventTimeoutMs);
        std::lock_guard<std::mutex> lock(mutex);
        yuri_wait_finished = true;
        condition.notify_one();
    });
    event.Set();
    {
        std::unique_lock<std::mutex> lock(mutex);
        passed = condition.wait_for(lock, kTimeout,
                                    [&] { return yuri_wait_finished; });
    }
    if (!passed) event.Set();
    yuri_waiter.join();
    if (!passed)
        return fail("tTVPThreadEvent 唤醒等待线程失败");

    // KAG timers are suspended tTVPThreads resumed immediately after their
    // constructor. Exercise that exact signal-before-wait race repeatedly.
    // A second Resume is used only to recover a failed probe for a clean join.
    constexpr int kSuspendedThreadRounds = 32;
    for (int round = 0; round < kSuspendedThreadRounds; ++round) {
        bool ran = false;
        SuspendedThreadProbe probe(mutex, condition, ran);
        probe.Resume();
        {
            std::unique_lock<std::mutex> lock(mutex);
            passed = condition.wait_for(lock, kTimeout, [&] { return ran; });
        }
        if (!passed) probe.Resume();
        probe.WaitFor();
        if (!passed)
            return fail("被挂起的 tTVPThread 在 Resume 后没有运行");
    }
    return true;
}
