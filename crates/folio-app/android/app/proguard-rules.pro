# Keep NativeActivity since it is referenced by name in AndroidManifest.xml.
-keep class android.app.NativeActivity { *; }

# JNI resolves these classes and methods through the Activity classloader.
-keep class dev.gpui.mobile.FolioSourceAccess { *; }
-keep class dev.gpui.mobile.FolioSpeech { *; }
