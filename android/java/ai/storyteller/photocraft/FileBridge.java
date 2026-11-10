package ai.storyteller.photocraft;

import android.app.Activity;
import android.content.ClipData;
import android.content.Context;
import android.content.Intent;
import android.database.Cursor;
import android.graphics.Insets;
import android.net.Uri;
import android.os.Build;
import android.provider.OpenableColumns;
import android.view.View;
import android.view.WindowInsets;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ConcurrentLinkedQueue;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * The Android side of PhotoCraft's file handling and window.
 *
 * Every public static method takes the application context and plain strings and answers with a
 * string: "ok:<payload>" or "err:<message>". Nothing throws across the boundary and nothing calls
 * back into Rust: picked files are copied into the app's cache, and Rust polls {@link #pollEvents}
 * for tab-separated event lines ("open|place\<name>\<path>", "saved\<name>\<result>",
 * "done\<mode>"). Files move through the cache so no byte arrays cross the JNI boundary.
 */
public final class FileBridge {
    static volatile Activity activity;

    private static final int REQ_OPEN = 1;
    private static final int REQ_PLACE = 2;
    private static final int REQ_SAVE_BASE = 100;

    private static final ConcurrentLinkedQueue<String> EVENTS = new ConcurrentLinkedQueue<>();
    private static final ConcurrentHashMap<Integer, String[]> SAVES = new ConcurrentHashMap<>();
    private static final AtomicInteger NEXT_SAVE = new AtomicInteger(REQ_SAVE_BASE);
    private static final AtomicInteger NEXT_FILE = new AtomicInteger();

    private FileBridge() {}

    /** Shows the system's open screen: mode "open" (one file) or "place" (several). */
    public static String startPick(Context context, String mode) {
        final Activity a = activity;
        if (a == null) {
            return "err:the window isn't ready";
        }
        final boolean many = "place".equals(mode);
        a.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                try {
                    Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
                    intent.addCategory(Intent.CATEGORY_OPENABLE);
                    intent.setType("*/*");
                    intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, many);
                    a.startActivityForResult(intent, many ? REQ_PLACE : REQ_OPEN);
                } catch (RuntimeException e) {
                    EVENTS.add("error\tpicker\t" + clean(String.valueOf(e)));
                    EVENTS.add("done\t" + (many ? "place" : "open") + "\t");
                }
            }
        });
        return "ok:";
    }

    /** Shows the system's save screen for the file already written at cachePath. */
    public static String startSave(Context context, final String name, final String cachePath) {
        final Activity a = activity;
        if (a == null) {
            return "err:the window isn't ready";
        }
        final int code = NEXT_SAVE.getAndIncrement();
        SAVES.put(code, new String[] {name, cachePath});
        a.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                try {
                    Intent intent = new Intent(Intent.ACTION_CREATE_DOCUMENT);
                    intent.addCategory(Intent.CATEGORY_OPENABLE);
                    intent.setType("application/octet-stream");
                    intent.putExtra(Intent.EXTRA_TITLE, name);
                    a.startActivityForResult(intent, code);
                } catch (RuntimeException e) {
                    SAVES.remove(code);
                    new File(cachePath).delete();
                    EVENTS.add("saved\t" + clean(name) + "\terr:" + clean(String.valueOf(e)));
                }
            }
        });
        return "ok:";
    }

    /** The events collected since the last call, one per line. */
    public static String pollEvents(Context context) {
        StringBuilder out = new StringBuilder("ok:");
        String line;
        while ((line = EVENTS.poll()) != null) {
            out.append(line).append('\n');
        }
        return out.toString();
    }

    /** "ok:top,bottom,left,right": the system bars' and cut-outs' size in pixels. */
    public static String insets(Context context) {
        Activity a = activity;
        if (a == null) {
            return "ok:0,0,0,0";
        }
        try {
            View decor = a.getWindow().getDecorView();
            WindowInsets wi = decor.getRootWindowInsets();
            if (wi == null) {
                return "ok:0,0,0,0";
            }
            if (Build.VERSION.SDK_INT >= 30) {
                Insets i = wi.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
                return "ok:" + i.top + "," + i.bottom + "," + i.left + "," + i.right;
            }
            return "ok:" + wi.getSystemWindowInsetTop() + "," + wi.getSystemWindowInsetBottom() + "," + wi.getSystemWindowInsetLeft()
                    + "," + wi.getSystemWindowInsetRight();
        } catch (RuntimeException e) {
            return "err:" + e;
        }
    }

    /** Opens a web link in the person's browser. */
    public static String openUrl(Context context, String url) {
        try {
            Intent intent = new Intent(Intent.ACTION_VIEW, Uri.parse(url));
            intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            context.startActivity(intent);
            return "ok:";
        } catch (RuntimeException e) {
            return "err:" + e;
        }
    }

    /** The activity's answer to a picker; copying happens off the UI thread. */
    static void onResult(final Activity a, final int request, final int result, final Intent data) {
        if (request == REQ_OPEN || request == REQ_PLACE) {
            final String mode = request == REQ_PLACE ? "place" : "open";
            new Thread(new Runnable() {
                @Override
                public void run() {
                    try {
                        if (result == Activity.RESULT_OK && data != null) {
                            copyPicked(a, mode, data);
                        }
                    } catch (RuntimeException e) {
                        EVENTS.add("error\t" + mode + "\t" + clean(String.valueOf(e)));
                    } finally {
                        EVENTS.add("done\t" + mode + "\t");
                    }
                }
            }).start();
        } else if (request >= REQ_SAVE_BASE) {
            final String[] save = SAVES.remove(request);
            if (save == null) {
                return;
            }
            final Uri target = data == null ? null : data.getData();
            new Thread(new Runnable() {
                @Override
                public void run() {
                    String answer = "cancelled";
                    if (result == Activity.RESULT_OK && target != null) {
                        answer = copyOut(a, save[1], target);
                    }
                    new File(save[1]).delete();
                    EVENTS.add("saved\t" + clean(save[0]) + "\t" + answer);
                }
            }).start();
        }
    }

    private static void copyPicked(Activity a, String mode, Intent data) {
        java.util.ArrayList<Uri> uris = new java.util.ArrayList<>();
        ClipData clip = data.getClipData();
        if (clip != null) {
            for (int i = 0; i < clip.getItemCount(); i++) {
                uris.add(clip.getItemAt(i).getUri());
            }
        } else if (data.getData() != null) {
            uris.add(data.getData());
        }
        File folder = new File(a.getCacheDir(), "picked");
        folder.mkdirs();
        for (Uri uri : uris) {
            String name = displayName(a, uri);
            File file = new File(folder, NEXT_FILE.getAndIncrement() + "-" + name.replaceAll("[/\\\\]", "_"));
            try (InputStream in = a.getContentResolver().openInputStream(uri); OutputStream out = new FileOutputStream(file)) {
                if (in == null) {
                    EVENTS.add("error\t" + clean(name) + "\tcouldn't be opened");
                    continue;
                }
                byte[] buffer = new byte[64 * 1024];
                int read;
                while ((read = in.read(buffer)) != -1) {
                    out.write(buffer, 0, read);
                }
            } catch (Exception e) {
                EVENTS.add("error\t" + clean(name) + "\t" + clean(String.valueOf(e)));
                continue;
            }
            EVENTS.add(mode + "\t" + clean(name) + "\t" + file.getAbsolutePath());
        }
    }

    private static String copyOut(Activity a, String cachePath, Uri target) {
        try (InputStream in = new FileInputStream(cachePath); OutputStream out = a.getContentResolver().openOutputStream(target, "wt")) {
            if (out == null) {
                return "err:the destination couldn't be opened";
            }
            byte[] buffer = new byte[64 * 1024];
            int read;
            while ((read = in.read(buffer)) != -1) {
                out.write(buffer, 0, read);
            }
            return "ok";
        } catch (Exception e) {
            return "err:" + clean(String.valueOf(e));
        }
    }

    private static String displayName(Context c, Uri uri) {
        try (Cursor cursor = c.getContentResolver().query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                String name = cursor.getString(0);
                if (name != null && !name.isEmpty()) {
                    return name;
                }
            }
        } catch (RuntimeException ignored) {
            // fall through to the last path segment
        }
        String last = uri.getLastPathSegment();
        return last == null ? "Untitled" : last;
    }

    private static String clean(String text) {
        return text.replace('\t', ' ').replace('\n', ' ').replace('\r', ' ');
    }
}
