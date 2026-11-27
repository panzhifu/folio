# Keep NativeActivity since it is referenced by name in AndroidManifest.xml.
-keep class android.app.NativeActivity { *; }

# JNI resolves these classes and methods through the Activity classloader — R8 cannot
# see the string lookups (find_app_class → loadClass). The list must match the Rust side
# exactly: folio's own calls (platform/android*.rs) and the fork's per-feature calls
# (GpuiSystemUi / GpuiPlatformView / GpuiPathPicker / GpuiAudio / GpuiClipboard /
# GpuiDeepLink / GpuiPermissions). Manifest-declared activities (GpuiActivity,
# GpuiPathPicker, GpuiPermissionActivity, FolioSpeechService) get AAPT keep rules.
-keep class dev.gpui.mobile.FolioSourceAccess { *; }
-keep class dev.gpui.mobile.FolioSpeech { *; }
-keep class dev.gpui.mobile.FolioSpeechService { *; }
-keep class dev.gpui.mobile.GpuiSystemUi { *; }
-keep class dev.gpui.mobile.GpuiPlatformView { *; }
-keep class dev.gpui.mobile.GpuiHelper { *; }
-keep class dev.gpui.mobile.GpuiPathPicker { *; }
-keep class dev.gpui.mobile.GpuiAudio { *; }
-keep class dev.gpui.mobile.GpuiClipboard { *; }
-keep class dev.gpui.mobile.GpuiDeepLink { *; }
-keep class dev.gpui.mobile.GpuiPermissions { *; }
