package dev.gpui.mobile;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ResolveInfo;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.provider.Settings;
import android.speech.tts.TextToSpeech;
import android.speech.tts.UtteranceProgressListener;
import android.speech.tts.Voice;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.Set;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

/** One silent initialization and one TTS object per native engine generation. */
public final class FolioSpeech {
    private static final Handler MAIN = new Handler(Looper.getMainLooper());
    private static final Object LOCK = new Object();
    private static final long INIT_TIMEOUT_MS = 6000;
    private static final int MAX_VOICES = 2048;
    private static long nextSession;
    private static volatile Session current;

    private FolioSpeech() {}

    private static final class Session {
        final long id;
        final String requestedEngine;
        final CountDownLatch initialized = new CountDownLatch(1);
        final ArrayDeque<JSONObject> events = new ArrayDeque<>();
        String activeUtterance;
        boolean started;
        volatile TextToSpeech tts;
        volatile boolean closed;
        volatile boolean ready;
        volatile String error;

        Session(long id, String requestedEngine) {
            this.id = id;
            this.requestedEngine = requestedEngine;
        }
    }

    public static String request(Activity activity, String argument) {
        try {
            JSONObject request = new JSONObject(argument);
            String operation = request.getString("op");
            if ("init".equals(operation)) {
                return initialize(activity).toString();
            }
            Session session = current;
            if (session == null || session.id != request.getLong("session") || session.closed) {
                return error("Speech session expired").toString();
            }
            if ("shutdown".equals(operation)) {
                close(session);
                return new JSONObject().toString();
            }
            if (!session.ready || session.tts == null) {
                return error("Speech engine is not ready").toString();
            }
            switch (operation) {
                case "voices":
                    return directory(session).toString();
                case "select":
                    return select(session, request.getString("voice")).toString();
                case "speak": {
                    String text = request.getString("text");
                    if (text.isEmpty() || text.length() > TextToSpeech.getMaxSpeechInputLength()) {
                        return error("Speech input length is unsupported").toString();
                    }
                    Voice voice = session.tts.getVoice();
                    if (voice == null || !localInstalled(voice)) {
                        return error("No installed local speech voice is selected").toString();
                    }
                    Bundle params = new Bundle();
                    params.putString(TextToSpeech.Engine.KEY_FEATURE_EMBEDDED_SYNTHESIS, "true");
                    String id = request.getString("utterance");
                    synchronized (session.events) {
                        session.activeUtterance = id;
                        session.started = false;
                        session.events.clear();
                    }
                    int result = session.tts.speak(text, TextToSpeech.QUEUE_FLUSH, params, id);
                    if (result != TextToSpeech.SUCCESS) clearPlayback(session);
                    return result(result, "Speech request failed").toString();
                }
                case "stop":
                    clearPlayback(session);
                    return result(session.tts.stop(), "Speech stop failed").toString();
                case "rate":
                    return result(session.tts.setSpeechRate((float) request.getDouble("value")),
                            "Speech rate change failed").toString();
                case "pitch":
                    return result(session.tts.setPitch((float) request.getDouble("value")),
                            "Speech pitch change failed").toString();
                case "events": {
                    JSONArray events = new JSONArray();
                    synchronized (session.events) {
                        while (!session.events.isEmpty()) events.put(session.events.removeFirst());
                    }
                    return new JSONObject().put("events", events).toString();
                }
                default:
                    return error("Unknown speech operation").toString();
            }
        } catch (Exception ignored) {
            return error("Android speech bridge failed").toString();
        }
    }

    private static JSONObject initialize(Activity activity) throws JSONException {
        if (Looper.myLooper() == Looper.getMainLooper()) {
            return error("Speech initialization requires a worker thread");
        }
        Context context = activity.getApplicationContext();
        String requested = Settings.Secure.getString(context.getContentResolver(),
                Settings.Secure.TTS_DEFAULT_SYNTH);
        if (requested == null || requested.isEmpty() || !hasEngine(context, requested)) {
            return error("No installed default speech engine");
        }
        Session session;
        synchronized (LOCK) {
            if (current != null && !current.closed) return error("Speech initialization is busy");
            session = new Session(++nextSession, requested);
            current = session;
        }
        MAIN.post(() -> create(context, session));
        try {
            if (!session.initialized.await(INIT_TIMEOUT_MS, TimeUnit.MILLISECONDS)) {
                close(session);
                return error("Speech initialization timed out");
            }
        } catch (InterruptedException ignored) {
            Thread.currentThread().interrupt();
            close(session);
            return error("Speech initialization was cancelled");
        }
        if (!session.ready || session.closed) {
            close(session);
            return error(session.error == null ? "Speech initialization failed" : session.error);
        }
        try {
            Voice voice = session.tts.getDefaultVoice();
            return new JSONObject().put("session", session.id)
                    .put("engine", engineScope(session))
                    .put("default_voice", voice != null && localInstalled(voice)
                            ? key(session, voice.getName()) : JSONObject.NULL);
        } catch (RuntimeException failure) {
            close(session);
            return error("Speech default voice is unavailable");
        }
    }

    private static boolean hasEngine(Context context, String requested) {
        List<ResolveInfo> engines = context.getPackageManager().queryIntentServices(
                new Intent(TextToSpeech.Engine.INTENT_ACTION_TTS_SERVICE), 0);
        for (ResolveInfo engine : engines) {
            if (engine.serviceInfo != null && engine.serviceInfo.enabled
                    && requested.equals(engine.serviceInfo.packageName)) return true;
        }
        return false;
    }

    private static void create(Context context, Session session) {
        if (session.closed || current != session) return;
        try {
            session.tts = new TextToSpeech(context,
                    status -> MAIN.post(() -> finishInitialization(session, status)),
                    session.requestedEngine);
            if (session.closed) dispose(session);
        } catch (RuntimeException ignored) {
            session.error = "Speech engine could not be created";
            close(session);
        }
    }

    private static void finishInitialization(Session session, int status) {
        try {
            completeInitialization(session, status);
        } catch (RuntimeException ignored) {
            session.error = "Speech engine initialization failed";
            close(session);
        }
    }

    private static void completeInitialization(Session session, int status) {
        if (session.closed || current != session) {
            dispose(session);
            return;
        }
        if (status != TextToSpeech.SUCCESS || session.tts == null) {
            session.error = "Speech engine initialization failed";
            close(session);
            return;
        }
        int listener = session.tts.setOnUtteranceProgressListener(new UtteranceProgressListener() {
            @Override public void onStart(String id) { event(session, id, "begin", null); }
            @Override public void onDone(String id) { event(session, id, "done", null); }
            @Override public void onStop(String id, boolean interrupted) {
                event(session, id, "cancelled", null);
            }
            @Override public void onError(String id) {
                event(session, id, "failed", "Speech synthesis failed");
            }
            @Override public void onError(String id, int code) {
                event(session, id, "failed", "Speech synthesis failed (" + code + ")");
            }
        });
        if (listener != TextToSpeech.SUCCESS) {
            session.error = "Speech progress listener could not be installed";
            close(session);
            return;
        }
        session.ready = true;
        session.initialized.countDown();
    }

    private static void event(Session session, String id, String kind, String failure) {
        if (session.closed || current != session || id == null) return;
        try {
            JSONObject event = new JSONObject().put("id", id).put("kind", kind);
            if (failure != null) event.put("error", failure);
            synchronized (session.events) {
                if (!id.equals(session.activeUtterance)) return;
                if ("begin".equals(kind)) {
                    if (session.started) return;
                    session.started = true;
                } else {
                    session.activeUtterance = null;
                }
                session.events.addLast(event);
            }
        } catch (JSONException ignored) { }
    }

    private static JSONObject directory(Session session) throws JSONException {
        Set<Voice> available = session.tts.getVoices();
        if (available == null) return error("Speech voice directory is unavailable");
        if (available.size() > MAX_VOICES) return error("Speech voice directory exceeds its limit");
        ArrayList<Voice> voices = new ArrayList<>(available);
        voices.sort(Comparator.comparing(Voice::getName));
        JSONArray directory = new JSONArray();
        for (Voice voice : voices) {
            Set<String> features = voice.getFeatures();
            directory.put(new JSONObject().put("key", key(session, voice.getName()))
                    .put("name", voice.getName())
                    .put("language", voice.getLocale().toLanguageTag())
                    .put("network_required", voice.isNetworkConnectionRequired())
                    .put("installed", features == null ? JSONObject.NULL
                            : !features.contains(TextToSpeech.Engine.KEY_FEATURE_NOT_INSTALLED)));
        }
        return new JSONObject().put("voices", directory);
    }

    private static JSONObject select(Session session, String name) {
        Set<Voice> voices = session.tts.getVoices();
        if (voices == null) return error("Speech voice directory is unavailable");
        for (Voice voice : voices) {
            if (voice.getName().equals(name) && localInstalled(voice)) {
                int result = session.tts.setVoice(voice);
                Voice selected = session.tts.getVoice();
                if (result == TextToSpeech.SUCCESS && selected != null
                        && selected.getName().equals(name) && localInstalled(selected)) {
                    return new JSONObject();
                }
                return error("Speech engine rejected the requested voice");
            }
        }
        return error("The requested local voice is no longer installed");
    }

    private static boolean localInstalled(Voice voice) {
        Set<String> features = voice.getFeatures();
        return !voice.isNetworkConnectionRequired() && features != null
                && !features.contains(TextToSpeech.Engine.KEY_FEATURE_NOT_INSTALLED);
    }

    private static String engineScope(Session session) {
        // Android's public API may fall back and cannot confirm the bound engine package.
        return "requested:" + session.requestedEngine;
    }

    private static JSONObject key(Session session, String id) throws JSONException {
        return new JSONObject().put("backend", "android").put("engine", engineScope(session))
                .put("id", id);
    }

    private static JSONObject result(int status, String message) {
        return status == TextToSpeech.SUCCESS ? new JSONObject() : error(message);
    }

    private static JSONObject error(String message) {
        JSONObject result = new JSONObject();
        try { result.put("error", message); } catch (JSONException ignored) { }
        return result;
    }

    private static void close(Session session) {
        synchronized (LOCK) {
            session.closed = true;
            session.ready = false;
            if (current == session) current = null;
        }
        session.initialized.countDown();
        if (Looper.myLooper() == Looper.getMainLooper()) dispose(session);
        else MAIN.post(() -> dispose(session));
    }

    private static void dispose(Session session) {
        TextToSpeech engine = session.tts;
        session.tts = null;
        if (engine != null) {
            try { engine.stop(); } catch (RuntimeException ignored) { }
            try { engine.shutdown(); } catch (RuntimeException ignored) { }
        }
        clearPlayback(session);
    }

    private static void clearPlayback(Session session) {
        synchronized (session.events) {
            session.activeUtterance = null;
            session.started = false;
            session.events.clear();
        }
    }

    public static void shutdownAll() {
        Session session = current;
        if (session != null) close(session);
    }
}
