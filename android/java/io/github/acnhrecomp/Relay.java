// Save sync (issue #81): the island goes through the owner's Dropbox with save-relay, as on the iPad. Before the game
// starts, the home screen pulls it (beforeGame, on a worker: the Rust loop in crates/runtime relay.rs, through
// relay_android.cpp), and the game only starts after a pull succeeded, with save sync not set up, or after "Play
// anyway" on an island another device holds. The game pushes it when it quits and when the app goes to the
// background (platform_android.cpp). The loop's screens come here as JSON requests (ask, BridgePrompt in relay.rs)
// and are shown as dialogs on the home screen; the sign-in is save-relay's own Dropbox sign-in through the browser
// (DropboxAuth, save-relay's C interface through its Java wrapper in org.saverelay). The host below is what that
// interface asks the app for: its network (HttpURLConnection) and its credential store (a 0600 file in the app's
// private files folder, crates/runtime FileStore, reached through the natives). Nothing here logs a token, the
// account or a request.
package io.github.acnhrecomp;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.Context;
import android.content.Intent;
import android.net.Uri;
import android.os.Handler;
import android.os.Looper;

import org.json.JSONObject;
import org.saverelay.SaveRelay;
import org.saverelay.SaveRelayHost;
import org.saverelay.SaveRelayResponse;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.BlockingQueue;

import static io.github.acnhrecomp.Screen.t;

final class Relay {
    // The latest failure of the host's HTTP call, shown on the sign-in's error page (no query strings, no tokens).
    static volatile String lastHttpError;

    private Relay() {}

    // ---- the natives (runtime/src/relay_android.cpp) ------------------------------------------------------------

    private static native void nativeAttach(SaveRelayHost host);

    private static native void nativeBeforeGame(String dataRoot, String files);

    // 0: not set up; 1: set up with its token; 2: set up, the token gone (a new sign-in is needed)
    static native int nativeState(String files);

    private static native boolean nativeForget(String files, String dataRoot);

    private static native boolean nativeSignOut(String files);

    private static native byte[] nativeCredGet(String files, String account);

    private static native boolean nativeCredSet(String files, String account, String secret);

    private static native boolean nativeCredDelete(String files, String account);

    static final int OFF = 0, ON = 1, SIGN_IN_AGAIN = 2, BROKEN = 3;  // BROKEN: its settings do not read

    private static String files(Context c) { return c.getApplicationContext().getFilesDir().getAbsolutePath(); }

    static int state(Context c) {
        try {
            return nativeState(files(c));
        } catch (Throwable e) {
            return OFF;
        }
    }

    // ---- save-relay's C interface, with this app as its host ----------------------------------------------------

    private static SaveRelay relay;

    // save-relay's folders: <files>/relay-config and <files>/relay-data (crates/runtime android_vars), where the loop
    // finds what the sign-in writes.
    static synchronized SaveRelay saveRelay(Context c) {
        if (relay == null) {
            String files = files(c);
            Host host = new Host(files);
            relay = new SaveRelay(files, files + "/relay-data", files + "/relay-config", null, host);
            nativeAttach(host);
        }
        return relay;
    }

    private static final class Host implements SaveRelayHost {
        private final String files;

        Host(String files) { this.files = files; }

        @Override
        public byte[] credentialGet(String account) {
            try {
                return nativeCredGet(files, account);
            } catch (Throwable e) {
                return null;
            }
        }

        @Override
        public boolean credentialSet(String account, String secret) { return nativeCredSet(files, account, secret); }

        @Override
        public boolean credentialDelete(String account) { return nativeCredDelete(files, account); }

        @Override
        public byte[] http(String method, String url, String headers, byte[] body, long maxResponseBytes) {
            HttpURLConnection c = null;
            try {
                c = (HttpURLConnection) new URL(url).openConnection();
                c.setConnectTimeout(20000);
                c.setReadTimeout(60000);
                c.setInstanceFollowRedirects(false);
                c.setRequestMethod(method);
                boolean contentType = false, encoding = false;
                if (headers != null)
                    for (String line : headers.split("\n")) {
                        int colon = line.indexOf(':');
                        if (colon <= 0) continue;
                        String name = line.substring(0, colon).trim(), value = line.substring(colon + 1).trim();
                        if (name.equalsIgnoreCase("Content-Length") || name.equalsIgnoreCase("Host")) continue;
                        if (name.equalsIgnoreCase("Content-Type")) contentType = true;
                        if (name.equalsIgnoreCase("Accept-Encoding")) encoding = true;
                        c.setRequestProperty(name, value);
                    }
                // HttpURLConnection gives a POST without a Content-Type "application/x-www-form-urlencoded", which
                // Dropbox's download and list calls refuse with 400 (their arguments are in a header, the body is
                // empty): octet-stream, as GameDownload does. No transparent gzip: the body comes back as sent.
                if (method.equalsIgnoreCase("POST") && !contentType)
                    c.setRequestProperty("Content-Type", "application/octet-stream");
                if (!encoding) c.setRequestProperty("Accept-Encoding", "identity");
                if (body != null && body.length > 0) {  // an empty body is sent as none (no doOutput)
                    c.setDoOutput(true);
                    c.setFixedLengthStreamingMode(body.length);
                    try (OutputStream o = c.getOutputStream()) { o.write(body); }
                }
                int status = c.getResponseCode();
                StringBuilder lines = new StringBuilder();
                for (Map.Entry<String, List<String>> h : c.getHeaderFields().entrySet()) {
                    if (h.getKey() == null) continue;  // the status line
                    for (String v : h.getValue()) lines.append(h.getKey()).append(": ").append(v).append('\n');
                }
                InputStream in = status < 400 ? c.getInputStream() : c.getErrorStream();
                ByteArrayOutputStream out = new ByteArrayOutputStream();
                if (in != null) {
                    byte[] buf = new byte[16384];
                    int n;
                    while ((n = in.read(buf)) > 0) {
                        out.write(buf, 0, n);
                        if (maxResponseBytes > 0 && out.size() > maxResponseBytes) {
                            lastHttpError = "the answer is over " + maxResponseBytes + " bytes (" + method + " "
                                    + url.replaceAll("\\?.*", "") + ")";
                            return null;
                        }
                    }
                    in.close();
                }
                return new SaveRelayResponse(status, lines.toString(), out.toByteArray()).toWire();
            } catch (Throwable e) {
                // a connection, DNS, TLS or timeout failure: kept (without the query) for the sign-in's error page
                String where = method + " " + url.replaceAll("\\?.*", "");
                lastHttpError = e.getClass().getSimpleName() + (e.getMessage() == null ? "" : ": " + e.getMessage())
                        + " (" + where + ")";
                android.util.Log.w("acnh-relay", "http " + where + " failed", e);
                return null;
            } finally {
                if (c != null) c.disconnect();
            }
        }
    }

    // The Dropbox account save-relay signed in to here (its config's user), or null.
    private static String account(Context c) {
        try {
            File f = new File(files(c), "relay-config/save-relay/config.json");
            JSONObject config = new JSONObject(new String(Files.readAllBytes(f.toPath()), StandardCharsets.UTF_8));
            if (!"dropbox".equals(config.optString("server"))) return null;
            String user = config.optString("user");
            return user.isEmpty() ? null : "dropbox:" + user;
        } catch (Exception e) {
            return null;
        }
    }

    // Where the download's token waits once save sync is turned off (crates/runtime DOWNLOAD_ACCOUNT).
    private static final String DOWNLOAD_ACCOUNT = "download:dropbox";

    private static String secret(Context c, String account) {
        try {
            byte[] secret = nativeCredGet(files(c), account);
            return secret == null ? null : new String(secret, StandardCharsets.UTF_8);
        } catch (Throwable e) {
            return null;
        }
    }

    // The Dropbox refresh token for the game download: save-relay's (one sign-in serves both), or the one kept for
    // the download when save sync was turned off; null when signed out.
    static String dropboxRefresh(Context c) {
        String account = account(c);
        String token = account == null ? null : secret(c, account);
        return token != null ? token : secret(c, DOWNLOAD_ACCOUNT);
    }

    // A new sign-in replaced the token kept for the download alone.
    static void signedIn(Context c) {
        try { nativeCredDelete(files(c), DOWNLOAD_ACCOUNT); } catch (Throwable ignored) {}
        // The first sync after a sign-in is a pull: a push an older setup left waiting goes (crates/runtime
        // push_waiting_file), as does a mark a background push could not write back.
        new File(files(c), "relay-data/save-relay/push-waiting").delete();
        new File(files(c), "relay-data/save-relay/mark-waiting").delete();
    }

    // Dropbox no longer accepts the token: it goes (save sync stays set up and asks for a new sign-in).
    static void dropDropboxToken(Context c) {
        String account = account(c);
        if (account != null) nativeCredDelete(files(c), account);
        nativeCredDelete(files(c), DOWNLOAD_ACCOUNT);
    }

    // "Turn off save sync": the syncing stops here (save-relay's config and secrets go), the Dropbox sign-in stays for
    // the game download. The island on this device stays. False when something was left behind.
    static boolean turnOff(Context c) { return nativeForget(files(c), dataRoot(c)); }

    // The game folder of the last start (Home writes <files>/data_root.txt), whose config.ini keeps relay.ask; "" if none.
    private static String dataRoot(Context c) {
        try {
            return new String(Files.readAllBytes(new File(files(c), "data_root.txt").toPath()), StandardCharsets.UTF_8).trim();
        } catch (Exception e) {
            return "";
        }
    }

    // "Sign out of Dropbox": save sync and the game download both end here. False when something was left behind.
    static boolean signOut(Context c) { return nativeSignOut(files(c)); }

    // ---- the pull before the game ----------------------------------------------------------------------------------

    private static volatile boolean running;
    private static volatile Activity host;
    private static final Handler ui = new Handler(Looper.getMainLooper());

    static boolean running() { return running; }

    // The screen the dialogs go on (Home's onResume); a dialog waiting for an answer comes back on a new one.
    static void attach(final Activity a) {
        ui.post(() -> {
            if (host != a && shown != null) {  // the dialog moves to the screen now in front
                try { shown.dismiss(); } catch (Exception ignored) {}
                shown = null;
            }
            host = a;
            showWaiting();
        });
    }

    static void detach(Activity a) {
        if (host != a) return;
        host = null;
        if (shown != null) {
            try { shown.dismiss(); } catch (Exception ignored) {}
            shown = null;
        }
    }

    // Pulls the island of `folder` (on a worker), then starts the game on whatever screen is in front by then.
    static void beforeGame(Activity a, final String folder) {
        if (running) return;
        running = true;
        host = a;  // now: the worker may ask for the sign-in before the posted attach runs
        attach(a);
        final Context app = a.getApplicationContext();
        new Thread(() -> {
            try {
                saveRelay(app);
                nativeBeforeGame(folder, files(app));
            } finally {
                running = false;
            }
            ui.post(() -> {
                Intent game = new Intent();
                game.setClassName(app.getPackageName(), "android.app.NativeActivity");
                Activity front = host;
                if (front != null && !front.isFinishing()) {
                    front.startActivity(game);
                    front.finish();
                } else {
                    game.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                    app.startActivity(game);
                }
            });
        }, "relay pull").start();
    }

    // ---- the screens: one request at a time from the loop, answered on the home screen -------------------------

    private static JSONObject waiting;                    // the request on screen (or waiting for a screen)
    private static BlockingQueue<String> answers;
    private static AlertDialog shown;

    // Called by the loop (relay_android.cpp, on the pull's worker): blocks until the person answers.
    static String ask(String request) {
        try {
            JSONObject r = new JSONObject(request);
            if (r.optString("kind").equals("sign_in")) return signInScreen(r);
            return show(r);
        } catch (Exception e) {
            return null;
        }
    }

    private static String show(JSONObject request) throws InterruptedException {
        BlockingQueue<String> q = new ArrayBlockingQueue<>(1);
        synchronized (Relay.class) {
            waiting = request;
            answers = q;
        }
        ui.post(Relay::showWaiting);
        String answer = q.take();
        synchronized (Relay.class) {
            if (answers == q) {
                waiting = null;
                answers = null;
            }
        }
        return "{\"answer\":" + JSONObject.quote(answer) + "}";
    }

    private static void answer(String a) {
        BlockingQueue<String> q;
        synchronized (Relay.class) {
            q = answers;
            waiting = null;
        }
        shown = null;
        if (q != null) q.offer(a);
    }

    // On the UI thread: the waiting request as a dialog on the screen in front.
    private static void showWaiting() {
        JSONObject r;
        synchronized (Relay.class) { r = waiting; }
        Activity a = host;
        if (r == null || a == null || a.isFinishing() || shown != null) return;
        AlertDialog.Builder b = new AlertDialog.Builder(a).setCancelable(false);
        String message = r.isNull("message") ? "" : r.optString("message");
        switch (r.optString("kind")) {
            case "sign_in_choice":
                b.setTitle(t("Save sync", "Sincronização do save"))
                        .setMessage((message.isEmpty() ? "" : message + "\n\n") + t(
                                "Keep your island in your Dropbox, so it goes with you to your other devices: the app "
                                        + "fetches it before the game starts and puts it back when you leave.",
                                "Guarde a sua ilha no seu Dropbox, para ela ir com você para os seus outros aparelhos: o "
                                        + "app a busca antes de o jogo começar e a devolve quando você sai."))
                        .setPositiveButton(t("Sign in to Dropbox", "Entrar no Dropbox"), (d, w) -> answer("sign_in"));
                if (r.optBoolean("allow_not_now"))
                    b.setNegativeButton(t("Not now", "Agora não"), (d, w) -> answer("not_now"))
                            .setNeutralButton(t("Later", "Depois"), (d, w) -> answer("cancel"));
                else
                    b.setNegativeButton(t("Cancel", "Cancelar"), (d, w) -> answer("cancel"));
                break;
            case "signing_in":
                b.setTitle(t("Dropbox sign-in", "Login no Dropbox"))
                        .setMessage(t("Sign in to Dropbox in the browser, allow ACNH Recomp's access, then come back here.",
                                "Entre no Dropbox no navegador, permita o acesso do ACNH Recomp e volte aqui."))
                        .setNegativeButton(t("Cancel", "Cancelar"), (d, w) -> answer("cancel"));
                break;
            case "conflict":
                b.setTitle(t("Both islands changed", "As duas ilhas mudaram"))
                        .setMessage(t("The island in Dropbox: ", "A ilha no Dropbox: ") + r.optString("server") + "\n\n"
                                + t("The island on this device: ", "A ilha neste aparelho: ") + r.optString("local")
                                + "\n\n" + t("Keep which one? save-relay keeps a backup of the other.",
                                "Qual manter? O save-relay guarda uma cópia da outra."))
                        .setPositiveButton(t("Keep Dropbox's", "Manter a do Dropbox"), (d, w) -> answer("server"))
                        .setNegativeButton(t("Keep this device's", "Manter a deste aparelho"), (d, w) -> answer("local"));
                break;
            case "held":
                b.setTitle(t("The island is open elsewhere", "A ilha está aberta em outro lugar"))
                        .setMessage(t("Another device has the island open: ", "Outro aparelho está com a ilha aberta: ")
                                + r.optString("device") + " (" + r.optString("since") + ").\n\n"
                                + t("Playing here now can leave two different islands.",
                                "Jogar aqui agora pode deixar duas ilhas diferentes."))
                        .setPositiveButton(t("Check again", "Verificar de novo"), (d, w) -> answer("check_again"))
                        .setNegativeButton(t("Play anyway", "Jogar mesmo assim"), (d, w) -> answer("play_anyway"));
                break;
            default:  // blocked
                String why = r.optString("why");
                String text = why.equals("offline")
                        ? t("Dropbox could not be reached. The game starts once your island is synced.",
                            "Não foi possível falar com o Dropbox. O jogo começa quando a ilha estiver sincronizada.")
                        : why.equals("sign_in_cancelled")
                        ? t("Save sync is set up, so the game waits for the Dropbox sign-in.",
                            "A sincronização está ligada, então o jogo espera o login no Dropbox.")
                        : message;
                b.setTitle(t("Save sync", "Sincronização do save")).setMessage(text)
                        .setPositiveButton(t("Retry", "Tentar de novo"), (d, w) -> answer("retry"));
                if (why.equals("sign_in_cancelled"))
                    b.setNegativeButton(t("Sign in again", "Entrar de novo"), (d, w) -> answer("sign_in_again"));
                // The way out when the sign-in is gone or unreadable: sync off here, the game starts with this
                // device's island (the Dropbox sign-in stays for the download).
                if (r.optBoolean("can_turn_off"))
                    b.setNeutralButton(t("Turn off save sync", "Desligar a sincronização"), (d, w) -> answer("turn_off"));
        }
        try {
            shown = b.show();
        } catch (Exception e) {
            shown = null;  // the screen went away: the next one shows it
        }
    }

    // The sign-in screen: the choice, then save-relay's sign-in in the browser; a failed sign-in comes back to the
    // choice with its reason. "signed_in" only once save-relay has set save sync up.
    private static String signInScreen(JSONObject request) throws Exception {
        String note = request.isNull("message") ? null : request.optString("message");
        while (true) {
            JSONObject choice = new JSONObject().put("kind", "sign_in_choice")
                    .put("allow_not_now", request.optBoolean("allow_not_now")).put("message", note == null ? JSONObject.NULL : note);
            String picked = new JSONObject(show(choice)).getString("answer");
            if (!picked.equals("sign_in")) return "{\"answer\":" + JSONObject.quote(picked) + "}";
            String failed = signIn(true);
            if (failed == null) return "{\"answer\":\"signed_in\"}";
            note = t("Dropbox sign-in: ", "Login no Dropbox: ") + failed;
        }
    }

    // save-relay's Dropbox sign-in in the browser, from the screen in front; null when it worked, else why not. With
    // `screen`, a dialog with Cancel shows while the browser is open.
    static String signIn(boolean screen) {
        Activity a = host;
        if (a == null) return t("the app is not on screen", "o app não está na tela");
        final int gen;
        try {
            String url = DropboxAuth.start(a);
            gen = DropboxAuth.current();
            a.startActivity(new Intent(Intent.ACTION_VIEW, Uri.parse(url)));
        } catch (Exception e) {
            return e.getMessage() == null ? e.toString() : e.getMessage();
        }
        if (!screen) return DropboxAuth.await(gen);
        final String[] result = new String[1];
        final boolean[] done = new boolean[1];
        Thread waiter = new Thread(() -> {
            String r = DropboxAuth.await(gen);
            synchronized (result) {
                result[0] = r;
                done[0] = true;
            }
            if (DropboxAuth.current() == gen) answerIf("signing_in", "done");
        }, "dropbox sign-in wait");
        waiter.start();
        try {
            String picked = new JSONObject(show(new JSONObject().put("kind", "signing_in"))).getString("answer");
            if (picked.equals("cancel")) {
                DropboxAuth.cancel(gen);
                return t("cancelled", "cancelado");
            }
            waiter.join();
        } catch (Exception e) {
            DropboxAuth.cancel(gen);
            return e.toString();
        }
        synchronized (result) { return done[0] ? result[0] : "no sign-in came back"; }
    }

    // Answers the request on screen when it is of `kind` (the browser's return ends the "signing in" dialog).
    private static void answerIf(String kind, String a) {
        ui.post(() -> {
            synchronized (Relay.class) {
                if (waiting == null || !waiting.optString("kind").equals(kind)) return;
            }
            if (shown != null) {
                try { shown.dismiss(); } catch (Exception ignored) {}
            }
            answer(a);
        });
    }
}
