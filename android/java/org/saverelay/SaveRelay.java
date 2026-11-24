// Ported to Java from iannitram/save-relay (MIT: third_party/save-relay/LICENSE), revision
// 132b9e57c7a46c596aacbd3d0a56a2579d89b9bc, bindings/android/src/main/kotlin/org/saverelay/SaveRelay.kt: the same class,
// methods and natives (third_party/save-relay/save_relay_jni.c), so the JNI names match. The app's APK is built with
// javac and d8 only (tools/build_android.sh), hence Java. One change: the natives are in the app's own library
// (lib<game>.so links save-relay's C interface through crates/runtime), so that is the one loaded.
package org.saverelay;

public final class SaveRelay implements AutoCloseable {
    static {
        System.loadLibrary("acnh");
    }

    public final String initialized;
    private boolean closed;

    public SaveRelay(String home, String data, String config, String documents, SaveRelayHost host) {
        initialized = take(nativeInit(home, data, config, documents, host));
    }

    public String pull(String manifestJson, String savesDir, String resolve) {
        return take(nativePull(manifestJson, savesDir, resolve));
    }

    public String push(String manifestJson, String savesDir, String resolve) {
        return take(nativePush(manifestJson, savesDir, resolve));
    }

    public String status(String manifestJson, String savesDir) {
        return take(nativeStatus(manifestJson, savesDir));
    }

    public String setup(String server, String user, String password, String device, String root) {
        return take(nativeSetup(server, user, password, device, root));
    }

    public String signInStart(String service, String redirectUri, String device, String root) {
        return take(nativeSignInStart(service, redirectUri, device, root));
    }

    public String signInFinish(String redirectUrl) {
        return take(nativeSignInFinish(redirectUrl));
    }

    @Override
    public void close() {
        if (!closed) {
            closed = true;
            nativeShutdown();
        }
    }

    private String take(long pointer) {
        if (pointer == 0L) return "";
        try {
            return nativeRead(pointer);
        } finally {
            nativeFree(pointer);
        }
    }

    private native long nativeInit(String home, String data, String config, String documents, SaveRelayHost host);

    private native String nativeRead(long pointer);

    private native void nativeFree(long pointer);

    private native void nativeShutdown();

    private native long nativePull(String manifestJson, String savesDir, String resolve);

    private native long nativePush(String manifestJson, String savesDir, String resolve);

    private native long nativeStatus(String manifestJson, String savesDir);

    private native long nativeSetup(String server, String user, String password, String device, String root);

    private native long nativeSignInStart(String service, String redirectUri, String device, String root);

    private native long nativeSignInFinish(String redirectUrl);
}
