// Hub pairing and prepared-game pull in the Android app (issue #80): the JNI side of crates/runtime's
// hub_android.rs. HubPull.java calls these natives to discover hubs, pair with a six-digit code, and pull the
// prepared game library and data into <files>/game/ through appgame::install.
#include <jni.h>

#include <cstdint>
#include <string>

// crates/runtime/src/hub_android.rs
extern "C" {
char* acnh_hub_android_discover(uint32_t timeout_ms);
char* acnh_hub_android_paired(const char* files);
char* acnh_hub_android_pair(const char* files, const char* addr, const char* hub_name, const char* code);
char* acnh_hub_android_pull(const char* files, const char* addr, const char* hub_name, uint64_t free_bytes);
void acnh_hub_android_free(char* ptr);
}

namespace {

std::string jstr(JNIEnv* env, jstring s) {
    if (!s) return {};
    const char* cs = env->GetStringUTFChars(s, nullptr);
    if (!cs) return {};
    std::string out(cs);
    env->ReleaseStringUTFChars(s, cs);
    return out;
}

jstring take_c_string(JNIEnv* env, char* ptr) {
    if (!ptr) return env->NewStringUTF("");
    jstring out = env->NewStringUTF(ptr);
    acnh_hub_android_free(ptr);
    return out;
}

}  // namespace

extern "C" JNIEXPORT jstring JNICALL Java_io_github_acnhrecomp_HubPull_nativeDiscover(JNIEnv* env, jclass,
                                                                                    jint timeout_ms) {
    uint32_t ms = timeout_ms > 0 ? static_cast<uint32_t>(timeout_ms) : 3000u;
    return take_c_string(env, acnh_hub_android_discover(ms));
}

extern "C" JNIEXPORT jstring JNICALL Java_io_github_acnhrecomp_HubPull_nativePaired(JNIEnv* env, jclass,
                                                                                  jstring files) {
    return take_c_string(env, acnh_hub_android_paired(jstr(env, files).c_str()));
}

extern "C" JNIEXPORT jstring JNICALL Java_io_github_acnhrecomp_HubPull_nativePair(JNIEnv* env, jclass, jstring files,
                                                                                jstring addr, jstring hub_name,
                                                                                jstring code) {
    return take_c_string(
        env, acnh_hub_android_pair(jstr(env, files).c_str(), jstr(env, addr).c_str(),
                                   jstr(env, hub_name).c_str(), jstr(env, code).c_str()));
}

extern "C" JNIEXPORT jstring JNICALL Java_io_github_acnhrecomp_HubPull_nativePull(JNIEnv* env, jclass, jstring files,
                                                                                jstring addr, jstring hub_name,
                                                                                jlong free_bytes) {
    uint64_t bytes = free_bytes > 0 ? static_cast<uint64_t>(free_bytes) : 0u;
    return take_c_string(env, acnh_hub_android_pull(jstr(env, files).c_str(), jstr(env, addr).c_str(),
                                                    jstr(env, hub_name).c_str(), bytes));
}
