package dev.gpui.mobile;

import android.app.NativeActivity;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.pm.PackageManager;
import android.media.AudioManager;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.util.Log;
import android.view.KeyEvent;

import androidx.core.splashscreen.SplashScreen;

/**
 * Custom Activity extending NativeActivity that integrates with the
 * AndroidX SplashScreen API.
 *
 * On API 31+ the system splash screen is displayed automatically via theme
 * attributes. On API 26-30 the AndroidX compat library emulates the same
 * behavior using the theme's windowBackground drawable.
 *
 * The splash screen is held visible until the Rust native library signals
 * that initialization is complete by setting NATIVE_INITIALIZED to true
 * (see src/android/jni.rs). This prevents the user from seeing an empty
 * or partially-rendered surface during startup.
 *
 * Also handles:
 * - Deep link intents (onNewIntent)
 * - Volume key routing to the MUSIC audio stream
 * - Media button events via MediaSessionCompat
 */
public class GpuiActivity extends GpuiInputActivity {

    /** Whether the native .so has been loaded via System.loadLibrary. */
    private static volatile boolean sNativeLibLoaded = false;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // 启动方向策略赶在**一切**之前：manifest 的 locked 让启动窗口按「启动那一刻系统
        // 的当前方向」出生（竖进竖、横进横，第一帧没有翻转可走），这里再把方向按设备
        // 分档落定 —— 最小边 ≥ 600dp 放宽成 FULL_USER 跟随读者的旋转设定，更小的设备
        // 钉回竖屏。此刻原生线程还没起，真有一次转向（握着横屏启动的手机）也落在系统
        // 最闲的时刻。全链条的理由与退路见 AndroidManifest.xml 里 screenOrientation 的
        // 注释与 GpuiSystemUi.applyStartupOrientation。
        // （在 super.onCreate 之前调是安全的：attach 已由 ActivityThread 完成，
        // getResources 与 setRequestedOrientation 都已可用。）
        GpuiSystemUi.applyStartupOrientation(this);

        // Install the splash screen BEFORE calling super.onCreate().
        // This is required by the AndroidX SplashScreen API.
        SplashScreen splash = SplashScreen.installSplashScreen(this);

        // NativeActivity loads the .so via dlopen (loadNativeCode), which does
        // NOT register JNI symbols with the classloader. We must call
        // System.loadLibrary() ourselves so that JNI can resolve our native
        // methods. Reading the library name from the manifest meta-data ensures
        // we stay in sync with the nativeLibraryName placeholder.
        if (!sNativeLibLoaded) {
            try {
                ActivityInfo ai = getPackageManager().getActivityInfo(
                        getComponentName(), PackageManager.GET_META_DATA);
                String libName = ai.metaData.getString("android.app.lib_name");
                if (libName != null) {
                    System.loadLibrary(libName);
                    sNativeLibLoaded = true;
                }
            } catch (PackageManager.NameNotFoundException e) {
                // Shouldn't happen — we're querying our own activity.
            } catch (UnsatisfiedLinkError e) {
                // Library may already be loaded by NativeActivity; that's fine.
                sNativeLibLoaded = true;
            }
        }

        // Keep the splash screen visible until the native side signals readiness.
        splash.setKeepOnScreenCondition(() -> !isNativeReady());

        // Route volume keys to the MUSIC stream so they control media volume
        // rather than the ringer/notification volume.
        setVolumeControlStream(AudioManager.STREAM_MUSIC);

        super.onCreate(savedInstanceState);

        // Cold start with an "open with Folio" intent: register the URI so the transient
        // grant admits it (see FolioSourceAccess INTENT_URIS). The data URI itself reaches
        // Rust through GpuiDeepLink.getInitialLink (deeplink install), the SEND stream
        // through the same fallback.
        rememberIntentUris(getIntent());
    }

    /** Register every document URI an intent hands us: its data and its EXTRA_STREAM. */
    private static void rememberIntentUris(Intent intent) {
        if (intent == null) return;
        FolioSourceAccess.rememberIntentUri(intent.getData());
        FolioSourceAccess.rememberIntentUri(streamOf(intent));
    }

    /** The EXTRA_STREAM document of an ACTION_SEND share, or null. */
    static Uri streamOf(Intent intent) {
        if (intent == null || !Intent.ACTION_SEND.equals(intent.getAction())) return null;
        if (Build.VERSION.SDK_INT >= 33) {
            return intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri.class);
        }
        // Pre-T getParcelableExtra(String) is the only shape; the raw-type call is the
        // documented fallback.
        @SuppressWarnings({"unchecked", "deprecation"})
        Uri stream = (Uri) intent.getParcelableExtra(Intent.EXTRA_STREAM);
        return stream;
    }

    /**
     * Check if the native library is fully initialized.
     * Returns false if the .so hasn't been loaded yet or if
     * NATIVE_INITIALIZED hasn't been set to true.
     */
    private boolean isNativeReady() {
        if (!sNativeLibLoaded) {
            return false;
        }
        try {
            return nativeIsInitialized();
        } catch (UnsatisfiedLinkError e) {
            return false;
        }
    }

    /**
     * Route media buttons to the MediaSession; everything else takes the normal path.
     *
     * The volume keys are deliberately NOT special-cased here. NativeActivity hands every key
     * event to the native input queue on the ordinary path, and the fork maps
     * {@code KEYCODE_VOLUME_UP/DOWN} onto bindable GPUI keystrokes, so the reader can spend them
     * on page turns. Screens that want the buttons back call
     * {@link GpuiSystemUi#adjustVolume}, which is what keeps the volume keys useful outside the
     * reading page.
     *
     * <p>Whether the system's own volume step still fires alongside that is decided in the
     * input policy, above this class, and not by returning from here — so on a real device the
     * level may move while the page turns. That is measured, not assumed; see
     * {@code plans/facts.md}.</p>
     */
    @Override
    public boolean dispatchKeyEvent(KeyEvent event) {
        int keyCode = event.getKeyCode();
        switch (keyCode) {
            case KeyEvent.KEYCODE_MEDIA_PLAY:
            case KeyEvent.KEYCODE_MEDIA_PAUSE:
            case KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE:
            case KeyEvent.KEYCODE_MEDIA_NEXT:
            case KeyEvent.KEYCODE_MEDIA_PREVIOUS:
            case KeyEvent.KEYCODE_MEDIA_STOP:
            case KeyEvent.KEYCODE_HEADSETHOOK:
                // Route media buttons through the MediaSession.
                // MediaButtonReceiver will dispatch to our session callback.
                return super.dispatchKeyEvent(event);

            default:
                return super.dispatchKeyEvent(event);
        }
    }

    @Override
    protected void onDestroy() {
        FolioSpeech.shutdownAll();
        // Tear the listen (TTS) foreground service down with the activity: app exit must
        // not leave a media notification hanging with no session behind it.
        FolioSpeechService.stop(this);
        super.onDestroy();
    }

    /**
     * Handle new intents delivered to this singleTask activity.
     *
     * When the app is already running and a deeplink is opened
     * (e.g. `adb shell am start -d gpui://video_player`), this method
     * receives the new intent. We update the activity's intent and
     * notify the Rust side via JNI.
     */
    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);

        // Register first: the URI's transient grant is only useful while we hold it, and
        // the Rust side may stat/open the document the moment the queue is drained.
        rememberIntentUris(intent);

        Uri data = intent.getData();
        if (data != null) {
            String url = data.toString();
            Log.i("GpuiActivity", "onNewIntent deeplink: " + url);
            try {
                nativeOnDeepLink(url);
            } catch (UnsatisfiedLinkError e) {
                Log.w("GpuiActivity", "nativeOnDeepLink not available yet");
            }
        }
        // "Share to Folio" carries the document in EXTRA_STREAM, not in the data slot;
        // forward it on the same channel — the Rust side routes content:// into the
        // import queue (services::intent_import) and folio:// into the cloud callback.
        Uri stream = streamOf(intent);
        if (stream != null && (data == null || !stream.equals(data))) {
            Log.i("GpuiActivity", "onNewIntent shared document: " + stream);
            try {
                nativeOnDeepLink(stream.toString());
            } catch (UnsatisfiedLinkError e) {
                Log.w("GpuiActivity", "nativeOnDeepLink not available yet");
            }
        }
    }

    /**
     * JNI bridge to check if the Rust NATIVE_INITIALIZED flag is set.
     */
    private static native boolean nativeIsInitialized();

    /**
     * JNI bridge to notify Rust of an incoming deeplink URL.
     */
    private static native void nativeOnDeepLink(String url);
}
