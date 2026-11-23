// The in-app game download (issue #230; SPEC M18): fetches the prepared game from the owner's own Dropbox (the
// app folder of save-relay's Dropbox app, acnh-universal-game/android/) into the app's private storage,
// <files>/gamedata, with nothing of it outside the app. A foreground service with a progress notification, so it
// keeps going with the app in the background or swiped away; a partial wake lock keeps the device from sleeping
// under it. It is started before the browser opens for the Dropbox sign-in, which also keeps the app's process
// from being frozen while the browser comes back to DropboxAuth's loopback server; it waits for the sign-in and
// then downloads.
//
// Resumable and updatable: manifest.json (written beside the files by tools/stage_dropbox_game.sh) lists each
// file's path, size and SHA-256. A file the installed game already has (same path and hash in
// gamedata/manifest.json) is kept; the others are fetched into <files>/gamedata.partial/, a shorter one continuing
// with an HTTP Range request, each checked against its hash (verified.txt), and moved into gamedata/ only when all
// check out. The player's own files in gamedata/ (user/, config.ini, logs) are never touched. Waits for an
// unmetered network (Wi-Fi), retries network errors with a growing pause, and stops at once on an error that a
// retry cannot fix.
package io.github.acnhrecomp;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.os.IBinder;
import android.os.PowerManager;
import android.os.SystemClock;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;

public class GameDownload extends Service {
    // Where the game waits, inside save-relay's Dropbox app folder.
    static final String REMOTE = "/acnh-universal-game/android";
    static final String DIR = "gamedata";
    private static final String CHANNEL = "download";
    private static final int NOTE = 1;
    private static final int TRIES = 30;  // network errors in a row before it stops

    // What the home screen draws while it is open (read on its own thread; written here).
    static volatile boolean running;
    static volatile long done, total;
    static volatile String status = "", error;
    static volatile boolean finished;

    // One worker per process, whatever happens to service instances.
    private static Thread worker;
    private PowerManager.WakeLock wake;

    // An error a retry cannot fix: the download stops with this message.
    static final class Fatal extends Exception {
        Fatal(String m) { super(m); }
    }

    static File gameDir(Context c) { return new File(c.getFilesDir(), DIR); }
    static File partialDir(Context c) { return new File(c.getFilesDir(), DIR + ".partial"); }

    // Starts the service from the foreground (the home screen). Throws when Android refuses.
    static void start(Context c) {
        c.startForegroundService(new Intent(c, GameDownload.class));
    }

    @Override
    public IBinder onBind(Intent intent) { return null; }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.createNotificationChannel(new NotificationChannel(CHANNEL, "Game download", NotificationManager.IMPORTANCE_LOW));
        Notification n = note("Starting the download", 0, 0);
        startForeground(NOTE, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC);
        synchronized (GameDownload.class) {
            if (worker == null || !worker.isAlive()) {
                running = true;
                finished = false;
                error = null;
                if (wake == null) {
                    wake = getSystemService(PowerManager.class).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "acnh:download");
                    wake.acquire();
                }
                worker = new Thread(this::work, "game-download");
                worker.start();
            }
        }
        return START_NOT_STICKY;
    }

    // Android 15 and later: a dataSync service gets about 6 hours a day. Stop cleanly; what is fetched stays.
    @Override
    public void onTimeout(int startId, int fgsType) {
        synchronized (GameDownload.class) { if (worker != null) worker.interrupt(); }
        error = "The download paused (Android's daily limit for downloads in the background). Open the app to go on.";
        running = false;
        release();
        stopForeground(STOP_FOREGROUND_REMOVE);
        stopSelf();
    }

    @Override
    public void onDestroy() {
        synchronized (GameDownload.class) { if (worker != null) worker.interrupt(); }
        release();
        super.onDestroy();
    }

    private void release() {
        try { if (wake != null && wake.isHeld()) wake.release(); } catch (Exception ignored) {}
    }

    private PendingIntent openHome() {
        Intent open = new Intent(this, Home.class).addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP | Intent.FLAG_ACTIVITY_SINGLE_TOP);
        return PendingIntent.getActivity(this, 0, open, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    }

    private Notification note(String text, long have, long all) {
        Notification.Builder b = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(android.R.drawable.stat_sys_download)
                .setContentTitle("ACNH Recomp: game download")
                .setContentText(text)
                .setContentIntent(openHome())
                .setOnlyAlertOnce(true)
                .setOngoing(true);
        if (all > 0) b.setProgress(1000, (int) (have * 1000 / all), false);
        return b.build();
    }

    private void update(String text) {
        status = text;
        getSystemService(NotificationManager.class).notify(NOTE, note(text, done, total));
    }

    static String gb(long bytes) { return String.format(Locale.ROOT, "%.2f GB", bytes / 1e9); }

    private static final class Item {
        String path, sha;
        long size;
    }

    private void work() {
        String fail = null;
        try {
            run();
        } catch (InterruptedException e) {
            fail = error != null ? error : "The download stopped. Open the app to go on.";
        } catch (Exception e) {
            fail = e.getMessage() == null ? e.toString() : e.getMessage();
        }
        error = fail;
        finished = fail == null;
        running = false;
        release();
        stopForeground(STOP_FOREGROUND_REMOVE);
        Notification.Builder b = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(fail == null ? android.R.drawable.stat_sys_download_done : android.R.drawable.stat_notify_error)
                .setContentTitle(fail == null ? "ACNH Recomp: the game is ready" : "ACNH Recomp: the download stopped")
                .setContentText(fail == null ? "Open the app to play." : fail)
                .setStyle(new Notification.BigTextStyle().bigText(fail == null ? "Open the app to play." : fail))
                .setAutoCancel(true)
                .setContentIntent(openHome());
        getSystemService(NotificationManager.class).notify(NOTE, b.build());
        stopSelf();
    }

    // Waits for the Dropbox sign-in the home screen started (DropboxAuth), up to ten minutes.
    private void awaitSignIn() throws Exception {
        long until = SystemClock.elapsedRealtime() + 10 * 60 * 1000;
        while (!DropboxAuth.signedIn(this)) {
            String failed = DropboxAuth.failure();
            if (failed != null) throw new Fatal("Dropbox sign-in: " + failed);
            if (SystemClock.elapsedRealtime() > until) throw new Fatal("The Dropbox sign-in did not finish. Try again.");
            update("Waiting for the Dropbox sign-in in the browser");
            Thread.sleep(500);
        }
    }

    private static List<Item> parse(byte[] raw) throws Exception {
        JSONArray files = new JSONObject(new String(raw, StandardCharsets.UTF_8)).getJSONArray("files");
        List<Item> items = new ArrayList<>();
        for (int i = 0; i < files.length(); i++) {
            JSONObject f = files.getJSONObject(i);
            Item it = new Item();
            it.path = f.getString("path");
            it.size = f.getLong("size");
            it.sha = f.getString("sha256").toLowerCase(Locale.ROOT);
            if (it.path.isEmpty() || it.path.startsWith("/") || it.path.contains("..") || it.path.contains("\\")
                    || it.path.startsWith("manifest.json") || it.path.startsWith("user/"))
                throw new Fatal("A bad name in the game's file list: " + it.path);
            items.add(it);
        }
        return items;
    }

    private void run() throws Exception {
        awaitSignIn();
        File part = partialDir(this), game = gameDir(this);
        if (!part.isDirectory() && !part.mkdirs()) throw new Fatal("cannot make " + part);
        update("Reading the game's file list");
        byte[] raw = withRetries(() -> fetch(REMOTE + "/manifest.json", 0, null, false));
        List<Item> items = parse(raw);

        // What the installed game already has: kept when the new list names the same file with the same hash.
        Map<String, String> installed = new HashMap<>();
        File installedList = new File(game, "manifest.json");
        if (installedList.isFile())
            try {
                for (Item it : parse(Files.readAllBytes(installedList.toPath()))) installed.put(it.path, it.sha);
            } catch (Exception ignored) {
                // an unreadable list: fetch everything again
            }
        File verifiedList = new File(part, "verified.txt");
        Set<String> verified = new HashSet<>();
        if (verifiedList.isFile())
            for (String l : new String(Files.readAllBytes(verifiedList.toPath()), StandardCharsets.UTF_8).split("\n"))
                if (!l.trim().isEmpty()) verified.add(l.trim());

        List<Item> fetch = new ArrayList<>();
        long all = 0, have = 0, need = 0;
        for (Item it : items) {
            all += it.size;
            File old = new File(game, it.path);
            if (it.sha.equals(installed.get(it.path)) && old.isFile() && old.length() == it.size) {
                have += it.size;
                continue;
            }
            fetch.add(it);
            File f = new File(part, it.path);
            long len = f.isFile() ? Math.min(f.length(), it.size) : 0;
            have += len;
            need += it.size - len;
        }
        total = all;
        done = have;
        if (fetch.isEmpty() && installedList.isFile()) {
            GameDownload.deleteTree(part);
            status = "The game is up to date.";
            return;
        }
        long free = getFilesDir().getUsableSpace();
        if (free < need + 500_000_000L)
            throw new Fatal("Not enough free space: the game needs " + gb(need + 500_000_000L) + " more, and "
                    + gb(free) + " is free.");

        for (Item it : fetch) {
            if (Thread.interrupted()) throw new InterruptedException();
            File f = new File(part, it.path);
            String key = it.path + " " + it.sha;
            if (verified.contains(key) && f.isFile() && f.length() == it.size) continue;
            File parent = f.getParentFile();
            if (parent != null && !parent.isDirectory() && !parent.mkdirs()) throw new Fatal("cannot make " + parent);
            for (int attempt = 0; ; attempt++) {
                download(it, f);
                update("Checking " + it.path);
                if (sha256(f).equals(it.sha)) break;
                done -= f.length();
                f.delete();
                if (attempt >= 1) throw new Fatal(it.path + " came out damaged twice. Try again later.");
            }
            verified.add(key);
            try (FileOutputStream o = new FileOutputStream(verifiedList, true)) {
                o.write((key + "\n").getBytes(StandardCharsets.UTF_8));
            }
        }

        // Every file checks out: move them into the game folder, then the list. The player's own files stay.
        update("Finishing");
        if (!game.isDirectory() && !game.mkdirs()) throw new Fatal("cannot make " + game);
        new File(game, "manifest.json").delete();  // a half-updated folder is never taken for a finished one
        for (Item it : fetch) {
            File from = new File(part, it.path), to = new File(game, it.path);
            File parent = to.getParentFile();
            if (parent != null && !parent.isDirectory() && !parent.mkdirs()) throw new Fatal("cannot make " + parent);
            if (to.exists() && !to.delete()) throw new Fatal("cannot replace " + to);
            if (!from.renameTo(to)) throw new Fatal("cannot move " + from + " to " + to);
        }
        Files.write(new File(game, "manifest.json").toPath(), raw);
        deleteTree(part);
        done = total;
        status = "The game is ready.";
    }

    private void download(Item it, File f) throws Exception {
        long have = f.isFile() ? f.length() : 0;
        if (have > it.size) {
            done -= Math.min(have, it.size);
            f.delete();
            have = 0;
        }
        int errors = 0;
        while (have < it.size) {
            if (Thread.interrupted()) throw new InterruptedException();
            try {
                fetch(REMOTE + "/" + it.path, have, f, false);
                errors = 0;
            } catch (InterruptedException | Fatal e) {
                throw e;
            } catch (Exception e) {
                if (++errors >= TRIES) throw new Fatal("The download keeps failing (" + e.getMessage() + "). Open the app to try again.");
                update("Waiting for the network (" + e.getMessage() + ")");
                Thread.sleep(Math.min(60_000L, 2000L * errors));
            }
            have = f.isFile() ? f.length() : 0;
        }
    }

    private interface Call { byte[] run() throws Exception; }

    private byte[] withRetries(Call call) throws Exception {
        for (int errors = 0; ; ) {
            try {
                return call.run();
            } catch (InterruptedException | Fatal e) {
                throw e;
            } catch (Exception e) {
                if (++errors >= TRIES) throw e;
                update("Waiting for the network (" + e.getMessage() + ")");
                Thread.sleep(Math.min(60_000L, 2000L * errors));
            }
        }
    }

    // Waits until the network is up and not metered: 11 GB never goes over mobile data.
    private void awaitUnmetered() throws InterruptedException {
        ConnectivityManager cm = getSystemService(ConnectivityManager.class);
        while (true) {
            Network n = cm.getActiveNetwork();
            NetworkCapabilities caps = n == null ? null : cm.getNetworkCapabilities(n);
            if (caps != null && caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                    && caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED))
                return;
            update(n == null ? "Waiting for the network" : "Waiting for Wi-Fi (the download never uses mobile data)");
            Thread.sleep(5000);
        }
    }

    // Dropbox-API-Arg must be ASCII: anything else as \\uXXXX.
    static String apiArg(String path) {
        StringBuilder s = new StringBuilder("{\"path\":\"");
        for (char ch : path.toCharArray()) {
            if (ch == '"' || ch == '\\') s.append('\\').append(ch);
            else if (ch < 0x20 || ch > 0x7e) s.append(String.format(Locale.ROOT, "\\u%04x", (int) ch));
            else s.append(ch);
        }
        return s.append("\"}").toString();
    }

    private long last;

    // One request to Dropbox's download endpoint. With `into` null, returns the whole body; else appends from byte
    // `from` to `into` and returns null.
    private byte[] fetch(String path, long from, File into, boolean retried) throws Exception {
        awaitUnmetered();
        HttpURLConnection c = (HttpURLConnection) new URL("https://content.dropboxapi.com/2/files/download").openConnection();
        c.setConnectTimeout(20000);
        c.setReadTimeout(60000);
        c.setRequestMethod("POST");
        // Android's HTTP stack adds a form Content-Type to a POST without one, which this endpoint refuses (400).
        c.setRequestProperty("Content-Type", "application/octet-stream");
        c.setRequestProperty("Accept-Encoding", "identity");
        c.setRequestProperty("Authorization", "Bearer " + DropboxAuth.access(this, retried));
        c.setRequestProperty("Dropbox-API-Arg", apiArg(path));
        if (from > 0) c.setRequestProperty("Range", "bytes=" + from + "-");
        try {
            int code = c.getResponseCode();
            if (code == 401 && !retried) {
                c.disconnect();
                return fetch(path, from, into, true);
            }
            if (code != 200 && code != 206) {
                InputStream es = c.getErrorStream();
                String body = es == null ? "" : new String(DropboxAuth.readAll(es), StandardCharsets.UTF_8);
                if (code == 409 && body.contains("not_found"))
                    throw new Fatal("The game's files are not in Dropbox yet (" + path.substring(1) + " in save-relay's "
                            + "app folder). If your computer is still uploading them, try again when it is done.");
                if (code == 401) {
                    DropboxAuth.signOut(this);
                    throw new Fatal("Dropbox refused the sign-in. Sign in again.");
                }
                if (code >= 400 && code < 500 && code != 408 && code != 429)
                    throw new Fatal("Dropbox answered " + code + (body.isEmpty() ? "" : ": " + body));
                throw new IOException("Dropbox answered " + code);
            }
            if (into == null) return DropboxAuth.readAll(c.getInputStream());
            if (from > 0 && code != 206) throw new IOException("Dropbox did not resume the file");
            byte[] buf = new byte[1 << 20];
            try (InputStream in = c.getInputStream(); FileOutputStream out = new FileOutputStream(into, true)) {
                int n;
                while ((n = in.read(buf)) > 0) {
                    out.write(buf, 0, n);
                    done += n;
                    long now = SystemClock.elapsedRealtime();
                    if (now - last > 1000) {
                        last = now;
                        update(gb(done) + " of " + gb(total));
                    }
                    if (Thread.interrupted()) throw new InterruptedException();
                }
            }
            return null;
        } finally {
            c.disconnect();
        }
    }

    private String sha256(File f) throws Exception {
        MessageDigest md = MessageDigest.getInstance("SHA-256");
        byte[] buf = new byte[1 << 20];
        try (FileInputStream in = new FileInputStream(f)) {
            int n;
            while ((n = in.read(buf)) > 0) {
                md.update(buf, 0, n);
                if (Thread.interrupted()) throw new InterruptedException();
            }
        }
        StringBuilder s = new StringBuilder();
        for (byte b : md.digest()) s.append(String.format(Locale.ROOT, "%02x", b));
        return s.toString();
    }

    static void deleteTree(File f) {
        File[] kids = f.listFiles();
        if (kids != null) for (File k : kids) deleteTree(k);
        f.delete();
    }
}
