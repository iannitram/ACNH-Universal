// Hub pairing and prepared-game pull into <files>/game/ (issue #80).
// Calls crates/runtime's hub_android.rs through runtime/src/hub_android.cpp.
package io.github.acnhrecomp;

import android.app.Activity;
import android.app.AlertDialog;
import android.text.InputType;
import android.widget.EditText;
import android.widget.LinearLayout;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.File;
import java.util.ArrayList;
import java.util.List;

import static io.github.acnhrecomp.Screen.t;

final class HubPull {
    interface Status {
        void onMessage(String text, boolean error);
    }

    private static native String nativeDiscover(int timeoutMs);
    private static native String nativePaired(String files);
    private static native String nativePair(String files, String addr, String hubName, String code);
    private static native String nativePull(String files, String addr, String hubName, long freeBytes);

    private HubPull() {}

    static void start(Activity activity, Status status, Runnable onInstalled) {
        File filesDir = activity.getFilesDir();
        String files = filesDir.getAbsolutePath();
        status.onMessage(t("Looking for a paired hub...", "Procurando um hub pareado..."), false);
        new Thread(() -> {
            String pairedJson = nativePaired(files);
            String pairedName = "";
            String pairedAddr = "";
            try {
                JSONObject obj = new JSONObject(pairedJson);
                pairedName = obj.optString("hub_name", "");
                pairedAddr = obj.optString("hub_address", "");
            } catch (Exception ignored) {}

            final String fName = pairedName;
            final String fAddr = pairedAddr;
            if (!fName.isEmpty() && !fAddr.isEmpty()) {
                activity.runOnUiThread(() -> askPairedAction(activity, filesDir, fAddr, fName, status, onInstalled));
                return;
            }
            discoverAndPick(activity, filesDir, status, onInstalled);
        }, "hub-paired-check").start();
    }

    private static void askPairedAction(Activity activity, File filesDir, String addr, String hubName,
                                        Status status, Runnable onInstalled) {
        if (activity.isFinishing()) return;
        new AlertDialog.Builder(activity)
                .setTitle(t("Paired hub: ", "Hub pareado: ") + hubName)
                .setMessage(t("Pull the prepared game from " + hubName + " (" + addr + ") into the app?",
                        "Baixar o jogo preparado de " + hubName + " (" + addr + ") para o app?"))
                .setPositiveButton(t("Pull game", "Baixar jogo"), (d, w) ->
                        pull(activity, filesDir, addr, hubName, status, onInstalled))
                .setNeutralButton(t("Pair another hub", "Parear outro hub"), (d, w) -> {
                    status.onMessage(t("Searching for hubs on your home network...",
                            "Procurando hubs na sua rede local..."), false);
                    new Thread(() -> discoverAndPick(activity, filesDir, status, onInstalled), "hub-discover").start();
                })
                .setNegativeButton(t("Cancel", "Cancelar"), null)
                .show();
    }

    private static void discoverAndPick(Activity activity, File filesDir, Status status, Runnable onInstalled) {
        String raw = nativeDiscover(3000);
        List<String> labels = new ArrayList<>();
        List<String> names = new ArrayList<>();
        List<String> addrs = new ArrayList<>();
        try {
            JSONArray arr = new JSONArray(raw);
            for (int i = 0; i < arr.length(); i++) {
                JSONObject item = arr.getJSONObject(i);
                String name = item.optString("hub", "");
                String ip = item.optString("addr", "");
                int port = item.optInt("port", 0);
                if (!name.isEmpty() && !ip.isEmpty() && port > 0) {
                    String addr = ip + ":" + port;
                    names.add(name);
                    addrs.add(addr);
                    labels.add(name + " (" + addr + ")");
                }
            }
        } catch (Exception ignored) {}

        activity.runOnUiThread(() -> {
            if (activity.isFinishing()) return;
            if (labels.isEmpty()) {
                askManualAddress(activity, filesDir, status, onInstalled);
                return;
            }
            labels.add(t("Enter address manually...", "Digitar endereço manualmente..."));
            new AlertDialog.Builder(activity)
                    .setTitle(t("Choose a hub", "Escolha um hub"))
                    .setItems(labels.toArray(new String[0]), (d, which) -> {
                        if (which == names.size()) {
                            askManualAddress(activity, filesDir, status, onInstalled);
                        } else {
                            askPairingCode(activity, filesDir, addrs.get(which), names.get(which), status, onInstalled);
                        }
                    })
                    .setNegativeButton(t("Cancel", "Cancelar"), null)
                    .show();
        });
    }

    private static void askManualAddress(Activity activity, File filesDir, Status status, Runnable onInstalled) {
        if (activity.isFinishing()) return;
        LinearLayout layout = new LinearLayout(activity);
        layout.setOrientation(LinearLayout.VERTICAL);
        int pad = Screen.dp(activity, 16);
        layout.setPadding(pad, pad, pad, 0);

        EditText nameInput = new EditText(activity);
        nameInput.setHint(t("Hub name (e.g. tablet)", "Nome do hub"));
        layout.addView(nameInput);

        EditText addrInput = new EditText(activity);
        addrInput.setHint(t("Address (ip:port)", "Endereço (ip:porta)"));
        layout.addView(addrInput);

        new AlertDialog.Builder(activity)
                .setTitle(t("Connect to hub", "Conectar ao hub"))
                .setView(layout)
                .setPositiveButton(t("Next", "Avançar"), (d, w) -> {
                    String name = nameInput.getText().toString().trim();
                    String addr = addrInput.getText().toString().trim();
                    if (name.isEmpty() || addr.isEmpty()) {
                        status.onMessage(t("Enter both the hub name and address.",
                                "Digite o nome e o endereço do hub."), true);
                        return;
                    }
                    askPairingCode(activity, filesDir, addr, name, status, onInstalled);
                })
                .setNegativeButton(t("Cancel", "Cancelar"), null)
                .show();
    }

    private static void askPairingCode(Activity activity, File filesDir, String addr, String hubName,
                                       Status status, Runnable onInstalled) {
        if (activity.isFinishing()) return;
        EditText codeInput = new EditText(activity);
        codeInput.setInputType(InputType.TYPE_CLASS_NUMBER);
        codeInput.setHint("000000");
        new AlertDialog.Builder(activity)
                .setTitle(t("Pair with ", "Parear com ") + hubName)
                .setMessage(t("Enter the 6-digit pairing code shown on the hub.",
                        "Digite o código de 6 dígitos mostrado no hub."))
                .setView(codeInput)
                .setPositiveButton(t("Pair & Pull", "Parear e baixar"), (d, w) -> {
                    String code = codeInput.getText().toString().trim();
                    status.onMessage(t("Pairing with " + hubName + "...", "Pareando com " + hubName + "..."), false);
                    new Thread(() -> {
                        String err = nativePair(filesDir.getAbsolutePath(), addr, hubName, code);
                        if (!err.isEmpty()) {
                            activity.runOnUiThread(() -> status.onMessage(err, true));
                            return;
                        }
                        pull(activity, filesDir, addr, hubName, status, onInstalled);
                    }, "hub-pair").start();
                })
                .setNegativeButton(t("Cancel", "Cancelar"), null)
                .show();
    }

    private static void pull(Activity activity, File filesDir, String addr, String hubName,
                             Status status, Runnable onInstalled) {
        activity.runOnUiThread(() -> status.onMessage(
                t("Pulling the game from " + hubName + " into private storage...",
                        "Baixando o jogo de " + hubName + " para o armazenamento privado..."), false));
        new Thread(() -> {
            long freeBytes = filesDir.getUsableSpace();
            String err = nativePull(filesDir.getAbsolutePath(), addr, hubName, freeBytes);
            activity.runOnUiThread(() -> {
                if (!err.isEmpty()) {
                    status.onMessage(err, true);
                } else {
                    onInstalled.run();
                }
            });
        }, "hub-pull").start();
    }
}
