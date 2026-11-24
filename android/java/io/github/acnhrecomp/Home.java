// The app's first screen: asks for the game folder, checks it and starts the game (android.app.NativeActivity,
// lib<game>.so). The APK holds no game: the folder is one prepared from the player's own copy, on the phone itself
// (Prepare.java, from the ExeFS and RomFS they extracted with their emulator), by ACNHRecomp on a computer or by
// ACNHRecomp-Importer (game/android-arm64/libacnh_game.so, romfs.pak, spirv.pak, ...; docs/ANDROID.md). A folder
// holding exefs/ and romfs/ that is no game folder yet opens Prepare. The folder chosen is kept in
// <files>/data_root.txt, which platform_android.cpp reads; next time the game starts right away. The launcher icon's
// shortcut "Change game folder" (res/xml/shortcuts.xml) asks again.
// Or the game comes from the owner's own Dropbox (issue #230): "Download the game" signs in (DropboxAuth) and
// GameDownload fetches it into the app's private storage, <files>/gamedata, which then is the game folder.
// Save sync (issue #81, Relay): before the game starts, the island is pulled from the owner's Dropbox through
// save-relay, with its dialogs here; "Set up save sync" signs in (the first start also offers it once).
package io.github.acnhrecomp;

import android.app.Activity;
import android.content.Intent;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Environment;
import android.os.Handler;
import android.os.Looper;
import android.provider.DocumentsContract;
import android.provider.Settings;
import android.view.View;
import android.widget.LinearLayout;
import android.widget.ProgressBar;
import android.widget.TextView;

import java.io.File;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.ArrayList;
import java.util.List;

import static io.github.acnhrecomp.Screen.t;

public class Home extends Activity {
    static {
        System.loadLibrary("acnh");
    }

    // platform_android.cpp: "" when the game library in `folder` loads (copied into `files` first), else why not.
    private static native String nativeLoadGame(String folder, String files);

    private static final int PICK = 1;
    private static final String GAME_LIB = "libacnh_game.so";

    private LinearLayout box;
    private TextView message;
    private String waitingForAccess;  // a folder picked before the all-files permission was given
    private boolean busy;
    private boolean askedNotifications;
    private TextView progress;
    private ProgressBar bar;
    private final Handler ticks = new Handler(Looper.getMainLooper());

    // The app started again in a new process (the chosen folder is saved: it opens straight into it). Android keeps a
    // closed app's process, with the game of the last folder loaded, so "close and open again" was not enough.
    private void restartProcess() {
        Intent i = getPackageManager().getLaunchIntentForPackage(getPackageName());
        if (i != null) {
            i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK);
            startActivity(i);
        }
        finishAffinity();
        // as platform_android.cpp's restart_app (_exit): no atexit handlers or static destructors with native threads
        // still running
        android.os.Process.killProcess(android.os.Process.myPid());
    }

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        if (Relay.running()) {  // a pull is waiting (on a dialog, or the browser's sign-in): this screen shows it
            box = Screen.page(this, "ACNH Recomp");
            box.addView(Screen.text(this, t("Syncing your island with Dropbox...", "Sincronizando a sua ilha com o Dropbox..."),
                    16, Screen.TEXT));
            Relay.attach(this);
            return;
        }
        String error = getIntent().getStringExtra("error");
        if ("restart".equals(error)) {  // another folder than the game loaded in this process: a new process takes it
            restartProcess();
            return;
        }
        if (PrepareService.active()) {  // the game is being prepared: its screen
            startActivity(new Intent(this, Prepare.class));
            finish();
            return;
        }
        boolean signedInReturn = getIntent().getData() != null && "acnhrecomp".equals(getIntent().getData().getScheme());
        boolean ask = getIntent().getBooleanExtra("pick", false) || error != null || signedInReturn;
        if (ask) GameDownload.finished = false;  // "Change game folder" or an error: no automatic start
        // While a download runs the game does not start by itself: leaving the game ends the app's process, and
        // the download with it.
        String saved = ask || GameDownload.running ? "" : readSaved();
        if (!saved.isEmpty() && hasPackage(new File(saved)) && canRead(saved)
                && (!saved.equals(GameDownload.gameDir(this).getAbsolutePath()) || GameDownload.downloaded(this))) {
            check(saved, true);  // the folder of last time: straight into the game
            return;
        }
        File downloaded = GameDownload.gameDir(this);
        if (!ask && !GameDownload.running && GameDownload.downloaded(this)) {
            check(downloaded.getAbsolutePath(), true);  // the game downloaded from Dropbox
            return;
        }
        show(error != null ? explain(error) : null);
        if (error != null) offerAgain(readSaved(), error);
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        if (!Relay.running() && intent != null && intent.getBooleanExtra("pick", false)) {
            GameDownload.finished = false;
            show(null);
        }
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        Relay.detach(this);
    }

    @Override
    protected void onResume() {
        super.onResume();
        DropboxAuth.setForeground(true);
        Relay.attach(this);
        if (Relay.running()) return;
        // the game being prepared, or prepared and its end not seen yet: its screen (Android can bring this task to
        // the front with Home on top when the preparation's notification is tapped)
        if (PrepareService.active() || (PrepareService.phase == PrepareService.FINISHED && PrepareService.result != null)) {
            startActivity(new Intent(this, Prepare.class));
            return;
        }
        if (waitingForAccess != null && Environment.isExternalStorageManager()) {
            String f = waitingForAccess;
            waitingForAccess = null;
            check(f, false);
        }
        ticks.removeCallbacksAndMessages(null);
        ticks.post(this::tick);
    }

    @Override
    protected void onPause() {
        super.onPause();
        DropboxAuth.setForeground(false);
        ticks.removeCallbacksAndMessages(null);
    }

    // ---- the download -----------------------------------------------------------------------------------------

    // The notification permission first (the download's progress shows there), then the sign-in or the download.
    private void download() {
        if (GameDownload.running) {
            if (DropboxAuth.signedIn(this)) return;
            // Still waiting on a sign-in the browser left: start a new one (it supersedes the old), same download.
            try {
                String url = DropboxAuth.start(this);
                startActivity(new Intent(Intent.ACTION_VIEW, Uri.parse(url)));
            } catch (Exception e) {
                say(t("Dropbox sign-in could not start: ", "O login no Dropbox não começou: ") + e.getMessage(), true);
            }
            return;
        }
        if (Build.VERSION.SDK_INT >= 33 && !askedNotifications
                && checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) != android.content.pm.PackageManager.PERMISSION_GRANTED) {
            askedNotifications = true;
            requestPermissions(new String[] {android.Manifest.permission.POST_NOTIFICATIONS}, 2);
            return;  // onRequestPermissionsResult goes on
        }
        String url = null;
        if (!DropboxAuth.signedIn(this)) {
            try {
                url = DropboxAuth.start(this);
            } catch (Exception e) {
                say(t("Dropbox sign-in could not start: ", "O login no Dropbox não começou: ") + e.getMessage(), true);
                return;
            }
        }
        // The service starts now, while the app is on screen (Android refuses it from the background); it waits for
        // the sign-in, and keeps the app awake for the browser's return to DropboxAuth's server.
        GameDownload.error = null;
        GameDownload.status = t("Starting the download", "Começando o download");
        GameDownload.running = true;
        try {
            GameDownload.start(this);
        } catch (Exception e) {
            GameDownload.running = false;
            say(t("The download could not start: ", "O download não começou: ") + e.getMessage(), true);
            return;
        }
        if (url != null) {
            say(t("Sign in to Dropbox in the browser, allow ACNH Recomp's access, then come back here. The same "
                            + "sign-in turns save sync on: your island then goes through your Dropbox too.",
                    "Entre no Dropbox no navegador, permita o acesso do ACNH Recomp e volte aqui. O mesmo login liga "
                            + "a sincronização: a sua ilha passa a ir pelo seu Dropbox também."), false);
            startActivity(new Intent(Intent.ACTION_VIEW, Uri.parse(url)));
        }
        tick();
    }

    @Override
    public void onRequestPermissionsResult(int request, String[] permissions, int[] results) {
        super.onRequestPermissionsResult(request, permissions, results);
        if (request == 2) download();  // with or without the permission: the download runs either way
    }

    // Twice a second while the screen is open: the download's progress, and the game when it is done.
    private void tick() {
        if (progress != null) {
            String e = GameDownload.error;
            if (GameDownload.running) {
                if (message != null && DropboxAuth.signedIn(this)) message.setVisibility(View.GONE);  // the sign-in note
                long all = GameDownload.total, have = GameDownload.done;
                progress.setText((all > 0 ? (have * 100 / all) + "%  ·  " : "") + GameDownload.status);
                progress.setVisibility(View.VISIBLE);
                bar.setIndeterminate(all <= 0);
                if (all > 0) bar.setProgress((int) (have * 1000 / all));
                bar.setVisibility(View.VISIBLE);
            } else if (e != null) {
                progress.setText(e);
                progress.setVisibility(View.VISIBLE);
                bar.setVisibility(View.GONE);
            } else if (GameDownload.downloaded(this) && GameDownload.finished) {
                GameDownload.finished = false;
                progress.setVisibility(View.GONE);
                check(GameDownload.gameDir(this).getAbsolutePath(), false);
                return;
            }
        }
        ticks.postDelayed(this::tick, 500);
    }

    // ---- the screen -------------------------------------------------------------------------------------------

    private void show(String note) {
        box = Screen.page(this, "ACNH Recomp");
        box.addView(Screen.text(this, t("Download the game your computer prepared from your own copy into your Dropbox "
                        + "(about 12 GB; it keeps going in the background and picks up where it stopped), or choose the "
                        + "game folder: one prepared before (on this phone or on a computer), or the folder holding the "
                        + "exefs and romfs you extracted from your own copy of the game with your emulator, which the app "
                        + "then prepares here.",
                "Baixe o jogo que o seu computador preparou a partir da sua cópia no seu Dropbox (cerca de 12 GB; "
                        + "continua em segundo plano e retoma de onde parou), ou escolha a pasta do jogo: uma preparada "
                        + "antes (neste celular ou num computador), ou a pasta com o exefs e o romfs que você extraiu da "
                        + "sua cópia do jogo com o seu emulador, que o app então prepara aqui."), 16, Screen.TEXT));
        message = Screen.text(this, note == null ? "" : note, 15, Screen.RED);
        message.setVisibility(note == null ? View.GONE : View.VISIBLE);
        box.addView(message);

        box.addView(Screen.button(this, t("Download the game from Dropbox", "Baixar o jogo do Dropbox"), Screen.GREEN,
                v -> download()));
        progress = new TextView(this);
        progress.setTextSize(15);
        progress.setTextColor(Screen.BROWN);
        progress.setPadding(0, Screen.dp(this, 8), 0, 0);
        progress.setVisibility(View.GONE);
        box.addView(progress);
        bar = new ProgressBar(this, null, android.R.attr.progressBarStyleHorizontal);
        bar.setMax(1000);
        bar.setVisibility(View.GONE);
        box.addView(bar, new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, Screen.dp(this, 14)));
        if (GameDownload.downloaded(this))
            box.addView(Screen.button(this, t("Play the downloaded game", "Jogar o jogo baixado"), Screen.GREEN,
                    v -> check(GameDownload.gameDir(this).getAbsolutePath(), false)));
        showSync();
        if (DropboxAuth.signedIn(this))
            box.addView(Screen.button(this, t("Sign out of Dropbox (ends save sync and the download)",
                    "Sair do Dropbox (encerra a sincronização e o download)"), Screen.WHITE, v -> {
                if (GameDownload.running) {
                    say(t("Wait for the download to finish first.", "Espere o download terminar."), true);
                    return;
                }
                boolean done = DropboxAuth.signOutAll(this);  // the download's sign-in and save sync's are one
                show(null);
                if (done) say(t("Signed out of Dropbox: save sync and the download are off on this device.",
                        "Saiu do Dropbox: a sincronização e o download estão desligados neste aparelho."), false);
                else say(t("The Dropbox sign-out could not remove everything. Try again.",
                        "Não foi possível remover tudo ao sair do Dropbox. Tente de novo."), true);
            }));

        for (final String f : found())
            box.addView(Screen.button(this, t("Use the game found", "Usar o jogo encontrado") + " ("
                            + (f.startsWith("/storage/emulated") ? t("phone", "celular") : t("memory card", "cartão de memória"))
                            + ")", Screen.GREEN, v -> check(f, false)));
        box.addView(Screen.button(this, t("Choose the game folder", "Escolher a pasta do jogo"), Screen.BLUE, v -> pick()));
    }

    // Save sync: its state, and the sign-in (or turning it off). The pull itself runs when the game starts.
    private void showSync() {
        int state = Relay.state(this);
        box.addView(Screen.heading(this, t("Save sync", "Sincronização do save")));
        box.addView(Screen.text(this, state == Relay.ON
                        ? t("On: your island goes through your Dropbox, fetched before the game starts and put back when "
                                + "you leave.", "Ligada: a sua ilha passa pelo seu Dropbox, buscada antes de o jogo "
                                + "começar e devolvida quando você sai.")
                        : state == Relay.SIGN_IN_AGAIN
                        ? t("Dropbox needs a new sign-in on this device before the game can start.",
                            "O Dropbox precisa de um novo login neste aparelho antes de o jogo começar.")
                        : state == Relay.BROKEN
                        ? t("Save sync's settings on this device cannot be read. Turn save sync off to play with "
                                + "the island on this device, or sign in to Dropbox again.",
                            "As configurações da sincronização neste aparelho não podem ser lidas. Desligue a "
                                + "sincronização para jogar com a ilha deste aparelho, ou entre no Dropbox de novo.")
                        : state == Relay.OFF && DropboxAuth.hasLegacyDownloadToken(this)
                        ? t("Off. Your earlier Dropbox sign-in was for downloading the game only (read-only). Set up "
                                + "save sync to sign in once more for both the download and your island.",
                            "Desligada. O seu login anterior no Dropbox era só para baixar o jogo (somente leitura). "
                                + "Ligue a sincronização para entrar de novo para o download e a sua ilha.")
                        : t("Off. Set it up to keep your island in your Dropbox and play it on your other devices.",
                            "Desligada. Ligue para guardar a sua ilha no seu Dropbox e jogar nos seus outros aparelhos."),
                15, Screen.TEXT));
        if (state == Relay.SIGN_IN_AGAIN || state == Relay.BROKEN)
            box.addView(Screen.button(this, t("Sign in to Dropbox again", "Entrar no Dropbox de novo"), Screen.BLUE,
                    v -> setUpSync()));
        else if (state == Relay.OFF)
            box.addView(Screen.button(this, t("Set up save sync (Dropbox)", "Ligar a sincronização (Dropbox)"),
                    Screen.BLUE, v -> setUpSync()));
        if (state != Relay.OFF)  // also the way out when the sign-in is gone: the game then starts with sync off
            box.addView(Screen.button(this, t("Turn off save sync (the download stays signed in)",
                    "Desligar a sincronização (o download continua com login)"), Screen.WHITE, v -> {
                boolean done = Relay.turnOff(this);
                show(null);
                if (done) say(t("Save sync is off on this device. Your island here stays as it is.",
                        "A sincronização está desligada neste aparelho. A sua ilha aqui fica como está."), false);
                else say(t("Save sync is off, but not everything could be removed. Try again.",
                        "A sincronização foi desligada, mas nem tudo pôde ser removido. Tente de novo."), true);
            }));
    }

    private void setUpSync() {
        if (busy || GameDownload.running) return;
        busy = true;
        say(t("Sign in to Dropbox in the browser, allow ACNH Recomp's access, then come back here.",
                "Entre no Dropbox no navegador, permita o acesso do ACNH Recomp e volte aqui."), false);
        new Thread(() -> {
            final String failed = Relay.signIn(false);
            runOnUiThread(() -> {
                busy = false;
                if (isFinishing()) return;
                show(null);
                if (failed == null) say(t("Save sync is on.", "A sincronização está ligada."), false);
                else say(t("Dropbox sign-in: ", "Login no Dropbox: ") + failed, true);
            });
        }, "save sync setup").start();
    }

    private void say(String text, boolean error) {
        if (message == null) return;
        message.setTextColor(error ? Screen.RED : Screen.GREEN_TEXT);
        message.setText(text);
        message.setVisibility(View.VISIBLE);
    }

    private String explain(String why) {
        if (why.startsWith("abi"))
            return t("The game in this folder was prepared for another version of the app. Prepare it again (on this "
                            + "phone, below, or on a computer) or install the matching app.",
                    "O jogo desta pasta foi preparado para outra versão do app. Prepare de novo (neste celular, "
                            + "abaixo, ou num computador) ou instale o app da mesma versão.");
        if (why.startsWith("missing"))
            return t("This folder has neither a prepared game (" + GAME_LIB + ") nor the game's extracted files. With "
                            + "your emulator, extract the ExeFS and RomFS of your own copy of the game, with update "
                            + "3.0.3 (Ryujinx: right-click the game, Extract Data; yuzu or Eden: Dump RomFS, and Dump "
                            + "ExeFS in the debug settings), into one folder as exefs and romfs, copy that folder to "
                            + "the phone and choose it here: the app prepares the game from it. A computer does that "
                            + "faster: ACNHRecomp's launcher there prepares a folder for Android too.",
                    "Esta pasta não tem nem o jogo preparado (" + GAME_LIB + ") nem os arquivos extraídos do jogo. "
                            + "Com o seu emulador, extraia o ExeFS e o RomFS da sua cópia do jogo, com a atualização "
                            + "3.0.3 (Ryujinx: botão direito no jogo, Extract Data; yuzu ou Eden: Dump RomFS, e Dump "
                            + "ExeFS nas configurações de depuração), numa pasta como exefs e romfs, copie essa pasta "
                            + "para o celular e escolha-a aqui: o app prepara o jogo a partir dela. Um computador faz "
                            + "isso mais rápido: o launcher do ACNHRecomp nele também prepara uma pasta para Android.");
        if (why.contains("cannot locate symbol"))  // a library from ACNHRecomp-Importer before 1.0.4 (no DT_NEEDED)
            return t("The app can't load the game in this folder. Usually it was prepared by an older "
                            + "ACNHRecomp-Importer (before 1.0.4): prepare it again (on this phone, below, or on a "
                            + "computer), which keeps what was extracted and builds the library again. If it was "
                            + "prepared by something newer, update this app.\n\n" + why,
                    "O app não consegue carregar o jogo desta pasta. Normalmente ela foi preparada por um "
                            + "ACNHRecomp-Importer antigo (anterior ao 1.0.4): prepare de novo (neste celular, abaixo, "
                            + "ou num computador), o que reaproveita o que foi extraído e refaz a biblioteca. Se foi "
                            + "preparada por algo mais novo, atualize este app.\n\n" + why);
        if (why.startsWith("gpu: "))
            return t("This phone's graphics driver cannot run the game: " + why.substring(5) + ". On a Snapdragon "
                            + "phone, the Turnip driver in the app is used; other phones need a Vulkan 1.3 driver.",
                    "O driver gráfico deste aparelho não roda o jogo: " + why.substring(5) + ". Em aparelhos "
                            + "Snapdragon o app usa o driver Turnip; os outros precisam de um driver Vulkan 1.3.");
        if (why.equals("restart"))
            return t("Close the app and open it again to use another folder.",
                    "Feche o app e abra de novo para usar outra pasta.");
        return why;
    }

    // ---- folders ----------------------------------------------------------------------------------------------

    // The game's files as the player's emulator extracted them, not prepared yet: exefs/ and romfs/ (any case; or
    // extract/romfs/), or yuzu's and Eden's dump folder of the game (01006F8002326000/). prepare/io/inputs.h says it.
    static boolean extracted(File f) {
        if (f.getName().equalsIgnoreCase("01006F8002326000")) return true;
        boolean exefs = false, romfs = false;
        File[] in = f.listFiles();
        if (in == null) return false;
        for (File c : in) {
            if (!c.isDirectory()) continue;
            String n = c.getName();
            if (n.equalsIgnoreCase("exefs")) exefs = true;
            else if (n.equalsIgnoreCase("romfs") || n.equalsIgnoreCase("01006F8002326000")) romfs = true;
            else if (n.equalsIgnoreCase("extract") && new File(c, "romfs").isDirectory()) romfs = true;
            if (n.equalsIgnoreCase("01006F8002326000")) exefs = true;
        }
        return exefs && romfs;
    }

    private void openPrepare(String folder, String again) {
        Intent i = new Intent(this, Prepare.class).putExtra("source", folder);
        if (again != null) i.putExtra("out", again);
        startActivity(i);
    }

    // A game folder this app cannot load (prepared for another version, or by an Importer before 1.0.4) but holding
    // the game's ExeFS: prepared again here, where it is.
    private void offerAgain(final String folder, String why) {
        if (folder.isEmpty() || !(why.startsWith("abi") || why.contains("cannot locate symbol"))
                || !new File(folder, "exefs").isDirectory() || box == null)
            return;
        box.addView(Screen.button(this, t("Prepare it again on this phone", "Preparar de novo neste celular"),
                Screen.GREEN, v -> openPrepare(folder, folder)));
    }

    private static boolean hasPackage(File f) {
        // the game folder ACNHRecomp-Importer prepares (game/android-arm64/), or one tools/android_install.sh filled
        return (new File(f, "game/android-arm64/" + GAME_LIB).isFile() || new File(f, GAME_LIB).isFile())
                && new File(f, "analysis/main.bin").isFile();
    }

    // The app's own folders (phone and memory card) where tools/android_install.sh puts the game: readable without
    // any permission, and not offered by the system's folder picker.
    private List<String> found() {
        List<String> out = new ArrayList<>();
        for (File f : getExternalFilesDirs(null))
            if (f != null && hasPackage(f)) out.add(f.getAbsolutePath());
        return out;
    }

    private boolean ownFolder(String path) {
        for (File f : getExternalFilesDirs(null))
            if (f != null && path.startsWith(f.getAbsolutePath())) return true;
        return false;
    }

    private boolean canRead(String path) {
        return path.startsWith(getFilesDir().getAbsolutePath()) || ownFolder(path) || Environment.isExternalStorageManager();
    }

    private String readSaved() {
        try {
            return new String(Files.readAllBytes(new File(getFilesDir(), "data_root.txt").toPath()),
                    StandardCharsets.UTF_8).trim();
        } catch (Exception e) {
            return "";
        }
    }

    private void pick() {
        Intent i = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
        try {
            startActivityForResult(i, PICK);
        } catch (Exception e) {
            say(t("This phone has no folder picker.", "Este celular não tem seletor de pastas."), true);
        }
    }

    // A folder from the system picker as a path: "primary:Games/ACNH" is the phone's storage, "7EFF-EC74:ACNH" a
    // memory card (/storage/7EFF-EC74).
    private static String pathOf(Uri tree) {
        String id = DocumentsContract.getTreeDocumentId(tree);
        int colon = id.indexOf(':');
        String volume = colon < 0 ? id : id.substring(0, colon), rest = colon < 0 ? "" : id.substring(colon + 1);
        String base = volume.equals("primary") ? Environment.getExternalStorageDirectory().getAbsolutePath()
                                               : "/storage/" + volume;
        return rest.isEmpty() ? base : base + "/" + rest;
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request != PICK || result != RESULT_OK || data == null || data.getData() == null) return;
        String path = pathOf(data.getData());
        if (message == null) show(null);
        if (!canRead(path)) {
            // Reading the game's files from a folder of the phone's storage needs the all-files permission.
            waitingForAccess = path;
            say(t("Allow access to all files for ACNH Recomp, then come back.",
                    "Permita o acesso a todos os arquivos para o ACNH Recomp e volte."), false);
            Intent i = new Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                    Uri.parse("package:" + getPackageName()));
            startActivity(i);
            return;
        }
        check(path, false);
    }

    // Loads the game library from the folder (in the background: the first time it is copied, a few seconds) and
    // starts the game, or says why not.
    private void check(final String folder, final boolean quiet) {
        if (busy) return;
        GameDownload.finished = false;
        if (GameDownload.running) {
            if (message == null) show(null);
            say(t("Wait for the download to finish: leaving the game would stop it.",
                    "Espere o download terminar: sair do jogo o interromperia."), true);
            return;
        }
        ((android.app.NotificationManager) getSystemService(NOTIFICATION_SERVICE)).cancel(GameDownload.DONE_NOTE);
        if (!hasPackage(new File(folder))) {
            if (extracted(new File(folder))) {  // the game's extracted files: prepared on the phone
                if (message == null) show(null);
                openPrepare(folder, null);
                return;
            }
            if (message == null) show(null);
            say(explain("missing") + "\n" + folder, true);
            return;
        }
        busy = true;
        if (!quiet) {
            if (message == null) show(null);
            say(t("Preparing the game...", "Preparando o jogo..."), false);
            ProgressBar p = new ProgressBar(this);
            p.setTag("progress");
            box.addView(p);
        }
        new Thread(() -> {
            final String why = nativeLoadGame(folder, getFilesDir().getAbsolutePath());
            runOnUiThread(() -> {
                busy = false;
                if (box != null) {
                    View p = box.findViewWithTag("progress");
                    if (p != null) box.removeView(p);
                }
                if (!why.isEmpty() && !why.equals("restart")) {
                    if (message == null) show(null);
                    say(explain(why), true);
                    offerAgain(folder, why);
                    return;
                }
                boolean saved = true;
                try (FileOutputStream o = new FileOutputStream(new File(getFilesDir(), "data_root.txt"))) {
                    o.write((folder + "\n").getBytes(StandardCharsets.UTF_8));
                } catch (Exception e) {
                    saved = false;  // the game still starts; next time the folder is asked for again
                }
                if (why.equals("restart")) {  // this process has another folder's game loaded: a new one takes this folder
                    if (saved) restartProcess();
                    else say(explain(why), true);  // not saved: a new process would not know the folder
                    return;
                }
                // The island first (save sync: Relay), then the game, which starts once the pull lets it.
                if (!quiet) say(t("Syncing your island...", "Sincronizando a sua ilha..."), false);
                Relay.beforeGame(this, folder);
            });
        }).start();
    }
}
