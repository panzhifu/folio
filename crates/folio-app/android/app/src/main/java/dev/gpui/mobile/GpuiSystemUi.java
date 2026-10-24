package dev.gpui.mobile;

import android.app.Activity;
import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.media.AudioManager;
import android.net.Uri;
import android.os.Build;
import android.os.Environment;
import android.provider.MediaStore;
import android.view.View;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;

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
     * Send the whole task to the back — the same thing HOME does.
     *
     * <p>This is what the back gesture asks for once the app has nothing left to pop. The
     * alternative ({@code finish()} / letting the process drop) looks like a crash: the process
     * disappears while its window is still the foreground top window, so WindowManager logs
     * {@code WINDOW DIED} and {@code Force removing ActivityRecord … app died, no saved state}
     * — with no saved state there is no task snapshot, and with no snapshot the system has
     * nothing to animate. Going to the back keeps the process and the activity alive, plays the
     * system's own app→home transition, and makes the next entry an instant resume. To actually
     * leave, the user swipes the card away in recents, which is a transition the system owns
     * end to end.</p>
     *
     * <p>Called by Rust, from any thread: {@link Activity#moveTaskToBack} is
     * {@code @MainThread}, so it goes through {@code runOnUiThread} like everything else here.</p>
     */
    public static void moveTaskToBack(Activity activity, boolean nonRoot) {
        if (activity == null) return;
        activity.runOnUiThread(() -> activity.moveTaskToBack(nonRoot));
    }

    /**
     * Pin the startup orientation per device class: tablets and foldables follow the
     * user's rotation setting, phones are pinned back to portrait.
     *
     * <p>The manifest declares {@code screenOrientation="locked"}, so the starting window
     * (splash + icon) is born in whatever orientation the system currently is — no entry
     * rotation on any device. This method then resolves the tier: smallest screen width
     * ≥ 600dp (the standard tablet divider) relaxes the lock to
     * {@link ActivityInfo#SCREEN_ORIENTATION_FULL_USER} — the user's rotation setting rules
     * from there on (locked: stay as the system currently is; unlocked: follow the sensor).
     * Below that, the phone tier is pinned to {@code SCREEN_ORIENTATION_PORTRAIT}: it has
     * one portrait layout, and a landscape phone (853dp wide) crosses the 840dp
     * {@code Expanded} threshold and crushes the desktop layout onto it. A phone launched
     * while held landscape gets one landscape splash frame, re-pinned the same instant.</p>
     *
     * <p>Since the manifest no longer declares the window rotatable from birth, the measured
     * Meizu 20 / Android 16 side effect (the system silently flipping the user's
     * {@code accelerometer_rotation} to {@code 1} while the app is foreground) should have
     * no trigger left; the escape hatch, should some ROM reopen it, is the previous
     * arrangement — portrait in the manifest, relaxed at runtime — which is in git
     * history. See the {@code screenOrientation} comment in AndroidManifest.xml.</p>
     *
     * <p>Called once per process from {@code GpuiActivity.onCreate}, before
     * {@code super.onCreate()} — the earliest moment in the process, while the native thread
     * and the GPU init have not started yet. An activity recreation re-runs it.</p>
     *
     * <p>Called by the activity on the UI thread, but it hops through
     * {@code runOnUiThread} like everything else here, so any thread may call it.</p>
     */
    public static void applyStartupOrientation(Activity activity) {
        if (activity == null) return;
        activity.runOnUiThread(() -> {
            int smallest = activity.getResources().getConfiguration().smallestScreenWidthDp;
            if (smallest >= 600) {
                // locked 解析出的「启动那一刻的当前方向」与 FULL_USER 的落点一致
                // （读者的旋转设定正是系统当前方向的来源），这次改请求不产生可见转向。
                activity.setRequestedOrientation(ActivityInfo.SCREEN_ORIENTATION_FULL_USER);
            } else {
                // 手机这一档只有竖屏一套版式；locked 下握着横屏启动的那一瞬横屏
                // splash 在这里当帧钉回。
                activity.setRequestedOrientation(ActivityInfo.SCREEN_ORIENTATION_PORTRAIT);
            }
        });
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

    /**
     * Override this window's screen brightness for the reader's dimmer slider.
     *
     * <p>{@code WindowManager.LayoutParams.screenBrightness} is window-scoped: it needs no
     * permission and never touches another app or the system setting. {@code value} is the
     * ratio the reader picked (0.0–1.0), or the {@code BRIGHTNESS_OVERRIDE_NONE} sentinel
     * {@code -1.0f}, which hands the backlight straight back to the system.</p>
     *
     * <p>{@code Window.getAttributes()} copies the params out, so the field has to be written
     * back through {@code setAttributes()} before the window manager sees it. That call ends up
     * in {@code ViewRootImpl.requestLayout()}, and since API 36 that path checks the thread
     * ({@code CalledFromWrongThreadException: Only the original thread that created a view
     * hierarchy can touch its views}). Rust calls this from GPUI's own native thread, so the
     * hop to the UI thread has to happen here — same shape as {@link #setImmersive}.</p>
     *
     * <p>Called by Rust, from any thread.</p>
     */
    public static void setWindowBrightness(Activity activity, float value) {
        if (activity == null) return;
        final float wanted = value;
        activity.runOnUiThread(() -> {
            try {
                Window window = activity.getWindow();
                WindowManager.LayoutParams attrs = window.getAttributes();
                attrs.screenBrightness = wanted;
                window.setAttributes(attrs);
            } catch (Exception e) {
                // A refused brightness must not take the reader's page down with it.
                android.util.Log.w("GpuiSystemUi", "setWindowBrightness failed", e);
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

    /**
     * Whether this app is allowed to ask the system to install APKs.
     *
     * That is a per-app grant on Android 8+ (Settings > Install unknown apps), and
     * {@link android.content.pm.PackageManager#canRequestPackageInstalls} is the only
     * way to read it. minSdk is 26, so no version guard is needed here.
     */
    public static boolean canRequestInstalls(Activity activity) {
        if (activity == null) return false;
        try {
            return activity.getPackageManager().canRequestPackageInstalls();
        } catch (Exception e) {
            android.util.Log.e("GpuiSystemUi", "canRequestInstalls failed", e);
            return false;
        }
    }

    /**
     * Open the system page where the reader grants that permission to Folio.
     *
     * We do not poll for the result: the page is outside this process, and coming back
     * is a deliberate act by the reader, so the UI asks them to tap "check for updates"
     * once more rather than us resuming a download they may never finish approving.
     */
    public static void openInstallPermissionSettings(Activity activity) {
        if (activity == null) return;
        try {
            Intent intent = new Intent(
                    android.provider.Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                    Uri.parse("package:" + activity.getPackageName()));
            intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            activity.startActivity(intent);
        } catch (Exception e) {
            android.util.Log.e("GpuiSystemUi", "openInstallPermissionSettings failed", e);
        }
    }

    /**
     * Hand a downloaded APK to the system installer.
     *
     * {@code absolutePath} must live under a root declared in res/xml/file_paths.xml —
     * Folio writes it to {@code <filesDir>/folio/data/apk/}, which is the "apk" entry
     * there. A path outside those roots makes FileProvider throw IllegalArgumentException,
     * which is by design: it is what stops us exposing arbitrary files to the installer.
     *
     * The installer runs in another process, hence the read grant. FLAG_ACTIVITY_NEW_TASK
     * is required because we may be called from outside an activity context.
     */
    public static void installApk(Activity activity, String absolutePath) {
        if (activity == null || absolutePath == null || absolutePath.isEmpty()) return;
        try {
            File apk = new File(absolutePath);
            if (!apk.isFile()) {
                android.util.Log.e("GpuiSystemUi", "installApk: no such file " + absolutePath);
                return;
            }
            Uri uri = FileProvider.getUriForFile(
                    activity, activity.getPackageName() + ".fileprovider", apk);
            Intent intent = new Intent(Intent.ACTION_VIEW);
            intent.setDataAndType(uri, "application/vnd.android.package-archive");
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
            intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            activity.startActivity(intent);
        } catch (Exception e) {
            android.util.Log.e("GpuiSystemUi", "installApk failed", e);
        }
    }
}
