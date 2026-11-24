// Ported to Java from iannitram/save-relay (MIT: third_party/save-relay/LICENSE), revision
// 132b9e57c7a46c596aacbd3d0a56a2579d89b9bc, bindings/android/src/main/kotlin/org/saverelay/SaveRelayHost.kt: what the
// app supplies to save-relay's C interface (its credential store and its network), with the same method names and
// signatures its JNI glue looks up.
package org.saverelay;

public interface SaveRelayHost {
    byte[] credentialGet(String account);

    boolean credentialSet(String account, String secret);

    boolean credentialDelete(String account);

    // The answer as SaveRelayResponse.toWire(), or null for a connection, DNS, TLS or timeout failure.
    byte[] http(String method, String url, String headers, byte[] body, long maxResponseBytes);
}
