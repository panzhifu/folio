package dev.gpui.mobile;

import android.app.Activity;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.IBinder;
import android.support.v4.media.MediaMetadataCompat;
import android.support.v4.media.session.MediaSessionCompat;
import android.support.v4.media.session.PlaybackStateCompat;
import android.util.Log;

import androidx.core.app.NotificationCompat;
import androidx.core.content.ContextCompat;

/**
 * Foreground service backing listen (TTS) playback.
 *
 * <p>Three jobs, all for the same reason — reading aloud must survive the reader leaving
 * the app:</p>
 * <ul>
 *   <li><b>Foreground service</b> ({@code mediaPlayback} type): keeps the process from
 *   being reaped while the TTS engine is talking. Without this, backgrounding the app
 *   drops it into the cached bucket and the reading simply stops mid-sentence.</li>
 *   <li><b>MediaSessionCompat</b>: lock screen, headset buttons and the system media
 *   panel control play/pause/stop and sentence stepping through the session callbacks.</li>
 *   <li><b>MediaStyle notification</b> with transport actions; the action buttons come
 *   back as service intents and the session buttons as callbacks — both funnel into
 *   {@link #nativeMediaAction}, whose Rust side queues them for the reading page.</li>
 * </ul>
 *
 * <p>Rust drives it with three statics ({@link #start}, {@link #update}, {@link #stop}),
 * all safe to call from any thread: service lifecycle calls hop through
 * {@code ContextCompat.startForegroundService} / {@code stopService}, which are
 * main-thread safe by contract.</p>
 */
public class FolioSpeechService extends Service {

    private static final String TAG = "FolioSpeechService";
    private static final String CHANNEL_ID = "folio_speech";
    private static final int NOTIFICATION_ID = 2001;
    private static final String ACTION_PLAY = "dev.gpui.mobile.speech.PLAY";
    private static final String ACTION_PAUSE = "dev.gpui.mobile.speech.PAUSE";
    private static final String ACTION_STOP = "dev.gpui.mobile.speech.STOP";
    private static final String ACTION_NEXT = "dev.gpui.mobile.speech.NEXT";
    private static final String ACTION_PREVIOUS = "dev.gpui.mobile.speech.PREVIOUS";

    /** Application context, set in onCreate — the statics need one for builders. */
    private static Context sContext;
    private static MediaSessionCompat sSession;
    private static NotificationManager sNotifications;
    private static String sTitle = "Folio";
    private static String sSubtitle = "";
    private static boolean sPlaying = false;

    /** Begin (or refresh) listen playback in the background. Called from Rust, any thread. */
    public static void start(Activity activity, boolean playing, String title, String subtitle) {
        if (activity == null) return;
        remember(playing, title, subtitle);
        try {
            ContextCompat.startForegroundService(
                    activity, new Intent(activity, FolioSpeechService.class));
        } catch (Exception e) {
            // A refused service start must not take the reading session down with it;
            // in-app playback continues, only the background protection is missing.
            Log.w(TAG, "startForegroundService failed", e);
        }
        refresh();
    }

    /** Refresh the playback state / metadata on session and notification. Any thread. */
    public static void update(Activity activity, boolean playing, String title, String subtitle) {
        if (activity == null) return;
        remember(playing, title, subtitle);
        refresh();
    }

    /** Listen stopped: tear the whole thing down. Any thread. */
    public static void stop(Activity activity) {
        sPlaying = false;
        if (activity != null) {
            try {
                activity.stopService(new Intent(activity, FolioSpeechService.class));
            } catch (Exception e) {
                Log.w(TAG, "stopService failed", e);
            }
        }
        if (sNotifications != null) {
            sNotifications.cancel(NOTIFICATION_ID);
        }
        if (sSession != null) {
            sSession.setActive(false);
            sSession.release();
            sSession = null;
        }
        sContext = null;
    }

    private static void remember(boolean playing, String title, String subtitle) {
        sPlaying = playing;
        sTitle = (title != null && !title.isEmpty()) ? title : "Folio";
        sSubtitle = subtitle != null ? subtitle : "";
    }

    /** Push the remembered state onto the session and the notification (if up). */
    private static void refresh() {
        if (sSession != null) {
            setSessionState();
        }
        if (sContext != null && sNotifications != null && sSession != null) {
            sNotifications.notify(NOTIFICATION_ID, buildNotification());
        }
    }

    @Override
    public void onCreate() {
        super.onCreate();
        sContext = getApplicationContext();
        createChannel(sContext);
        sNotifications = (NotificationManager) getSystemService(Context.NOTIFICATION_SERVICE);
        sSession = new MediaSessionCompat(this, "FolioSpeech");
        sSession.setCallback(new MediaSessionCompat.Callback() {
            @Override public void onPlay() { nativeMediaAction("play"); }
            @Override public void onPause() { nativeMediaAction("pause"); }
            @Override public void onStop() { nativeMediaAction("stop"); }
            @Override public void onSkipToNext() { nativeMediaAction("next"); }
            @Override public void onSkipToPrevious() { nativeMediaAction("previous"); }
        });
        sSession.setActive(true);
        setSessionState();
        // The type must be declared at startForeground time on API 29+; the manifest entry
        // plus FOREGROUND_SERVICE_MEDIA_PLAYBACK make this type legal on API 34+.
        Notification notification = buildNotification();
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(NOTIFICATION_ID, notification,
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
        } else {
            startForeground(NOTIFICATION_ID, notification);
        }
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        // Notification action buttons ride service intents; session buttons ride the
        // callbacks. Both land on the same Rust queue.
        if (intent != null) {
            String action = intent.getAction();
            if (ACTION_PLAY.equals(action)) nativeMediaAction("play");
            else if (ACTION_PAUSE.equals(action)) nativeMediaAction("pause");
            else if (ACTION_STOP.equals(action)) nativeMediaAction("stop");
            else if (ACTION_NEXT.equals(action)) nativeMediaAction("next");
            else if (ACTION_PREVIOUS.equals(action)) nativeMediaAction("previous");
        }
        // The reader stopped: do not let the system resurrect us to read half a sentence.
        return START_NOT_STICKY;
    }

    @Override
    public void onDestroy() {
        if (sSession != null) {
            sSession.setActive(false);
            sSession.release();
            sSession = null;
        }
        sContext = null;
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    private static void createChannel(Context context) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return;
        NotificationChannel channel = new NotificationChannel(
                CHANNEL_ID, "Folio listen",
                NotificationManager.IMPORTANCE_LOW);
        channel.setDescription("Listen (text-to-speech) playback controls");
        channel.setShowBadge(false);
        NotificationManager manager =
                (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
        if (manager != null) manager.createNotificationChannel(channel);
    }

    private static void setSessionState() {
        if (sSession == null) return;
        int state = sPlaying
                ? PlaybackStateCompat.STATE_PLAYING
                : PlaybackStateCompat.STATE_PAUSED;
        long actions = PlaybackStateCompat.ACTION_PLAY
                | PlaybackStateCompat.ACTION_PAUSE
                | PlaybackStateCompat.ACTION_PLAY_PAUSE
                | PlaybackStateCompat.ACTION_STOP
                | PlaybackStateCompat.ACTION_SKIP_TO_NEXT
                | PlaybackStateCompat.ACTION_SKIP_TO_PREVIOUS;
        sSession.setPlaybackState(new PlaybackStateCompat.Builder()
                .setActions(actions)
                .setState(state, 0L, 1.0f)
                .build());
        sSession.setMetadata(new MediaMetadataCompat.Builder()
                .putString(MediaMetadataCompat.METADATA_KEY_TITLE, sTitle)
                .putString(MediaMetadataCompat.METADATA_KEY_ARTIST, sSubtitle)
                .build());
    }

    private static Notification buildNotification() {
        if (sContext == null || sSession == null) {
            throw new IllegalStateException("notification built before the service is up");
        }
        Intent open = sContext.getPackageManager()
                .getLaunchIntentForPackage(sContext.getPackageName());
        PendingIntent content = PendingIntent.getActivity(
                sContext, 0, open,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        NotificationCompat.Builder builder = new NotificationCompat.Builder(sContext, CHANNEL_ID)
                .setContentTitle(sTitle)
                .setContentText(sSubtitle)
                .setSmallIcon(android.R.drawable.ic_media_play)
                .setContentIntent(content)
                .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
                .setOnlyAlertOnce(true)
                .setOngoing(sPlaying)
                .setStyle(new androidx.media.app.NotificationCompat.MediaStyle()
                        .setMediaSession(sSession.getSessionToken())
                        .setShowActionsInCompactView(0, 1, 2))
                .addAction(android.R.drawable.ic_media_previous, "Previous",
                        serviceAction(ACTION_PREVIOUS))
                .addAction(
                        sPlaying ? android.R.drawable.ic_media_pause : android.R.drawable.ic_media_play,
                        sPlaying ? "Pause" : "Play",
                        serviceAction(sPlaying ? ACTION_PAUSE : ACTION_PLAY))
                .addAction(android.R.drawable.ic_media_next, "Next", serviceAction(ACTION_NEXT))
                // Fourth row only shows when the shade is expanded; the compact view keeps
                // prev / play-pause / next.
                .addAction(android.R.drawable.ic_delete, "Stop", serviceAction(ACTION_STOP));
        return builder.build();
    }

    private static PendingIntent serviceAction(String action) {
        Intent intent = new Intent(sContext, FolioSpeechService.class);
        intent.setAction(action);
        return PendingIntent.getService(
                sContext, 0, intent,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
    }

    /**
     * Rust side queue for transport commands from the session callbacks and notification
     * actions ("play" / "pause" / "stop" / "next" / "previous"). JNI resolves this symbol
     * in the app classloader.
     */
    private static native void nativeMediaAction(String action);
}
