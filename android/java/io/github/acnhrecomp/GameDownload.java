// The in-app game download (issue #230; SPEC M18): fetches the prepared game from the owner's own Dropbox (the
// app folder of save-relay's Dropbox app, acnh-universal-game/android/) into the app's private storage,
// <files>/game, with nothing of it outside the app. A foreground service with a progress notification, so it
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
import android.app.job.JobInfo;
import android.app.job.JobScheduler;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.os.Build;
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
import java.util.concurrent.ExecutionException;
import java.util.concurrent.ExecutorCompletionService;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

public class GameDownload extends Service {
    // Where the game waits, inside save-relay's Dropbox app folder.
    static final String REMOTE = "/acnh-universal-game/android";
    static final String DIR = "game";
    static final String CHANNEL = "download";
    static final int NOTE = 1;
    static final int DONE_NOTE = 2;  // the "ready" or "stopped" note, which outlives the service
    private static final int JOB_ID = 230;
    private static final int TRIES = 30;  // network errors in a row before it stops

    // What the home screen draws while it is open (read on its own thread; written here).
    static volatile boolean running;
    static volatile long done, total;
    static volatile String status = "", error;
    static volatile boolean finished;

    interface ProgressSink {
        void onProgress(String text, long done, long total);
        void onFinish(String fail);
    }

    // One worker per process, whatever happens to service instances.
    private static Thread worker;
    private static volatile PowerManager.WakeLock sharedWake;
    private static volatile ProgressSink activeSink;

    // An error a retry cannot fix: the download stops with this message.
    static final class Fatal extends Exception {
        Fatal(String m) { super(m); }
    }

    static File gameDir(Context c) { return new File(c.getFilesDir(), DIR); }

    // The downloaded game is complete only with its file list, which the download writes last.
    static boolean downloaded(Context c) {
        return new File(gameDir(c), "manifest.json").isFile() && new File(gameDir(c), "analysis/main.bin").isFile();
    }
    static File partialDir(Context c) { return new File(c.getFilesDir(), DIR + ".partial"); }

    static void ensureChannel(Context c) {
        NotificationManager nm = c.getSystemService(NotificationManager.class);
        nm.createNotificationChannel(new NotificationChannel(CHANNEL, "Game download", NotificationManager.IMPORTANCE_LOW));
    }

    // Starts the user-initiated data transfer job on Android 14+ (API 34+), or the dataSync
    // foreground service on older Android. Throws when Android refuses.
    static void start(Context c) {
        if (Build.VERSION.SDK_INT >= 34) {
            JobScheduler js = c.getSystemService(JobScheduler.class);
            JobInfo job = new JobInfo.Builder(JOB_ID, new ComponentName(c, GameDownloadJob.class))
                    .setUserInitiated(true)
                    .setRequiredNetworkType(JobInfo.NETWORK_TYPE_UNMETERED)
                    .setPersisted(true)
                    .build();
            if (js != null && js.schedule(job) == JobScheduler.RESULT_SUCCESS) {
                return;
            }
        }
        c.startForegroundService(new Intent(c, GameDownload.class));
    }

    static void startWorker(Context c, ProgressSink sink) {
        final Context appCtx = c.getApplicationContext();
        synchronized (GameDownload.class) {
            activeSink = sink;
            if (worker == null || !worker.isAlive()) {
                running = true;
                finished = false;
                error = null;
                if (sharedWake == null) {
                    sharedWake = appCtx.getSystemService(PowerManager.class)
                            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "acnh:download");
                }
                if (!sharedWake.isHeld()) {
                    sharedWake.acquire();
                }
                worker = new Thread(() -> new Runner(appCtx).work(), "game-download");
                worker.start();
            }
        }
    }

    static void stopWorker() {
        synchronized (GameDownload.class) {
            if (worker != null) worker.interrupt();
        }
        Runner.cancelAll();
        running = false;
        releaseWake();
    }

    private static void releaseWake() {
        try {
            if (sharedWake != null && sharedWake.isHeld()) sharedWake.release();
        } catch (Exception ignored) {}
    }

    @Override
    public IBinder onBind(Intent intent) { return null; }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        ensureChannel(this);
        Notification n = buildNote(this, "Starting the download", 0, 0);
        startForeground(NOTE, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC);
        startWorker(this, new ProgressSink() {
            @Override
            public void onProgress(String text, long have, long all) {
                getSystemService(NotificationManager.class).notify(NOTE, buildNote(GameDownload.this, text, have, all));
            }

            @Override
            public void onFinish(String fail) {
                stopForeground(STOP_FOREGROUND_REMOVE);
                postDoneNotification(GameDownload.this, fail);
                stopSelf();
            }
        });
        return START_NOT_STICKY;
    }

    // Android 15 and later: a dataSync service gets about 6 hours a day. Stop cleanly; what is fetched stays.
    @Override
    public void onTimeout(int startId, int fgsType) {
        error = "The download paused (Android's daily limit for downloads in the background). Open the app to go on.";
        stopWorker();
        stopForeground(STOP_FOREGROUND_REMOVE);
        stopSelf();
    }

    @Override
    public void onDestroy() {
        stopWorker();
        super.onDestroy();
    }

    // The app's launcher intent: brings the existing task (Home, or the game when it runs) to the front instead of
    // stacking a new Home on a running game.
    static PendingIntent openHome(Context c) {
        Intent open = c.getPackageManager().getLaunchIntentForPackage(c.getPackageName());
        if (open == null) open = new Intent(c, Home.class);
        open.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED);
        return PendingIntent.getActivity(c, 0, open, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    }

    static Notification buildNote(Context c, String text, long have, long all) {
        Notification.Builder b = new Notification.Builder(c, CHANNEL)
                .setSmallIcon(android.R.drawable.stat_sys_download)
                .setContentTitle("ACNH Recomp: game download")
                .setContentText(text)
                .setContentIntent(openHome(c))
                .setOnlyAlertOnce(true)
                .setOngoing(true);
        if (all > 0) b.setProgress(1000, (int) (have * 1000 / all), false);
        return b.build();
    }

    static void postDoneNotification(Context c, String fail) {
        Notification.Builder b = new Notification.Builder(c, CHANNEL)
                .setSmallIcon(fail == null ? android.R.drawable.stat_sys_download_done : android.R.drawable.stat_notify_error)
                .setContentTitle(fail == null ? "ACNH Recomp: the game is ready" : "ACNH Recomp: the download stopped")
                .setContentText(fail == null ? "Open the app to play." : fail)
                .setStyle(new Notification.BigTextStyle().bigText(fail == null ? "Open the app to play." : fail))
                .setAutoCancel(true)
                .setContentIntent(openHome(c));
        c.getSystemService(NotificationManager.class).notify(DONE_NOTE, b.build());
    }

    static String gb(long bytes) { return String.format(Locale.ROOT, "%.2f GB", bytes / 1e9); }

    private static final class Item {
        String path, sha;
        long size;
    }

    private static final class Runner {
        private final Context ctx;
        private volatile long last;

        Runner(Context ctx) {
            this.ctx = ctx;
        }

        private void update(String text) {
            if (stale()) return;
            status = text;
            ProgressSink sink = activeSink;
            if (sink != null) sink.onProgress(text, done, total);
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
            releaseWake();
            ProgressSink sink = activeSink;
            if (sink != null) sink.onFinish(fail);
        }

    // Waits for the Dropbox sign-in the home screen started (DropboxAuth), up to ten minutes.
    private void awaitSignIn() throws Exception {
        long until = SystemClock.elapsedRealtime() + 10 * 60 * 1000;
        while (!DropboxAuth.signedIn(ctx)) {
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
        final int gen;
        synchronized (GameDownload.class) { gen = ++generation; }
        runOf.set(gen);
        awaitSignIn();
        File part = partialDir(ctx), game = gameDir(ctx);
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
        // Which version each partial file belongs to: one left from an older list is started over.
        File startedList = new File(part, "started.txt");
        Map<String, String> started = new HashMap<>();
        if (startedList.isFile())
            for (String l : new String(Files.readAllBytes(startedList.toPath()), StandardCharsets.UTF_8).split("\n")) {
                int sp = l.lastIndexOf(' ');
                if (sp > 0) started.put(l.substring(0, sp), l.substring(sp + 1).trim());
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
            boolean moved = verified.contains(it.path + " " + it.sha) && !new File(part, it.path).exists();
            if ((it.sha.equals(installed.get(it.path)) || moved) && old.isFile() && old.length() == it.size) {
                have += it.size;  // on disk already: needs no space and no fetch
                if (moved) fetch.add(it);  // moved by a run that stopped mid-move: the move loop and the list still run
                continue;
            }
            fetch.add(it);
            File f = new File(part, it.path);
            if (f.exists() && !it.sha.equals(started.get(it.path))) f.delete();
            long len = f.isFile() ? Math.min(f.length(), it.size) : 0;
            have += len;
            need += it.size - len;
        }
        total = all;
        done = have;
        if (fetch.isEmpty() && installedList.isFile()) {
            Runner.deleteTree(part);
            status = "The game is up to date.";
            return;
        }
        long free = ctx.getFilesDir().getUsableSpace();
        if (free < need + 500_000_000L)
            throw new Fatal("Not enough free space: the game needs " + gb(need + 500_000_000L) + " more, and "
                    + gb(free) + " is free.");

        // Small files (the DLC's ~1,200, fonts, analysis) several at a time: each is mostly a round trip. Then
        // the big ones one after another, each one stream at the line's full speed.
        final Lists lists = new Lists(startedList, started, verifiedList, verified);
        List<Item> small = new ArrayList<>(), big = new ArrayList<>();
        for (Item it : fetch) (it.size < SMALL ? small : big).add(it);
        ExecutorService pool = Executors.newFixedThreadPool(PARALLEL);
        boolean ok = false;
        try {
            ExecutorCompletionService<Void> jobs = new ExecutorCompletionService<>(pool);
            for (final Item it : small)
                jobs.submit(() -> {
                    runOf.set(gen);
                    fetchOne(it, part, game, lists);
                    return null;
                });
            for (int i = 0; i < small.size(); i++) {
                try {
                    jobs.take().get();  // the first failure, whichever file it was, at once
                } catch (ExecutionException e) {
                    Throwable c = e.getCause();
                    if (c instanceof Exception) throw (Exception) c;
                    throw new Exception(c);
                }
            }
            ok = true;
        } finally {
            if (!ok) cancelAll();  // the other fetches stop now, not after their next read times out
            pool.shutdownNow();
            try {
                pool.awaitTermination(10, TimeUnit.SECONDS);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        }
        for (Item it : big) fetchOne(it, part, game, lists);

        // Every file checks out: move them into the game folder, then the list. The player's own files stay.
        update("Finishing");
        if (!game.isDirectory() && !game.mkdirs()) throw new Fatal("cannot make " + game);
        new File(game, "manifest.json").delete();  // a half-updated folder is never taken for a finished one
        for (Item it : fetch) {
            File from = new File(part, it.path), to = new File(game, it.path);
            if (!from.exists() && to.isFile() && to.length() == it.size) continue;  // moved by an earlier run
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

    private static final long SMALL = 16L << 20;  // under 16 MB: fetched in parallel
    private static final int PARALLEL = 6;

    // started.txt and verified.txt with their in-memory copies, shared by the parallel fetches.
    private static final class Lists {
        final File startedList, verifiedList;
        final Map<String, String> started;
        final Set<String> verified;
        Lists(File s, Map<String, String> sm, File v, Set<String> vs) { startedList = s; started = sm; verifiedList = v; verified = vs; }
        synchronized boolean isVerified(String key) { return verified.contains(key); }
        synchronized void start(Item it) throws Exception {
            if (it.sha.equals(started.get(it.path))) return;
            try (FileOutputStream o = new FileOutputStream(startedList, true)) {
                o.write((it.path + " " + it.sha + "\n").getBytes(StandardCharsets.UTF_8));
            }
            started.put(it.path, it.sha);
        }
        synchronized void verify(String key) throws Exception {
            verified.add(key);
            try (FileOutputStream o = new FileOutputStream(verifiedList, true)) {
                o.write((key + "\n").getBytes(StandardCharsets.UTF_8));
            }
        }
    }

    // Each run has a generation; a thread of an older run (a fetch still blocked in a read when its run ended)
    // finds its generation out of date and stops without touching the counter, the files or the notification.
    private static volatile int generation;
    private static final ThreadLocal<Integer> runOf = new ThreadLocal<>();
    private static final Set<HttpURLConnection> open = java.util.Collections.newSetFromMap(new java.util.concurrent.ConcurrentHashMap<HttpURLConnection, Boolean>());

    private static boolean stale() {
        Integer g = runOf.get();
        return g != null && g != generation;
    }

    // Ends every fetch of the current run at once: a blocked read in HttpURLConnection ignores interrupts but
    // throws when its connection is closed.
    static synchronized void cancelAll() {
        generation++;
        for (HttpURLConnection c : open) {
            try { c.disconnect(); } catch (Exception ignored) {}
        }
        open.clear();
    }

    private static synchronized void addDone(long n) {
        if (!stale()) done += n;
    }

    // One file: fetched (resumed), checked against its hash, recorded as verified.
    private void fetchOne(Item it, File part, File game, Lists lists) throws Exception {
        if (Thread.interrupted()) throw new InterruptedException();
        File f = new File(part, it.path);
        String key = it.path + " " + it.sha;
        if (lists.isVerified(key) && f.isFile() && f.length() == it.size) return;
        File movedTo = new File(game, it.path);
        if (lists.isVerified(key) && !f.exists() && movedTo.isFile() && movedTo.length() == it.size) return;
        lists.start(it);
        File parent = f.getParentFile();
        if (parent != null && !parent.isDirectory() && !parent.mkdirs() && !parent.isDirectory())
            throw new Fatal("cannot make " + parent);
        for (int attempt = 0; ; attempt++) {
            download(it, f);
            if (it.size > SMALL) update("Checking " + it.path);
            if (sha256(f).equals(it.sha)) break;
            addDone(-f.length());
            f.delete();
            if (attempt >= 1) throw new Fatal(it.path + " came out damaged twice. Try again later.");
        }
        lists.verify(key);
    }

    private void download(Item it, File f) throws Exception {
        long have = f.isFile() ? f.length() : 0;
        if (have > it.size) {
            addDone(-Math.min(have, it.size));
            f.delete();
            have = 0;
        }
        if (it.size == 0) {
            if (!f.exists() && !f.createNewFile()) throw new Fatal("cannot make " + f);
            return;
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
                if (f.isFile() && f.length() > have) errors = 0;  // it was getting somewhere: a dropped connection
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
        ConnectivityManager cm = ctx.getSystemService(ConnectivityManager.class);
        while (true) {
            Network n = cm.getActiveNetwork();
            NetworkCapabilities caps = n == null ? null : cm.getNetworkCapabilities(n);
            if (caps != null && caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) && unmetered(caps)) return;
            // A VPN reports itself; what counts is the network under it: Wi-Fi or Ethernet that is not metered.
            if (caps != null && caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN))
                for (Network other : cm.getAllNetworks()) {
                    NetworkCapabilities o = cm.getNetworkCapabilities(other);
                    if (o != null && !o.hasTransport(NetworkCapabilities.TRANSPORT_VPN) && unmetered(o)
                            && (o.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) || o.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET))
                            && !anyCellular(cm))
                        return;
                }
            update(n == null ? "Waiting for the network" : "Waiting for Wi-Fi (the download never uses mobile data)");
            Thread.sleep(5000);
        }
    }

    private static boolean unmetered(NetworkCapabilities c) {
        return c.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)
                || (android.os.Build.VERSION.SDK_INT >= 30 && c.hasCapability(NetworkCapabilities.NET_CAPABILITY_TEMPORARILY_NOT_METERED));
    }

    // True when the device's default route under a VPN could be mobile data (no Wi-Fi or Ethernet up).
    private static boolean anyCellular(ConnectivityManager cm) {
        for (Network other : cm.getAllNetworks()) {
            NetworkCapabilities o = cm.getNetworkCapabilities(other);
            if (o != null && (o.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) || o.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET))
                    && o.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED))
                return false;
        }
        return true;
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

    private byte[] fetch(String path, long from, File into, boolean retried) throws Exception {
        awaitUnmetered();
        if (stale()) throw new InterruptedException();
        HttpURLConnection c = (HttpURLConnection) new URL("https://content.dropboxapi.com/2/files/download").openConnection();
        open.add(c);
        c.setConnectTimeout(20000);
        c.setReadTimeout(60000);
        c.setRequestMethod("POST");
        // Android's HTTP stack adds a form Content-Type to a POST without one, which this endpoint refuses (400).
        c.setRequestProperty("Content-Type", "application/octet-stream");
        c.setRequestProperty("Accept-Encoding", "identity");
        c.setRequestProperty("Authorization", "Bearer " + DropboxAuth.access(ctx, retried));
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
                    DropboxAuth.signOut(ctx);
                    throw new Fatal("Dropbox refused the sign-in. Sign in again.");
                }
                if (code >= 400 && code < 500 && code != 408 && code != 429)
                    throw new Fatal("Dropbox answered " + code + (body.isEmpty() ? "" : ": " + body));
                throw new IOException("Dropbox answered " + code);
            }
            if (into == null) return DropboxAuth.readAll(c.getInputStream());
            if (from > 0 && code != 206) throw new IOException("Dropbox did not resume the file");
            long length = c.getContentLengthLong();  // -1 when not sent: the full buffer
            byte[] buf = new byte[(int) (length <= 0 ? 1 << 20 : Math.min(1 << 20, Math.max(8192, length)))];
            try (InputStream in = c.getInputStream(); FileOutputStream out = new FileOutputStream(into, true)) {
                int n;
                while ((n = in.read(buf)) > 0) {
                    if (stale()) throw new InterruptedException();
                    out.write(buf, 0, n);
                    addDone(n);
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
            open.remove(c);
            c.disconnect();
        }
    }

    private String sha256(File f) throws Exception {
        MessageDigest md = MessageDigest.getInstance("SHA-256");
        byte[] buf = new byte[(int) Math.min(1 << 20, Math.max(8192, f.length()))];
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
}
