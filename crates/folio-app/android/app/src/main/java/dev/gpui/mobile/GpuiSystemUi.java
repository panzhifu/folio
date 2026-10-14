package dev.gpui.mobile;

import android.app.Activity;
import android.content.Context;
import android.media.AudioManager;
import android.os.Build;
import android.view.View;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowInsetsController;

/**
 * Hide and restore the system bars for immersive reading.
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
