package dev.gpui.mobile;

import android.app.Activity;
import android.content.Intent;
import android.net.Uri;

public class GpuiDeepLink {

    public static String getInitialLink(Activity activity) {
        if (activity == null) return null;
        Intent intent = activity.getIntent();
        if (intent == null) return null;
        String action = intent.getAction();
        Uri data = intent.getData();
        if (data == null && Intent.ACTION_SEND.equals(action)) {
            // "Share to Folio" cold start: the document lives in EXTRA_STREAM, the data
            // slot is empty. Same channel as the hot path (nativeOnDeepLink) — the Rust
            // side routes content:// into the import queue.
            data = GpuiActivity.streamOf(intent);
        }
        if (data != null && (Intent.ACTION_VIEW.equals(action) || Intent.ACTION_MAIN.equals(action)
                || Intent.ACTION_SEND.equals(action))) {
            return data.toString();
        }
        return null;
    }
}
