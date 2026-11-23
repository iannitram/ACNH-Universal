// The in-app game download (issue #230; SPEC M18): fetches the prepared game from the owner's own Dropbox (the
// app folder of save-relay's Dropbox app, acnh-universal-game/android/) into the app's private storage,
// <files>/gamedata, with nothing of it outside the app. A foreground service with a progress notification, so it
// keeps going with the app in the background or swiped away; a partial wake lock and a Wi-Fi lock keep the device
// from sleeping under it.
//
// Resumable: files go into <files>/gamedata.partial/. manifest.json (written beside the files by
// tools/stage_dropbox_game.sh) lists each file's path, size and SHA-256. A file whose size and hash are already
// confirmed (verified.txt) is skipped; a shorter one continues with an HTTP Range request; the folder becomes
// gamedata/ only when every file checks out. Network errors are retried with a growing pause; a stop (no network
// for long, the app's process ended) leaves what was fetched, and the next start goes on from there.
package io.github.acnhrecomp;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.net.wifi.WifiManager;
import android.os.IBinder;
import android.os.PowerManager;
import android.os.SystemClock;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
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

    private Thread worker;
    private PowerManager.WakeLock wake;
    private WifiManager.WifiLock wifi;

    static File gameDir(Context c) { return new File(c.getFilesDir(), DIR); }
    static File partialDir(Context c) { return new File(c.getFilesDir(), DIR + ".partial"); }

    static void start(Context c) {
        Intent i = new Intent(c, GameDownload.class);
        c.startForegroundService(i);
    }

    @Override
    public IBinder onBind(Intent intent) { return null; }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.createNotificationChannel(new NotificationChannel(CHANNEL, "Game download", NotificationManager.IMPORTANCE_LOW));
        Notification n = note("Starting the download", 0, 0);
        if (android.os.Build.VERSION.SDK_INT >= 29) startForeground(NOTE, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC);
        else startForeground(NOTE, n);
        if (worker == null || !worker.isAlive()) {
            running = true;
            finished = false;
            error = null;
            wake = getSystemService(PowerManager.class).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "acnh:download");
            wake.acquire();
            WifiManager wm = (WifiManager) getApplicationContext().getSystemService(Context.WIFI_SERVICE);
            wifi = wm.createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "acnh:download");
            wifi.acquire();
            worker = new Thread(this::work, "game-download");
            worker.start();
        }
        return START_NOT_STICKY;
    }

    @Override
    public void onDestroy() {
        if (worker != null) worker.interrupt();
        release();
        running = false;
        super.onDestroy();
    }

    private void release() {
        try { if (wake != null && wake.isHeld()) wake.release(); } catch (Exception ignored) {}
        try { if (wifi != null && wifi.isHeld()) wifi.release(); } catch (Exception ignored) {}
    }

    private Notification note(String text, long have, long all) {
        Intent open = new Intent(this, Home.class).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
        PendingIntent pi = PendingIntent.getActivity(this, 0, open, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        Notification.Builder b = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(android.R.drawable.stat_sys_download)
                .setContentTitle("ACNH Recomp: game download")
                .setContentText(text)
                .setContentIntent(pi)
                .setOnlyAlertOnce(true)
                .setOngoing(true);
        if (all > 0) b.setProgress(1000, (int) (have * 1000 / all), false);
        return b.build();
    }

    private void update(String text) {
        status = text;
        getSystemService(NotificationManager.class).notify(NOTE, note(text, done, total));
    }

    static String gb(long bytes) { return String.format(java.util.Locale.ROOT, "%.2f GB", bytes / 1e9); }

    private static final class Item {
        String path, sha;
        long size;
    }

    private void work() {
        String fail = null;
        try {
            run();
        } catch (InterruptedException e) {
            fail = "The download stopped. Open the app to go on.";
        } catch (Exception e) {
            fail = e.getMessage() == null ? e.toString() : e.getMessage();
        }
        error = fail;
        finished = fail == null;
        running = false;
        release();
        stopForeground(STOP_FOREGROUND_DETACH);
        NotificationManager nm = getSystemService(NotificationManager.class);
        Notification.Builder b = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(fail == null ? android.R.drawable.stat_sys_download_done : android.R.drawable.stat_notify_error)
                .setContentTitle(fail == null ? "ACNH Recomp: the game is ready" : "ACNH Recomp: the download stopped")
                .setContentText(fail == null ? "Open the app to play." : fail)
                .setAutoCancel(true)
                .setContentIntent(PendingIntent.getActivity(this, 0, new Intent(this, Home.class),
                        PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT));
        nm.notify(NOTE, b.build());
        stopSelf();
    }

    private void run() throws Exception {
        File part = partialDir(this), game = gameDir(this);
        if (new File(game, "analysis/main.bin").isFile()) return;  // already there
        if (!part.isDirectory() && !part.mkdirs()) throw new Exception("cannot make " + part);
        update("Reading the file list");
        byte[] raw = withRetries(() -> fetchBytes(REMOTE + "/manifest.json"));
        Files.write(new File(part, "manifest.json").toPath(), raw);
        JSONArray files = new JSONObject(new String(raw, StandardCharsets.UTF_8)).getJSONArray("files");
        List<Item> items = new ArrayList<>();
        long all = 0;
        for (int i = 0; i < files.length(); i++) {
            JSONObject f = files.getJSONObject(i);
            Item it = new Item();
            it.path = f.getString("path");
            it.size = f.getLong("size");
            it.sha = f.getString("sha256").toLowerCase(java.util.Locale.ROOT);
            if (it.path.startsWith("/") || it.path.contains("..") || it.path.contains("\\"))
                throw new Exception("a bad name in the file list: " + it.path);
            items.add(it);
            all += it.size;
        }
        File verifiedList = new File(part, "verified.txt");
        Set<String> verified = new HashSet<>();
        if (verifiedList.isFile())
            for (String l : new String(Files.readAllBytes(verifiedList.toPath()), StandardCharsets.UTF_8).split("\n"))
                if (!l.trim().isEmpty()) verified.add(l.trim());
        long have = 0, need = 0;
        for (Item it : items) {
            File f = new File(part, it.path);
            long len = f.isFile() ? Math.min(f.length(), it.size) : 0;
            if (verified.contains(it.path + " " + it.sha) && f.length() == it.size) len = it.size;
            have += len;
            need += it.size - len;
        }
        total = all;
        done = have;
        long free = getFilesDir().getUsableSpace();
        if (free < need + 500_000_000L)
            throw new Exception("Not enough free space: the game needs " + gb(need + 500_000_000L) + " more, and "
                    + gb(free) + " is free.");
        for (Item it : items) {
            if (Thread.interrupted()) throw new InterruptedException();
            File f = new File(part, it.path);
            String key = it.path + " " + it.sha;
            if (verified.contains(key) && f.isFile() && f.length() == it.size) continue;
            File parent = f.getParentFile();
            if (parent != null && !parent.isDirectory() && !parent.mkdirs()) throw new Exception("cannot make " + parent);
            for (int attempt = 0; ; attempt++) {
                download(it, f);
                update("Checking " + it.path);
                if (sha256(f).equals(it.sha)) break;
                done -= f.length();
                if (!f.delete() || attempt >= 1) throw new Exception(it.path + " came out damaged twice. Try again later.");
            }
            verified.add(key);
            try (FileOutputStream o = new FileOutputStream(verifiedList, true)) {
                o.write((key + "\n").getBytes(StandardCharsets.UTF_8));
            }
        }
        update("Finishing");
        new File(part, "verified.txt").delete();
        if (game.exists()) deleteTree(game);
        if (!part.renameTo(game)) throw new Exception("cannot rename " + part + " to " + game);
        done = total;
        status = "The game is ready.";
    }

    private void download(Item it, File f) throws Exception {
        long have = f.isFile() ? f.length() : 0;
        if (have > it.size) {
            done -= have;
            f.delete();
            have = 0;
        }
        int errors = 0;
        while (have < it.size) {
            if (Thread.interrupted()) throw new InterruptedException();
            try {
                long got = fetchInto(REMOTE + "/" + it.path, have, f);
                have += got;
                errors = 0;
            } catch (InterruptedException e) {
                throw e;
            } catch (Exception e) {
                if (++errors >= TRIES) throw new Exception("The download keeps failing (" + e.getMessage() + "). Open the app to try again.");
                update("Waiting for the network (" + e.getMessage() + ")");
                Thread.sleep(Math.min(60_000L, 2000L * errors));
                have = f.isFile() ? f.length() : 0;
            }
        }
    }

    private interface Call { byte[] run() throws Exception; }

    private byte[] withRetries(Call call) throws Exception {
        for (int errors = 0; ; ) {
            try {
                return call.run();
            } catch (InterruptedException e) {
                throw e;
            } catch (Exception e) {
                if (++errors >= TRIES) throw e;
                update("Waiting for the network (" + e.getMessage() + ")");
                Thread.sleep(Math.min(60_000L, 2000L * errors));
            }
        }
    }

    private long last;

    // One request to Dropbox's download endpoint. With `into` null, returns the whole body; else appends from byte
    // `from` to `into` and returns null.
    private byte[] fetch(String path, long from, File into, boolean retried) throws Exception {
        HttpURLConnection c = (HttpURLConnection) new URL("https://content.dropboxapi.com/2/files/download").openConnection();
        c.setConnectTimeout(20000);
        c.setReadTimeout(60000);
        c.setRequestMethod("POST");
        c.setRequestProperty("Authorization", "Bearer " + DropboxAuth.access(this, retried));
        c.setRequestProperty("Dropbox-API-Arg", new JSONObject().put("path", path).toString());
        if (from > 0) c.setRequestProperty("Range", "bytes=" + from + "-");
        try {
            int code = c.getResponseCode();
            if (code == 401 && !retried) {
                c.disconnect();
                return fetch(path, from, into, true);
            }
            if (code == 409) {
                String body = new String(DropboxAuth.readAll(c.getErrorStream()), StandardCharsets.UTF_8);
                if (body.contains("not_found"))
                    throw new Exception("The game's files are not in Dropbox yet (" + path + " in the app folder).");
                throw new Exception("Dropbox answered 409: " + body);
            }
            if (code != 200 && code != 206) {
                InputStream es = c.getErrorStream();
                String body = es == null ? "" : new String(DropboxAuth.readAll(es), StandardCharsets.UTF_8);
                throw new java.io.IOException("Dropbox answered " + code + (body.isEmpty() ? "" : ": " + body));
            }
            if (into == null) return DropboxAuth.readAll(c.getInputStream());
            if (from > 0 && code != 206) throw new java.io.IOException("Dropbox did not resume the file");
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

    // A whole small file (the manifest).
    private byte[] fetchBytes(String path) throws Exception { return fetch(path, 0, null, false); }

    // Appends the rest of a file from byte `from`; returns the bytes appended this call.
    private long fetchInto(String path, long from, File into) throws Exception {
        long before = into.isFile() ? into.length() : 0;
        fetch(path, from, into, false);
        return into.length() - before;
    }

    private String sha256(File f) throws Exception {
        MessageDigest md = MessageDigest.getInstance("SHA-256");
        byte[] buf = new byte[1 << 20];
        try (FileInputStream in = new FileInputStream(f)) {
            int n;
            while ((n = in.read(buf)) > 0) md.update(buf, 0, n);
        }
        StringBuilder s = new StringBuilder();
        for (byte b : md.digest()) s.append(String.format("%02x", b));
        return s.toString();
    }

    static void deleteTree(File f) {
        File[] kids = f.listFiles();
        if (kids != null) for (File k : kids) deleteTree(k);
        f.delete();
    }
}
