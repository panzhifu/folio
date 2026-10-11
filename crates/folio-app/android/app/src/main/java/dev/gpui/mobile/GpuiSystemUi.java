package dev.gpui.mobile;

import android.app.Activity;
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
