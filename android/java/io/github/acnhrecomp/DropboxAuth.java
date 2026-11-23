// Dropbox sign-in for the in-app game download (issue #230; SPEC M18 and the Dropbox exception). It reuses
// save-relay's Dropbox app (owner's choice, 2026-11-23: "Reuse Save Relay's app"): an App folder app, so the token
// reaches only that app's own folder, Apps/<app>/, where the prepared game's files wait in
// acnh-universal-game/android/. OAuth 2 with PKCE, no client secret; the browser comes back to the redirect
// save-relay registered, http://127.0.0.1:53710/, which a small server in this app answers. The refresh token is
// kept in the app's private preferences; nothing of the sign-in leaves the device but the calls to Dropbox.
package io.github.acnhrecomp;

import android.content.Context;
import android.content.SharedPreferences;
import android.util.Base64;

import org.json.JSONObject;

import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.net.URL;
import java.net.URLDecoder;
import java.net.URLEncoder;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.SecureRandom;

final class DropboxAuth {
    // save-relay's Dropbox app key (save-relay src/clients.rs: a public PKCE client, App folder access).
    static final String APP_KEY = "p8xhxihp8s3gb33";
    static final String REDIRECT = "http://127.0.0.1:53710/";
    private static final int PORT = 53710;
    private static final String PREFS = "dropbox";

    private DropboxAuth() {}

    private static SharedPreferences prefs(Context c) { return c.getSharedPreferences(PREFS, Context.MODE_PRIVATE); }

    static boolean signedIn(Context c) { return !prefs(c).getString("refresh", "").isEmpty(); }

    static void signOut(Context c) { prefs(c).edit().clear().apply(); }

    private static String b64(byte[] b) { return Base64.encodeToString(b, Base64.URL_SAFE | Base64.NO_PADDING | Base64.NO_WRAP); }

    private static String enc(String s) {
        try {
            return URLEncoder.encode(s, "UTF-8");
        } catch (Exception e) {
            throw new RuntimeException(e);
        }
    }

    // What the sign-in started: the browser's address, and a thread waiting on the redirect.
    interface Done { void done(String error); }  // error null: signed in

    private static ServerSocket waiting;

    // Starts the loopback server and returns the address to open in the browser. `done` runs on the server's thread
    // when Dropbox comes back (or with an error).
    static synchronized String start(final Context ctx, final Done done) throws Exception {
        if (waiting != null) {
            try { waiting.close(); } catch (Exception ignored) {}
            waiting = null;
        }
        SecureRandom rnd = new SecureRandom();
        byte[] v = new byte[48], s = new byte[16];
        rnd.nextBytes(v);
        rnd.nextBytes(s);
        final String verifier = b64(v), state = b64(s);
        String challenge = b64(MessageDigest.getInstance("SHA-256").digest(verifier.getBytes(StandardCharsets.US_ASCII)));
        final ServerSocket server = new ServerSocket(PORT, 4, InetAddress.getByName("127.0.0.1"));
        server.setSoTimeout(10 * 60 * 1000);  // ten minutes to sign in
        waiting = server;
        final Context app = ctx.getApplicationContext();
        new Thread(() -> {
            String error;
            try {
                error = serve(app, server, verifier, state);
            } catch (Exception e) {
                error = e.getMessage() == null ? e.toString() : e.getMessage();
            } finally {
                try { server.close(); } catch (Exception ignored) {}
                synchronized (DropboxAuth.class) { if (waiting == server) waiting = null; }
            }
            done.done(error);
        }, "dropbox-sign-in").start();
        return "https://www.dropbox.com/oauth2/authorize?client_id=" + APP_KEY + "&response_type=code"
                + "&code_challenge=" + challenge + "&code_challenge_method=S256&token_access_type=offline"
                + "&scope=" + enc("files.metadata.read files.content.read") + "&redirect_uri=" + enc(REDIRECT)
                + "&state=" + state;
    }

    // Answers requests until one carries the code (or an error) for this sign-in.
    private static String serve(Context app, ServerSocket server, String verifier, String state) throws Exception {
        while (true) {
            try (Socket s = server.accept()) {
                s.setSoTimeout(15000);
                String line = firstLine(s.getInputStream());
                String query = "";
                int q = line.indexOf('?'), sp = line.lastIndexOf(' ');
                if (line.startsWith("GET ") && q > 0) query = line.substring(q + 1, sp > q ? sp : line.length());
                String code = param(query, "code"), err = param(query, "error"), got = param(query, "state");
                if (code == null && err == null) {  // a favicon or a stray request
                    reply(s.getOutputStream(), 404, "");
                    continue;
                }
                if (!state.equals(got)) {
                    reply(s.getOutputStream(), 400, page("This sign-in is out of date. Go back to ACNH Recomp and try again."));
                    return "the sign-in was out of date";
                }
                if (err != null) {
                    reply(s.getOutputStream(), 200, page("Dropbox sign-in was cancelled. Go back to ACNH Recomp."));
                    String why = param(query, "error_description");
                    return why != null ? why : err;
                }
                String error = null;
                try {
                    JSONObject t = token("grant_type=authorization_code&code=" + enc(code) + "&client_id=" + APP_KEY
                            + "&code_verifier=" + enc(verifier) + "&redirect_uri=" + enc(REDIRECT));
                    prefs(app).edit().putString("refresh", t.getString("refresh_token"))
                            .putString("access", t.getString("access_token"))
                            .putLong("expires", System.currentTimeMillis() + t.optLong("expires_in", 14400) * 1000).apply();
                } catch (Exception e) {
                    error = e.getMessage() == null ? e.toString() : e.getMessage();
                }
                reply(s.getOutputStream(), 200, page(error == null
                        ? "Signed in to Dropbox. Go back to ACNH Recomp: the download starts there."
                        : "Dropbox sign-in failed: " + error));
                return error;
            }
        }
    }

    private static String firstLine(InputStream in) throws Exception {
        ByteArrayOutputStream b = new ByteArrayOutputStream();
        int c;
        while ((c = in.read()) >= 0 && c != '\n' && b.size() < 8192) b.write(c);
        return new String(b.toByteArray(), StandardCharsets.UTF_8).trim();
    }

    private static String param(String query, String name) throws Exception {
        for (String kv : query.split("&")) {
            int eq = kv.indexOf('=');
            if (eq > 0 && kv.substring(0, eq).equals(name)) return URLDecoder.decode(kv.substring(eq + 1), "UTF-8");
        }
        return null;
    }

    private static String page(String text) {
        return "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'>"
                + "<body style='font-family:sans-serif;padding:2em;background:#fffae8;color:#5c4632'><h2>ACNH Recomp</h2><p>"
                + text.replace("&", "&amp;").replace("<", "&lt;") + "</p>";
    }

    private static void reply(OutputStream out, int status, String body) throws Exception {
        byte[] b = body.getBytes(StandardCharsets.UTF_8);
        out.write(("HTTP/1.1 " + status + " OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: " + b.length
                + "\r\nConnection: close\r\n\r\n").getBytes(StandardCharsets.US_ASCII));
        out.write(b);
        out.flush();
    }

    private static JSONObject token(String form) throws Exception {
        HttpURLConnection c = (HttpURLConnection) new URL("https://api.dropboxapi.com/oauth2/token").openConnection();
        c.setConnectTimeout(20000);
        c.setReadTimeout(30000);
        c.setRequestMethod("POST");
        c.setDoOutput(true);
        c.setRequestProperty("Content-Type", "application/x-www-form-urlencoded");
        try (OutputStream o = c.getOutputStream()) { o.write(form.getBytes(StandardCharsets.UTF_8)); }
        int code = c.getResponseCode();
        InputStream in = code < 400 ? c.getInputStream() : c.getErrorStream();
        String body = in == null ? "" : new String(readAll(in), StandardCharsets.UTF_8);
        c.disconnect();
        if (code != 200) throw new Exception("Dropbox answered " + code + (body.isEmpty() ? "" : ": " + body));
        return new JSONObject(body);
    }

    static byte[] readAll(InputStream in) throws Exception {
        ByteArrayOutputStream b = new ByteArrayOutputStream();
        byte[] buf = new byte[16384];
        int n;
        while ((n = in.read(buf)) > 0) b.write(buf, 0, n);
        in.close();
        return b.toByteArray();
    }

    // A current access token, refreshed when it is within five minutes of expiring (or when `force`).
    static synchronized String access(Context c, boolean force) throws Exception {
        SharedPreferences p = prefs(c);
        String refresh = p.getString("refresh", "");
        if (refresh.isEmpty()) throw new Exception("not signed in to Dropbox");
        String access = p.getString("access", "");
        if (!force && !access.isEmpty() && p.getLong("expires", 0) - System.currentTimeMillis() > 5 * 60 * 1000) return access;
        JSONObject t = token("grant_type=refresh_token&refresh_token=" + enc(refresh) + "&client_id=" + APP_KEY);
        access = t.getString("access_token");
        p.edit().putString("access", access)
                .putLong("expires", System.currentTimeMillis() + t.optLong("expires_in", 14400) * 1000).apply();
        return access;
    }
}
