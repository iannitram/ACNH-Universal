// Save sync in the Android app (issue #81): the JNI side of crates/runtime's relay_android.rs. The home screen
// (android/java/.../Relay.java) calls the Rust loop through the natives here, and the loop calls back for its log
// lines, the relay.ask setting, the home screen's dialogs (Relay.ask) and the network (the Java host's http, the one
// save-relay's own JNI glue, third_party/save-relay/save_relay_jni.c, uses for the sign-in). No secret, account or
// path is ever logged here: the requests and answers that cross are passed on, never printed.
#include <android/log.h>
#include <jni.h>

#include <cstdlib>
#include <cstring>
#include <mutex>
#include <string>

#include <nx/game.h>
#include <nx/runtime.h>

#include "config.h"
#include "platform.h"

namespace nx::plat {
void android_set_data_root(const std::string& root);  // platform_android.cpp
}

// crates/runtime/src/relay_android.rs
extern "C" {
void acnh_relay_android_before_game(const char* data_root, const char* files);
int acnh_relay_android_state(const char* files);
bool acnh_relay_android_forget(const char* files);
int acnh_relay_android_cred_get(const char* files, const char* account, uint8_t* buf, size_t cap, size_t* len);
int acnh_relay_android_cred_set(const char* files, const char* account, const char* secret);
int acnh_relay_android_cred_delete(const char* files, const char* account);
}

namespace {
JavaVM* g_vm = nullptr;
jclass g_relay = nullptr;       // io.github.acnhrecomp.Relay (a global ref: FindClass fails on native threads)
jmethodID g_ask = nullptr;      // static String ask(String)
jobject g_host = nullptr;       // the app's org.saverelay.SaveRelayHost
jmethodID g_http = nullptr;     // byte[] http(String, String, String, byte[], long)
std::mutex g_lock;

// This thread's JNIEnv, attached for the scope when it was not (the background push runs on a Rust thread).
struct Env {
    JNIEnv* env = nullptr;
    bool attached = false;
    Env() {
        if (!g_vm) return;
        if (g_vm->GetEnv(reinterpret_cast<void**>(&env), JNI_VERSION_1_6) == JNI_OK) return;
        env = nullptr;
        if (g_vm->AttachCurrentThread(&env, nullptr) == JNI_OK) attached = true;
        else env = nullptr;
    }
    ~Env() {
        if (attached) g_vm->DetachCurrentThread();
    }
};

std::string str(JNIEnv* env, jstring js) {
    std::string out;
    if (const char* cs = js ? env->GetStringUTFChars(js, nullptr) : nullptr) out = cs, env->ReleaseStringUTFChars(js, cs);
    return out;
}

void remember(JNIEnv* env, jclass cls) {
    std::lock_guard lk(g_lock);
    if (!g_vm) env->GetJavaVM(&g_vm);
    if (!g_relay) {
        g_relay = static_cast<jclass>(env->NewGlobalRef(cls));
        g_ask = env->GetStaticMethodID(g_relay, "ask", "(Ljava/lang/String;)Ljava/lang/String;");
        if (!g_ask) env->ExceptionClear();
    }
}

std::string g_data_root;  // the game folder of the last pull, for the log and relay.ask
}  // namespace

// ---- the calls back from Rust ---------------------------------------------------------------------------------

extern "C" void nx_relay_log(const char* line) {
    if (g_data_root.empty()) __android_log_write(ANDROID_LOG_INFO, nx::game::kInfo.id, line);
    else nx::log("%s", line);
}

// relay.ask lives in the game folder's config.ini, which the C++ side alone writes (config_save rewrites the file from
// what it loaded): loaded fresh from the game folder of this pull first, so a key written meanwhile is kept.
extern "C" bool nx_relay_ask_off() {
    nx::config_load();
    return nx::config_get("relay.ask", "") == "0";
}
extern "C" bool nx_relay_remember_not_now() {
    nx::config_load();
    nx::config_set("relay.ask", "0");
    return nx::config_save();
}

extern "C" char* nx_relay_ask(const char* request) {
    Env e;
    if (!e.env || !g_relay || !g_ask) return nullptr;
    JNIEnv* env = e.env;
    jstring jrequest = env->NewStringUTF(request);
    auto answer = static_cast<jstring>(env->CallStaticObjectMethod(g_relay, g_ask, jrequest));
    env->DeleteLocalRef(jrequest);
    if (env->ExceptionCheck()) {
        env->ExceptionClear();
        return nullptr;
    }
    if (!answer) return nullptr;
    std::string text = str(env, answer);
    env->DeleteLocalRef(answer);
    return strdup(text.c_str());
}

// The answer in save-relay's wire format (SaveRelayResponse.toWire), malloc'ed into *out.
extern "C" int nx_relay_http(const char* method, const char* url, const char* headers, const uint8_t* body,
                             size_t body_len, size_t max_response_bytes, uint8_t** out, size_t* out_len) {
    *out = nullptr;
    *out_len = 0;
    Env e;
    if (!e.env || !g_host || !g_http) return 1;
    JNIEnv* env = e.env;
    jstring jmethod = env->NewStringUTF(method), jurl = env->NewStringUTF(url), jheaders = env->NewStringUTF(headers);
    jbyteArray jbody = env->NewByteArray(jsize(body_len));
    if (jbody && body_len) env->SetByteArrayRegion(jbody, 0, jsize(body_len), reinterpret_cast<const jbyte*>(body));
    int result = 1;
    if (jmethod && jurl && jheaders && jbody && !env->ExceptionCheck()) {
        auto wire = static_cast<jbyteArray>(
            env->CallObjectMethod(g_host, g_http, jmethod, jurl, jheaders, jbody, jlong(max_response_bytes)));
        if (env->ExceptionCheck()) {
            env->ExceptionClear();
        } else if (wire) {
            jsize n = env->GetArrayLength(wire);
            if (n >= 8 && (*out = static_cast<uint8_t*>(malloc(size_t(n))))) {
                env->GetByteArrayRegion(wire, 0, n, reinterpret_cast<jbyte*>(*out));
                *out_len = size_t(n);
                result = 0;
            }
            env->DeleteLocalRef(wire);
        }
    }
    if (env->ExceptionCheck()) env->ExceptionClear();
    for (jobject o : {static_cast<jobject>(jmethod), static_cast<jobject>(jurl), static_cast<jobject>(jheaders),
                      static_cast<jobject>(jbody)})
        if (o) env->DeleteLocalRef(o);
    return result;
}

// ---- the natives of io.github.acnhrecomp.Relay ----------------------------------------------------------------

// The app's save-relay host (its network and credential calls), kept for the loop's network.
extern "C" JNIEXPORT void JNICALL Java_io_github_acnhrecomp_Relay_nativeAttach(JNIEnv* env, jclass cls, jobject host) {
    remember(env, cls);
    std::lock_guard lk(g_lock);
    if (g_host) env->DeleteGlobalRef(g_host), g_host = nullptr;
    if (!host) return;
    g_host = env->NewGlobalRef(host);
    g_http = env->GetMethodID(env->GetObjectClass(host), "http",
                              "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;[BJ)[B");
    if (!g_http) env->ExceptionClear();
}

// Blocks (on the home screen's worker thread) until the game may start; the dialogs come through Relay.ask.
extern "C" JNIEXPORT void JNICALL Java_io_github_acnhrecomp_Relay_nativeBeforeGame(JNIEnv* env, jclass cls,
                                                                                  jstring data_root, jstring files) {
    remember(env, cls);
    g_data_root = str(env, data_root);
    nx::plat::android_set_data_root(g_data_root);  // config.ini and nx.log of this game folder
    acnh_relay_android_before_game(g_data_root.c_str(), str(env, files).c_str());
}

extern "C" JNIEXPORT jint JNICALL Java_io_github_acnhrecomp_Relay_nativeState(JNIEnv* env, jclass, jstring files) {
    return acnh_relay_android_state(str(env, files).c_str());
}

extern "C" JNIEXPORT jboolean JNICALL Java_io_github_acnhrecomp_Relay_nativeForget(JNIEnv* env, jclass, jstring files) {
    return acnh_relay_android_forget(str(env, files).c_str());
}

extern "C" JNIEXPORT jbyteArray JNICALL Java_io_github_acnhrecomp_Relay_nativeCredGet(JNIEnv* env, jclass, jstring files,
                                                                                     jstring account) {
    uint8_t buf[16 * 1024];
    size_t len = 0;
    int got = acnh_relay_android_cred_get(str(env, files).c_str(), str(env, account).c_str(), buf, sizeof buf, &len);
    if (got != 0) {
        std::memset(buf, 0, sizeof buf);
        if (got == 1) return nullptr;
        jclass io = env->FindClass("java/io/IOException");
        if (io) env->ThrowNew(io, "the credential store failed");
        return nullptr;
    }
    jbyteArray out = env->NewByteArray(jsize(len));
    if (out) env->SetByteArrayRegion(out, 0, jsize(len), reinterpret_cast<const jbyte*>(buf));
    std::memset(buf, 0, sizeof buf);
    return out;
}

extern "C" JNIEXPORT jboolean JNICALL Java_io_github_acnhrecomp_Relay_nativeCredSet(JNIEnv* env, jclass, jstring files,
                                                                                   jstring account, jstring secret) {
    return acnh_relay_android_cred_set(str(env, files).c_str(), str(env, account).c_str(), str(env, secret).c_str()) == 0;
}

extern "C" JNIEXPORT jboolean JNICALL Java_io_github_acnhrecomp_Relay_nativeCredDelete(JNIEnv* env, jclass,
                                                                                      jstring files, jstring account) {
    return acnh_relay_android_cred_delete(str(env, files).c_str(), str(env, account).c_str()) == 0;
}
