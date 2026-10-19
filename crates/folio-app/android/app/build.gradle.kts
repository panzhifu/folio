// App module: packages the Rust cdylib libfolio_app.so into an APK hosted
// by NativeActivity. The Java bridge classes must stay in package
// dev.gpui.mobile - the Rust platform looks them up by name (GpuiHelper,
// GpuiPlatformView) - so only the app identity here is Folio's.
//
// This module packages the pre-compiled Rust native library into an APK
// that uses Android's NativeActivity to host the GPUI application.
//
// The Rust library must be compiled separately and placed into
// app/src/main/jniLibs/<abi>/ before building the APK.
//
// Quick start:
//   cd <repo-root>
//   cargo ndk -t arm64-v8a -o example/android_app/gradle/app/src/main/jniLibs \
//       build --example android_app --release
//   cd example/android_app/gradle
//   gradle assembleDebug --offline

plugins {
    id("com.android.application")
}

android {
    namespace = "dev.folio.reader"
    compileSdk = 34

    defaultConfig {
        applicationId = "dev.folio.reader"
        minSdk = 26          // Vulkan 1.0 is mandatory from API 26+
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"

        // Tell NativeActivity which .so to load.
        // This must match the cdylib / example output name.
        ndk {
            abiFilters += listOf("arm64-v8a")
        }

        // Forward the library name to the manifest via a placeholder.
        manifestPlaceholders["nativeLibraryName"] = "folio_app"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
        debug {
            isDebuggable = true
            isJniDebuggable = true
        }
    }

    // We do NOT use CMake / ndk-build — the native library is compiled
    // externally via cargo-ndk and placed directly into jniLibs.
    //
    // Disable the built-in native build system so Gradle doesn't look for
    // a CMakeLists.txt or Android.mk.
    externalNativeBuild {
        // Intentionally left empty.
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }

    // Tell Gradle where the pre-built .so files live.
    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }

    packaging {
        // Prevent stripping of the Rust library — cargo already strips in
        // release mode and stripping again can break backtraces.
        jniLibs {
            // gpui-mobile 这个依赖同时编 rlib（静态链进 libfolio_app.so，运行的就是它）和
            // cdylib；cargo-ndk 会把那个 cdylib 也拷进 jniLibs，名字带源码版本的哈希，于是
            // 每换一个版本就多一份，全都被打进 APK。实测都不需要：Java 那头两处加载都只读
            // manifest 的 android.app.lib_name（= folio_app），而 libfolio_app.so 的 NEEDED
            // 只有 liblog/libandroid/libdl/libm/libc，整库里也搜不到 "libgpui_mobile" 这个
            // 字符串。手动删没用（下次构建又生成一份新的），所以在这里排除掉。
            //
            // `libtts` 那条是同一件事的第二次发生（2026-10-19 加 `tts` 之后实测）：它是
            // rlib + cdylib + staticlib 三种 crate-type，cargo-ndk 把 cdylib 那份
            // （`libtts-ec8918794bb18dab.so`，390,104 B）也拷了进来，而链进
            // `libfolio_app.so` 的是 rlib 那一份 —— NEEDED 里没有任何 libtts，整库里
            // grep 不到 "libtts" 这个串。所以它打进 APK 纯属死重。
            // `JNI_OnLoad` 与五条 `Java_rs_tts_Bridge_*` 在 libfolio_app.so 自己的导出表里
            // （llvm-readelf --dyn-syms 查到），不靠那个 .so。
            excludes += listOf("**/libgpui_mobile*.so", "**/libtts*.so")
            keepDebugSymbols += listOf(
                "*/arm64-v8a/libfolio_app.so",
                "*/armeabi-v7a/libfolio_app.so",
                "*/x86_64/libfolio_app.so",
                "*/x86/libfolio_app.so"
            )
        }
    }

    // Lint configuration — relaxed for an example project.
    lint {
        abortOnError = false
        checkReleaseBuilds = false
    }
}

dependencies {
    // AndroidX core for NotificationCompat (used by GpuiNotifications)
    implementation("androidx.core:core:1.12.0")
    // AndroidX SplashScreen compat (used by GpuiActivity to hold splash until native init)
    implementation("androidx.core:core-splashscreen:1.0.1")
    // AndroidX Biometric for BiometricPrompt (used by GpuiAuthActivity)
    implementation("androidx.biometric:biometric:1.1.0")
    // AndroidX Media for MediaSessionCompat (used by GpuiMediaSession for system controls)
    implementation("androidx.media:media:1.7.1")
}
