#include <cstdlib>
// Android host (NativeActivity): memory (mmap at fixed addresses), pthreads, the app's window (ANativeWindow),
// game controllers + touch, AAudio, the Vulkan loader and android surface, a signal crash handler and
// android_main. Game data lives in the app's external files dir (see docs/ANDROID.md):
//   <files>/analysis/{main,sdk}.bin   module images (tools/nso2bin.py)
//   <files>/extract/romfs/...         the RomFS
//   <files>/user/...                  saves
//   <files>/config.txt                optional: resolution=WxH
#include <aaudio/AAudio.h>
#include <android/log.h>
#include <android/native_window.h>
#include <android/window.h>
#include <android_native_app_glue.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <jni.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cerrno>
#include <cmath>
#include <condition_variable>
#include <cstring>
#include <deque>
#include <filesystem>
#include <fstream>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#define VK_NO_PROTOTYPES
#define VK_USE_PLATFORM_ANDROID_KHR
#include <vulkan/vulkan.h>

#include <nx/game.h>
#include <nx/runtime.h>

#include "kernel.h"
#include "platform.h"
#include "platform_android.h"

// Android audio out: AAudio (split from platform_android.cpp).
namespace nx::plat {

// ---- audio: AAudio (callback pulls from a queue of submitted buffers) ------------------------------------------

namespace {
struct Pending {
    const uint8_t* data;
    size_t size, pos;
    void* ctx;
};
std::mutex g_audio;
std::deque<Pending> g_pending;
AudioDone g_done = nullptr;
std::atomic<AAudioStream*> g_stream{nullptr};
std::mutex g_stream_lock;  // opening, closing, pausing the stream
bool g_paused = false;     // under g_stream_lock

// Dedicated reopen worker: AAudio forbids closing a stream from its own error callback, and audio_shutdown joins
// this thread cleanly instead of detaching it.
std::mutex g_reopen_mu;
std::condition_variable g_reopen_cv;
AAudioStream* g_reopen_target = nullptr;
bool g_shutting_down = false;
bool g_worker_active = false;  // the worker thread runs (not with NX_MUTE or headless)
bool g_retry = false;          // a caller asked for another go at opening a stream that is gone
std::thread g_reopen_thread;
std::atomic<int64_t> g_last_retry_ms{0};  // when a submit last asked (rate limit)
constexpr int64_t kRetryEveryMs = 1000;

int64_t now_ms() {
    return std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::steady_clock::now().time_since_epoch())
        .count();
}

aaudio_data_callback_result_t audio_cb(AAudioStream*, void*, void* out, int32_t frames) {
    auto* dst = static_cast<uint8_t*>(out);
    size_t want = size_t(frames) * 4;
    std::vector<void*> finished;
    {
        std::lock_guard lk(g_audio);
        while (want && !g_pending.empty()) {
            Pending& p = g_pending.front();
            size_t n = std::min(want, p.size - p.pos);
            std::memcpy(dst, p.data + p.pos, n);
            dst += n;
            want -= n;
            p.pos += n;
            if (p.pos == p.size) {
                finished.push_back(p.ctx);
                g_pending.pop_front();
            }
        }
    }
    if (want) std::memset(dst, 0, want);  // underrun: silence
    for (void* c : finished) g_done(c);
    return AAUDIO_CALLBACK_RESULT_CONTINUE;
}

void audio_error(AAudioStream* s, void*, aaudio_result_t error);

// A stream on the system's current default output (no device id: Android picks the speaker, wired or Bluetooth
// headphones, a USB device such as a Razer Kishi's audio). Under g_stream_lock.
AAudioStream* open_stream() {
    AAudioStreamBuilder* b = nullptr;
    if (AAudio_createStreamBuilder(&b) != AAUDIO_OK) return nullptr;
    AAudioStreamBuilder_setFormat(b, AAUDIO_FORMAT_PCM_I16);
    AAudioStreamBuilder_setChannelCount(b, 2);
    AAudioStreamBuilder_setSampleRate(b, 48000);
    AAudioStreamBuilder_setPerformanceMode(b, AAUDIO_PERFORMANCE_MODE_LOW_LATENCY);
    AAudioStreamBuilder_setDataCallback(b, audio_cb, nullptr);
    AAudioStreamBuilder_setErrorCallback(b, audio_error, nullptr);
    AAudioStream* s = nullptr;
    bool ok = AAudioStreamBuilder_openStream(b, &s) == AAUDIO_OK && (g_paused || AAudioStream_requestStart(s) == AAUDIO_OK);
    AAudioStreamBuilder_delete(b);
    if (!ok && s) AAudioStream_close(s), s = nullptr;
    if (s)
        log("audio: stream open on device %d, %d Hz, %s", AAudioStream_getDeviceId(s), AAudioStream_getSampleRate(s),
            g_paused ? "paused" : "playing");
    return s;
}

// The output changed (headphones or a USB device plugged in or out, Bluetooth connected): Android disconnects the
// stream, which then stays silent for good. Reopen on the new default output from the dedicated worker thread
// (AAudio forbids closing a stream in its own callback); the queued buffers carry on into the new stream.
bool shutting_down() {
    std::lock_guard lk(g_reopen_mu);
    return g_shutting_down;
}

void reopen_loop() {
    for (;;) {
        AAudioStream* target = nullptr;
        {
            std::unique_lock lk(g_reopen_mu);
            g_reopen_cv.wait(lk, [] { return g_shutting_down || g_reopen_target != nullptr || g_retry; });
            if (g_shutting_down) return;
            target = g_reopen_target;
            g_reopen_target = nullptr;
            g_retry = false;
        }

        if (target) {
            std::lock_guard lk(g_stream_lock);
            if (g_stream.load() != target) continue;  // already replaced or closed
            log("audio: output changed: opening the stream again");
            AAudioStream_requestStop(target);
            AAudioStream_close(target);
            g_stream = nullptr;
        }

        for (int attempt = 0; attempt < 5; attempt++) {
            if (attempt) {
                std::unique_lock lk(g_reopen_mu);
                if (g_reopen_cv.wait_for(lk, std::chrono::milliseconds(200 * attempt),
                                         [] { return g_shutting_down; })) {
                    return;
                }
            }
            std::lock_guard lk(g_stream_lock);
            if (shutting_down()) return;
            if (g_stream.load()) break;  // already reopened
            if (AAudioStream* next = open_stream()) {
                g_stream = next;
                break;
            }
        }
        if (!g_stream.load() && !shutting_down()) log("audio: no output after the change");
    }
}

// The worker gave up (or the first open failed) and the stream is still gone: ask it for another go. From
// audio_pause(false) at once (the app is back in front: the output is likely there again), from audio_submit at most
// once a second (it is called for every buffer). Never opens here: the caller may be the game's audio thread, and the
// worker takes g_stream_lock, honours the pause and the shutdown. Does nothing without the worker (NX_MUTE, headless).
void request_retry(bool rate_limited) {
    if (rate_limited) {
        const int64_t now = now_ms();
        int64_t last = g_last_retry_ms.load();
        if (now - last < kRetryEveryMs || !g_last_retry_ms.compare_exchange_strong(last, now)) return;
    }
    {
        std::lock_guard lk(g_reopen_mu);
        if (g_shutting_down || !g_worker_active) return;
        g_retry = true;
    }
    g_reopen_cv.notify_one();
}

void audio_error(AAudioStream* s, void*, aaudio_result_t error) {
    if (error != AAUDIO_ERROR_DISCONNECTED) {
        log("audio: stream error %d", int(error));
        return;
    }
    {
        std::lock_guard lk(g_reopen_mu);
        if (g_shutting_down) return;
        g_reopen_target = s;
    }
    g_reopen_cv.notify_one();
}
}  // namespace

bool audio_open(AudioDone done, int) {
    g_done = done;
    // NX_MUTE=1 (and headless test runs): no device, the callers pace themselves with a timer (as on Windows and macOS).
    if (headless() || std::getenv("NX_MUTE")) return false;
    {
        std::lock_guard lk(g_reopen_mu);
        g_shutting_down = false;
        g_reopen_target = nullptr;
        g_retry = false;
        g_worker_active = true;
        if (!g_reopen_thread.joinable()) g_reopen_thread = std::thread(reopen_loop);
    }
    std::lock_guard lk(g_stream_lock);
    g_stream = open_stream();
    return g_stream.load() != nullptr;
}

void audio_shutdown() {
    std::thread to_join;
    {
        std::lock_guard lk(g_reopen_mu);
        g_shutting_down = true;
        g_worker_active = false;
        g_reopen_target = nullptr;
        to_join = std::move(g_reopen_thread);
    }
    g_reopen_cv.notify_all();
    if (to_join.joinable()) to_join.join();

    std::lock_guard lk(g_stream_lock);
    if (AAudioStream* s = g_stream.exchange(nullptr)) {
        AAudioStream_requestStop(s);
        AAudioStream_close(s);
    }
}

void audio_pause(bool paused) {
    std::lock_guard lk(g_stream_lock);
    g_paused = paused;
    AAudioStream* s = g_stream.load();
    if (!s) {
        if (!paused) request_retry(false);  // coming back with no stream: try the output again
        return;
    }
    if (paused)
        AAudioStream_requestPause(s);
    else
        AAudioStream_requestStart(s);
}

bool audio_submit(const void* data, size_t bytes, void* ctx) {
    if (!g_stream.load()) {  // between a disconnect and the new stream, or after the worker gave up: the caller paces itself
        request_retry(true);
        return false;
    }
    std::lock_guard lk(g_audio);
    g_pending.push_back({static_cast<const uint8_t*>(data), bytes, 0, ctx});
    return true;
}

uint32_t audio_queued() {
    std::lock_guard lk(g_audio);
    return uint32_t(g_pending.size());
}

}  // namespace nx::plat
