package dev.gpui.mobile;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.UriPermission;
import android.database.Cursor;
import android.net.Uri;
import android.os.Looper;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract;
import android.provider.OpenableColumns;
import org.json.JSONObject;

import java.io.File;
import java.io.FileNotFoundException;
import java.io.IOException;
import java.util.Collections;
import java.util.Iterator;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.Map;
import java.util.Set;

/** Device-local SAF access. The database owns source links, never this bridge. */
public final class FolioSourceAccess {
    private static final int MAX_PICK_LINKS = 4096;
    private static final Map<String, Pick> PICKS = new LinkedHashMap<String, Pick>() {
        @Override protected boolean removeEldestEntry(Map.Entry<String, Pick> eldest) {
            return size() > MAX_PICK_LINKS;
        }
    };
    private static final class Pick {
        final String uri;
        final String name;
        Pick(String uri, String name) { this.uri = uri; this.name = name; }
    }

    public static boolean persistSelection(Context context, Uri uri, int returnedFlags) {
        if (!isContent(uri) || (returnedFlags & Intent.FLAG_GRANT_READ_URI_PERMISSION) == 0) return false;
        try {
            if ((returnedFlags & Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION) != 0) {
                context.getContentResolver().takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION);
            }
            return hasReadGrant(context, uri);
        } catch (RuntimeException error) {
            return false;
        }
    }

    /** Called only for the exact cache file created from this selected URI. */
    public static void rememberPick(Context context, Uri uri, File cacheFile, int returnedFlags) {
        if (!persistSelection(context, uri, returnedFlags)) return;
        try {
            String path = cacheFile.getCanonicalPath();
            synchronized (PICKS) { PICKS.put(path, new Pick(uri.toString(), cacheFile.getName())); }
        } catch (IOException | SecurityException ignored) { }
    }

    public static String sourceForPick(Activity activity, String cachePath) {
        try {
            Pick pick;
            synchronized (PICKS) { pick = PICKS.get(new File(cachePath).getCanonicalPath()); }
            if (pick == null || !hasReadGrant(activity, Uri.parse(pick.uri))) return null;
            return new JSONObject().put("locator", pick.uri).put("display_name", pick.name).toString();
        } catch (Exception error) { return null; }
    }

    public static String statUri(Activity activity, String locator) {
        try {
            requireWorker();
            Uri uri = requireReadGrant(activity, locator);
            String[] complete = { OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE,
                    DocumentsContract.Document.COLUMN_LAST_MODIFIED };
            Cursor queried;
            try { queried = activity.getContentResolver().query(uri, complete, null, null, null); }
            catch (IllegalArgumentException unsupportedColumn) {
                queried = activity.getContentResolver().query(uri,
                        new String[] { OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE }, null, null, null);
            }
            try (Cursor cursor = queried) {
                if (cursor == null || !cursor.moveToFirst()) return "{\"missing\":true}";
                JSONObject value = new JSONObject();
                int name = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME);
                int size = cursor.getColumnIndex(OpenableColumns.SIZE);
                int modified = cursor.getColumnIndex(DocumentsContract.Document.COLUMN_LAST_MODIFIED);
                value.put("display_name", name >= 0 && !cursor.isNull(name) ? cursor.getString(name) : "document");
                value.put("size", size >= 0 && !cursor.isNull(size) && cursor.getLong(size) >= 0 ? cursor.getLong(size) : JSONObject.NULL);
                // SAF timestamps are Unix milliseconds; unknown remains null.
                value.put("modified", modified >= 0 && !cursor.isNull(modified) && cursor.getLong(modified) > 0 ? cursor.getLong(modified) : JSONObject.NULL);
                return value.toString();
            }
        } catch (SecurityException error) { return "{\"error\":\"permission_denied\"}"; }
        catch (Exception error) { return "{\"error\":\"unavailable\"}"; }
    }

    /** On success transfers ownership of a newly opened descriptor to Rust. */
    public static int openUriFd(Activity activity, String locator) {
        try {
            requireWorker();
            Uri uri = requireReadGrant(activity, locator);
            try (ParcelFileDescriptor descriptor = activity.getContentResolver().openFileDescriptor(uri, "r")) {
                return descriptor == null ? -1 : descriptor.detachFd();
            }
        } catch (SecurityException error) { return -2; }
        catch (FileNotFoundException error) { return -3; }
        catch (Exception error) { return -1; }
    }

    private static boolean isContent(Uri uri) {
        return uri != null && "content".equals(uri.getScheme())
                && uri.getAuthority() != null && !uri.getAuthority().isEmpty();
    }

    /**
     * URIs this process received from ACTION_VIEW / ACTION_SEND intents ("open with Folio").
     *
     * <p>Intent-granted access is <b>transient</b>: it is valid for as long as the process
     * lives, but it never shows up in {@link ContentResolver#getPersistedUriPermissions},
     * which is all {@link #hasReadGrant} used to look at — so an "open with Folio" tap was
     * always rejected with {@code permission_denied}. Intents register their URIs here on
     * arrival ({@link GpuiActivity#rememberIntentUris}); reads admit registry members
     * alongside persistably-granted ones. Bounded: intents arrive one at a time.</p>
     */
    private static final Set<Uri> INTENT_URIS = Collections.synchronizedSet(new LinkedHashSet<>());
    private static final int INTENT_URI_CAP = 16;

    /** Register a URI this process just received from an intent. Called on the main thread. */
    public static void rememberIntentUri(Uri uri) {
        if (!isContent(uri)) return;
        synchronized (INTENT_URIS) {
            INTENT_URIS.remove(uri);
            INTENT_URIS.add(uri);
            while (INTENT_URIS.size() > INTENT_URI_CAP) {
                Iterator<Uri> it = INTENT_URIS.iterator();
                it.next();
                it.remove();
            }
        }
    }

    private static boolean hasReadGrant(Context context, Uri uri) {
        if (!isContent(uri)) return false;
        if (INTENT_URIS.contains(uri)) return true;
        for (UriPermission permission : context.getContentResolver().getPersistedUriPermissions()) {
            if (permission.isReadPermission() && uri.equals(permission.getUri())) return true;
        }
        return false;
    }
    private static Uri requireReadGrant(Context context, String locator) {
        Uri uri = Uri.parse(locator);
        if (!hasReadGrant(context, uri)) throw new SecurityException("Persistent read access is unavailable");
        return uri;
    }
    private static void requireWorker() {
        if (Looper.myLooper() == Looper.getMainLooper()) throw new IllegalStateException("Source reads must run on a worker");
    }
    private FolioSourceAccess() { }
}
