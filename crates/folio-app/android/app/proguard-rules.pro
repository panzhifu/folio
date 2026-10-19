# ProGuard / R8 rules for the GPUI Mobile Android Example.
#
# This is a pure native (Rust) application using NativeActivity — there is no
# Java or Kotlin application code to shrink, optimize, or obfuscate.
#
# These rules are intentionally empty.  They exist only because the
# build.gradle.kts references this file in the release buildType's
# proguardFiles configuration.

# Keep NativeActivity since it is referenced by name in AndroidManifest.xml.
-keep class android.app.NativeActivity { *; }

# `rs.tts.Bridge` 是 tts 的 JNI 桥：Rust 侧的导出符号名就是把包名+类名+方法名拼出来的
# （`Java_rs_tts_Bridge_onDone` 等五条），而 `JNI_OnLoad` 用字符串 "rs/tts/Bridge" 找它。
# 改名或剪掉任何一条，症状都不是编译错误，而是加载期 `expect` 炸掉进程。
# 2026-10-19 实测过这个下场：APK 里没有这个类时 logcat 是
# `Abort message: 'Failed to find `Bridge`: JavaException'` + SIGABRT。
# 现在两个 buildType 都是 `isMinifyEnabled = false`，所以这条暂时不干活；
# 它是给「将来为体积开 R8」上的保险，同一个文件里 Bridge.java 还带 @Keep。
-keep class rs.tts.Bridge { *; }
-keepclassmembers class rs.tts.Bridge { native <methods>; }
