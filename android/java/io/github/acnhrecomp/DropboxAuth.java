// Dropbox sign-in, for the save sync (issue #81) and the in-app game download (issue #230; SPEC M18 and the Dropbox
// exception). Both use save-relay's Dropbox app (owner's choice, 2026-11-23: "Reuse Save Relay's app"): an App
// folder app, so the token reaches only that app's own folder, Apps/<app>/, where save-relay keeps the island and the
// prepared game's files wait in acnh-universal-game/android/. The sign-in is save-relay's own (its C interface,
// save_relay_sign_in_start / _finish, through org.saverelay.SaveRelay): OAuth 2 with PKCE, no client secret, its
// scopes (account_info.read, files.metadata.read/write, files.content.read/write), the browser coming back to the
// redirect save-relay registered, http://127.0.0.1:53710/, which a small server in this app answers and hands to
// save-relay. save-relay exchanges the code and files the refresh token in the app's credential store (Relay); the
// game download reads it from there too, so one sign-in serves both. An install signed in before this, for the
// download only (read-only scopes, its refresh token in the private preferences), keeps downloading with that token;
// save sync asks it to sign in once more. Nothing of the sign-in leaves the device but the calls to Dropbox.
package io.github.acnhrecomp;

import android.content.Context;
import android.content.SharedPreferences;
import android.net.Uri;

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

final class DropboxAuth {
    // save-relay's Dropbox app key (save-relay src/clients.rs: a public PKCE client, App folder access).
    static final String APP_KEY = "p8xhxihp8s3gb33";
    static final String REDIRECT = "http://127.0.0.1:53710/";
    private static final int PORT = 53710;
    private static final String PREFS = "dropbox";

    private DropboxAuth() {}

    private static SharedPreferences prefs(Context c) { return c.getSharedPreferences(PREFS, Context.MODE_PRIVATE); }

    // The refresh token the download uses: save-relay's (the current sign-in), else an older install's own.
    private static String refresh(Context c) {
        String relay = Relay.dropboxRefresh(c);
        return relay != null ? relay : prefs(c).getString("refresh", "");
    }

    static boolean signedIn(Context c) { return !refresh(c).isEmpty(); }

    // Dropbox no longer takes the token: forgotten (the download's cache and save-relay's token), so the next try
    // signs in again. Save sync stays set up and asks for that sign-in before the game.
    static void signOut(Context c) {
        prefs(c).edit().clear().apply();
        Relay.dropDropboxToken(c);
    }

    // Home's "Sign out of Dropbox": the download's sign-in and save sync both end on this device.
    static void signOutAll(Context c) {
        prefs(c).edit().clear().apply();
        Relay.turnOff(c);
    }

    private static String enc(String s) {
        try {
            return URLEncoder.encode(s, "UTF-8");
        } catch (Exception e) {
            throw new RuntimeException(e);
        }
    }

    private static ServerSocket waiting;
    private static int generation, finished;
    private static String failed;  // why the current sign-in failed, or null

    // Why the latest sign-in failed (GameDownload waits on it), or null while it runs or when it worked.
    static synchronized String failure() { return failed; }

    // The latest sign-in's number (start's), for await and cancel.
    static synchronized int current() { return generation; }

    // Waits for sign-in `gen` to end: null when it worked, else why not (a newer start supersedes it).
    static synchronized String await(int gen) {
        while (finished < gen && generation == gen) {
            try {
                DropboxAuth.class.wait();
            } catch (InterruptedException e) {
                return "interrupted";
            }
        }
        if (generation != gen) return "another sign-in started";
        return failed;
    }

    // Stops sign-in `gen` waiting for the browser.
    static synchronized void cancel(int gen) {
        if (gen == generation && waiting != null) {
            try { waiting.close(); } catch (Exception ignored) {}
        }
    }

    private static String envelopeError(JSONObject envelope) {
        JSONObject e = envelope.optJSONObject("error");
        String m = e == null ? "" : e.optString("message");
        return m.isEmpty() ? "save-relay could not sign in" : m;
    }

    // Starts save-relay's sign-in and the loopback server, and returns the address to open in the browser. A newer
    // start supersedes an older one, whose outcome is then ignored.
    static synchronized String start(final Context ctx) throws Exception {
        if (waiting != null) {
            try { waiting.close(); } catch (Exception ignored) {}
            waiting = null;
        }
        failed = null;
        final int gen = ++generation;
        DropboxAuth.class.notifyAll();  // an older await ends: superseded
        final Context app = ctx.getApplicationContext();
        appContext = app;
        JSONObject started = new JSONObject(Relay.saveRelay(app).signInStart("dropbox", REDIRECT, "Android", null));
        if (!started.optBoolean("ok")) throw new Exception(envelopeError(started));
        final String url = started.getJSONObject("data").getString("url");
        final String state = Uri.parse(url).getQueryParameter("state");
        if (state == null) throw new Exception("save-relay's sign-in has no state");
        final ServerSocket server = new ServerSocket(PORT, 4, InetAddress.getByName("127.0.0.1"));
        server.setSoTimeout(15 * 60 * 1000);  // fifteen minutes to sign in
        waiting = server;
        new Thread(() -> {
            String error;
            try {
                error = serve(app, server, state);
            } catch (Exception e) {
                error = server.isClosed() ? "cancelled" : e.getMessage() == null ? e.toString() : e.getMessage();
            } finally {
                try { server.close(); } catch (Exception ignored) {}
                synchronized (DropboxAuth.class) {
                    if (waiting == server) waiting = null;
                }
            }
            synchronized (DropboxAuth.class) {
                if (gen == generation) {
                    failed = error == null ? null : error;
                    if (error == null && !signedIn(app)) failed = "no sign-in came back";
                    finished = gen;
                    DropboxAuth.class.notifyAll();
                }
            }
        }, "dropbox-sign-in").start();
        return url;
    }

    // Answers requests until one carries this sign-in's answer, then hands it to save-relay.
    private static String serve(Context app, ServerSocket server, String state) throws Exception {
        while (true) {
            Socket s = server.accept();
            try {
                s.setSoTimeout(5000);
                InputStream in = s.getInputStream();
                String line = firstLine(in);
                // Read the rest of the request (to the blank line): closing with it unread resets the connection,
                // and the browser would show an error instead of the page below.
                for (String h = firstLine(in); !h.isEmpty(); h = firstLine(in)) {}
                String target = "", query = "";
                int sp = line.lastIndexOf(' ');
                if (line.startsWith("GET ") && sp > 4) target = line.substring(4, sp);
                int q = target.indexOf('?');
                if (q >= 0) query = target.substring(q + 1);
                String code = param(query, "code"), err = param(query, "error"), got = param(query, "state");
                if ((code == null && err == null) || !state.equals(got)) {
                    // a favicon, a stray request, or an older sign-in's page reloaded: not this sign-in's answer
                    reply(s.getOutputStream(), 404, page("Not this sign-in. Go back to ACNH Recomp."));
                    continue;
                }
                if (err != null) {
                    reply(s.getOutputStream(), 200, page("Dropbox sign-in was cancelled. Go back to ACNH Recomp."));
                    String why = param(query, "error_description");
                    return why != null ? why : err;
                }
                String error = null;
                try {
                    // save-relay checks the state, exchanges the code with its PKCE verifier and files the token.
                    JSONObject done = new JSONObject(Relay.saveRelay(app).signInFinish("http://127.0.0.1:" + PORT + target));
                    if (!done.optBoolean("ok")) error = envelopeError(done);
                    else prefs(app).edit().clear().apply();  // an older download-only sign-in: replaced
                } catch (Exception e) {
                    error = e.getMessage() == null ? e.toString() : e.getMessage();
                }
                reply(s.getOutputStream(), 200, error == null ? back() : page("Dropbox sign-in failed: " + error));
                return error;
            } catch (java.net.SocketTimeoutException e) {
                // one bad connection (a preconnect that never sends): wait for the next
            } catch (java.net.SocketException e) {
                if (server.isClosed()) throw e;  // cancelled
            } finally {
                try { s.close(); } catch (Exception ignored) {}
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

    // After a sign-in: straight back to the app (an intent link Chrome hands to Android), with a button in case
    // the browser asks for a tap first.
    private static String back() {
        String app = "intent://signed-in#Intent;scheme=acnhrecomp;package=io.github.acnhrecomp;end";
        return "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'>"
                + "<body style='font-family:sans-serif;padding:2em;background:#fffae8;color:#5c4632;text-align:center'>"
                + "<h2>Signed in to Dropbox</h2><p>Go back to ACNH Recomp.</p>"
                + "<p><a href='" + app + "' style='display:inline-block;padding:16px 28px;border-radius:26px;"
                + "background:#5aaa5a;color:white;font-size:20px;text-decoration:none'>Back to ACNH Recomp</a></p>"
                + "<script>location.replace('" + app + "')</script>";
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

    private static Context appContext;

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
        if (code == 400 || code == 401) {
            if (body.contains("invalid_grant") && form.startsWith("grant_type=refresh_token")) {
                signOut(appContext);
                throw new GameDownload.Fatal("Dropbox no longer accepts this device's sign-in. Sign in again.");
            }
            throw new GameDownload.Fatal("Dropbox refused the sign-in (" + code + (body.isEmpty() ? "" : ": " + body) + ")");
        }
        if (code != 200) throw new java.io.IOException("Dropbox answered " + code);
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

    // A current access token for the download, refreshed when it is within five minutes of expiring (or when
    // `force`). Only the access token is cached here; the refresh token stays where its sign-in put it.
    static synchronized String access(Context c, boolean force) throws Exception {
        appContext = c.getApplicationContext();
        SharedPreferences p = prefs(c);
        String refresh = refresh(c);
        if (refresh.isEmpty()) throw new GameDownload.Fatal("Not signed in to Dropbox. Tap Download to sign in.");
        String access = p.getString("access", "");
        if (!force && !access.isEmpty() && p.getLong("expires", 0) - System.currentTimeMillis() > 5 * 60 * 1000) return access;
        JSONObject t = token("grant_type=refresh_token&refresh_token=" + enc(refresh) + "&client_id=" + APP_KEY);
        access = t.getString("access_token");
        p.edit().putString("access", access)
                .putLong("expires", System.currentTimeMillis() + t.optLong("expires_in", 14400) * 1000).apply();
        return access;
    }
}
