package dev.gpui.mobile;

import android.app.Activity;
import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.Context;
import android.content.Intent;
import android.media.AudioManager;
import android.net.Uri;
import android.os.Build;
import android.os.Environment;
import android.provider.MediaStore;
import android.view.View;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowInsetsController;

import androidx.core.content.FileProvider;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;

/**
 * Hide and restore the system bars for immersive reading.
 *
 * <p>The reader goes full screen: both the status bar and the navigation bar are dropped. The
 * Rust side still lays its own top bar below the status bar's line (the reading layer keeps a
 * safe-area inset at the top), so the punch-hole camera never overlaps the back button.</p>
 *
 * <p>GPUI's Android event loop runs on its own native thread, so Rust reaches this from a
 * non-UI thread: {@link #setImmersive} hops to the UI thread itself rather than trusting the
 * caller. Both directions are a <em>set</em>, not a toggle — calling it twice with the same
 * value is a no-op, which is what lets the Rust side fire it on every state change without
 * tracking what the window currently looks like.</p>
 *
 * <p>Two API tiers because {@code minSdk} is 26: API 30 has
 * {@link WindowInsetsController}, older releases only have the deprecated
 * {@code SYSTEM_UI_FLAG_*} bits on the decor view.</p>
 */
public final class GpuiSystemUi {
    private GpuiSystemUi() {
    }

    /** Called by Rust, from any thread. */
    public static void setImmersive(Activity activity, boolean immersive) {
        if (activity == null) return;
        final boolean wanted = immersive;
        activity.runOnUiThread(() -> apply(activity, wanted));
    }

    /**
     * Move the music stream one volume step in the direction Rust names.
     *
     * <p>The volume keys are not handled by the system here: NativeActivity forwards every key
     * event to the native input queue and consumes it, so the framework's own volume handling
     * never runs. That is what lets the reader spend the keys on page turns — and what would
     * leave the buttons doing nothing at all on any other screen, were it not for this call. So
     * the app decides: the reading page turns a page, everything else comes here to hand the
     * button back.</p>
     *
     * <p>{@link AudioManager#FLAG_SHOW_UI} keeps the system's level bar visible, so the feedback
     * is the one a user gets outside this app. {@code ADJUST_RAISE} and {@code ADJUST_LOWER} are
     * the API 1 constants; the {@code ADJUST_VOLUME_*} aliases came later than the API 26
     * {@code minSdk} allows for the older devices still in the field.</p>
     *
     * <p>Called by Rust, from any thread.</p>
     */
    public static void adjustVolume(Activity activity, boolean up) {
        if (activity == null) return;
        final int direction = up ? AudioManager.ADJUST_RAISE : AudioManager.ADJUST_LOWER;
        activity.runOnUiThread(() -> {
            AudioManager audio =
                    (AudioManager) activity.getSystemService(Context.AUDIO_SERVICE);
            if (audio == null) return;
            try {
                audio.adjustStreamVolume(AudioManager.STREAM_MUSIC, direction,
                        AudioManager.FLAG_SHOW_UI);
            } catch (SecurityException e) {
                // Only a managed profile or a device policy can refuse this; log rather than
                // rethrow, because a silent volume key beats a crashed reader.
                android.util.Log.e("GpuiSystemUi", "adjustStreamVolume refused", e);
            }
        });
    }

    /**
     * Hand an image to the system share sheet.
     *
     * <p>The bytes come from Rust (the reader holds the container, not a file), so we drop them
     * into a {@code cache/share/} file and hand out a {@code content://} URI through our
     * {@code FileProvider} — a raw {@code file://} URI throws {@code FileUriExposedException} on
     * API 24+. The cache path matches {@code res/xml/file_paths.xml}.</p>
     *
     * <p>Called by Rust, from any thread.</p>
     */
    public static void shareImage(Activity activity, byte[] data, String mime, String name) {
        if (activity == null || data == null) return;
        final byte[] bytes = data;
        final String type = (mime == null || mime.isEmpty()) ? "image/*" : mime;
        final String fileName = (name == null || name.isEmpty()) ? "image" : name;
        activity.runOnUiThread(() -> {
            try {
                File dir = new File(activity.getCacheDir(), "share");
                //noinspection ResultOfMethodCallIgnored
                dir.mkdirs();
                File out = new File(dir, fileName);
                try (OutputStream os = new FileOutputStream(out)) {
                    os.write(bytes);
                }
                Uri uri = FileProvider.getUriForFile(
                        activity, activity.getPackageName() + ".fileprovider", out);
                Intent send = new Intent(Intent.ACTION_SEND);
                send.setType(type);
                send.putExtra(Intent.EXTRA_STREAM, uri);
                send.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
                Intent chooser = Intent.createChooser(send, null);
                chooser.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                activity.startActivity(chooser);
            } catch (Exception e) {
                android.util.Log.e("GpuiSystemUi", "shareImage failed", e);
            }
        });
    }

    /**
     * Write an image into the public gallery ({@code Pictures/Folio}).
     *
     * <p>API 29+ goes through {@code MediaStore} with {@code RELATIVE_PATH} and the
     * {@code IS_PENDING} dance, which needs no runtime permission. Older releases have no
     * relative-path column, so this is best-effort there (the {@code WRITE_EXTERNAL_STORAGE}
     * permission in the manifest covers API 26–28).</p>
     *
     * <p>Called by Rust, from any thread.</p>
     */
    public static void saveImageToGallery(Activity activity, byte[] data, String mime, String name) {
        if (activity == null || data == null) return;
        final byte[] bytes = data;
        final String type = (mime == null || mime.isEmpty()) ? "image/*" : mime;
        final String fileName = (name == null || name.isEmpty()) ? "image" : name;
        activity.runOnUiThread(() -> {
            try {
                ContentResolver resolver = activity.getContentResolver();
                ContentValues values = new ContentValues();
                values.put(MediaStore.Images.Media.DISPLAY_NAME, fileName);
                values.put(MediaStore.Images.Media.MIME_TYPE, type);
                if (Build.VERSION.SDK_INT >= 29) {
                    values.put(MediaStore.Images.Media.RELATIVE_PATH,
                            Environment.DIRECTORY_PICTURES + "/Folio");
                    values.put(MediaStore.Images.Media.IS_PENDING, 1);
                }
                Uri uri = resolver.insert(
                        MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values);
                if (uri == null) {
                    android.util.Log.e("GpuiSystemUi", "saveImageToGallery: insert returned null");
                    return;
                }
                try (OutputStream os = resolver.openOutputStream(uri)) {
                    if (os == null) return;
                    os.write(bytes);
                }
                if (Build.VERSION.SDK_INT >= 29) {
                    ContentValues done = new ContentValues();
                    done.put(MediaStore.Images.Media.IS_PENDING, 0);
                    resolver.update(uri, done, null, null);
                }
            } catch (Exception e) {
                android.util.Log.e("GpuiSystemUi", "saveImageToGallery failed", e);
            }
        });
    }

    private static void apply(Activity activity, boolean immersive) {
        Window window = activity.getWindow();
        View decor = window.getDecorView();
        if (Build.VERSION.SDK_INT >= 30) {
            // Let the app draw edge to edge first, then ask the controller to drop the bars.
            // Without this the insets keep reserving the status bar's height and the page
            // gets a blank band where the bar used to be.
            window.setDecorFitsSystemWindows(!immersive);
            WindowInsetsController controller = decor.getWindowInsetsController();
            if (controller == null) return;
            int bars = WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout();
            if (immersive) {
                controller.setSystemBarsBehavior(
                        WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
                controller.hide(bars);
            } else {
                controller.show(bars);
            }
            return;
        }
        int flags = View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN;
        if (immersive) {
            flags |= View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                    | View.SYSTEM_UI_FLAG_FULLSCREEN
                    | View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY;
        }
        decor.setSystemUiVisibility(flags);
    }
}
