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

import java.util.Properties

plugins {
    id("com.android.application")
}

// ---- release 签名 ----
//
// 找到 keystore 就签，找不到就按环境分两种结局，这差别是有意的：
//
// - **本地开发**退回 debug 签名并 warn 一句。私钥只在两处离线备份里，不该要求每个工作副本
//   都持有它才能跑 `assembleRelease` 试打包形状。
// - **CI 里硬失败**。静默产出一个 debug 签名的 release 包并发出去，等于亲手触发那条不可逆
//   事故：系统按签名判「是不是同一个应用」，发出去的包永远换不了签名，届时所有已装用户
//   只能卸载重装。宁可红在这里。
//
// 配置的两个来源，按顺序取第一个能用的：
//   1. 一份 properties 文件：`$FOLIO_KEYSTORE_PROPERTIES` 指哪儿读哪儿 → 否则试
//      `~/.local/share/folio-keystore/keystore.properties`（本机的默认位置，**在仓库之外**）→
//      否则试本目录的 `keystore.properties`（已被 .gitignore 收）。
//   2. 环境变量（CI 那一侧的路子，secret 不进文件系统配置）：`FOLIO_KEYSTORE_FILE` /
//      `FOLIO_KEYSTORE_PASSWORD` / `FOLIO_KEY_ALIAS` / `FOLIO_KEY_PASSWORD`。
// 文件里那几个键名与环境变量名各管各的，两边都填时**文件优先**。
//
// keystore 本身（`.p12`）永不进版本库，也不在 CI 里解到 workspace —— 解到 workspace 有被
// `upload-artifact` 顺手传出去的风险，那等于把私钥发到公网。CI 请解到 `$RUNNER_TEMP`。
val keystorePropsFile = listOfNotNull(
    System.getenv("FOLIO_KEYSTORE_PROPERTIES"),
    System.getProperty("user.home")?.let { "$it/.local/share/folio-keystore/keystore.properties" },
    "keystore.properties",
).map(::File).firstOrNull { it.isFile }

val keystoreProps = Properties().also { p ->
    keystorePropsFile?.let { f -> f.inputStream().use { p.load(it) } }
}

fun signValue(key: String, env: String): String? =
    keystoreProps.getProperty(key)?.takeIf { it.isNotBlank() }
        ?: System.getenv(env)?.takeIf { it.isNotBlank() }

val releaseStoreFile = signValue("storeFile", "FOLIO_KEYSTORE_FILE")
val releaseStorePassword = signValue("storePassword", "FOLIO_KEYSTORE_PASSWORD")
val releaseKeyAlias = signValue("keyAlias", "FOLIO_KEY_ALIAS")
val releaseKeyPassword = signValue("keyPassword", "FOLIO_KEY_PASSWORD")

val releaseSigningReady =
    listOf(releaseStoreFile, releaseStorePassword, releaseKeyAlias, releaseKeyPassword)
        .all { it != null }

val runningInCi = System.getenv("GITHUB_ACTIONS") == "true" ||
    System.getenv("CI").orEmpty().equals("true", ignoreCase = true)

if (!releaseSigningReady) {
    val missing = buildString {
        append("storeFile / storePassword / keyAlias / keyPassword 里至少一项没拿到")
        keystorePropsFile?.let { append("（试过的配置文件：$it）") } ?: append("（没找到任何 keystore.properties）")
    }
    if (runningInCi) {
        throw GradleException(
            "release 签名配置缺失：$missing。CI 里这四枚 secret 要到位：" +
                "FOLIO_KEYSTORE_FILE / FOLIO_KEYSTORE_PASSWORD / FOLIO_KEY_ALIAS / FOLIO_KEY_PASSWORD。" +
                "**不能退回 debug 签名** —— 发出去的包一旦是 debug 签的，以后换不了签名。",
        )
    }
    logger.warn(
        "folio: 没有 release 签名配置，$missing；`assembleRelease` 这次会退回 **debug 签名**。" +
            "本地试打包形状没问题，但**这种包绝不要发布**（详见 app/build.gradle.kts 顶部那段）。" +
            "要接上真签名：把 keystore.properties 放到 ~/.local/share/folio-keystore/，" +
            "或设 FOLIO_KEYSTORE_PROPERTIES 指到别处。",
    )
}

android {
    namespace = "dev.folio.reader"
    compileSdk = 34

    signingConfigs {
        if (releaseSigningReady) {
            create("folioRelease") {
                // 路径来自配置文件或环境变量，所以这里**不写死绝对路径** —— 写死的那一份
                // 在别人机器上与 CI 上都必然不存在，症状是「配置读到了却签不上」。
                storeFile = file(releaseStoreFile!!)
                storePassword = releaseStorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
                storeType = "PKCS12"
            }
        }
    }

    defaultConfig {
        applicationId = "dev.folio.reader"
        minSdk = 26          // Vulkan 1.0 is mandatory from API 26+
        targetSdk = 34
        // versionCode 每次发版都要 +1：系统按它判「这个包能不能覆盖上一个」，不递增的症状是安装器
        // 拒绝覆盖安装，看着像自动更新坏了。Cargo 版本与 tag 那两处有 CI 一致性校验，只有这一处
        // 没人管，所以单独留一行提醒。发版配方见 README「发版」那一节。
        versionCode = 3
        versionName = "0.1.2"

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
            // 拿到配置就签正式包，本地拿不到退回 debug（上面已经 warn 过），CI 拿不到直接失败。
            signingConfig = if (releaseSigningReady) {
                signingConfigs.getByName("folioRelease")
            } else {
                signingConfigs.getByName("debug")
            }
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
            //
            // 第三次发生是格式那一路（2026-10-23 实测）：`pdf_oxide` / `office_oxide` /
            // `djvu_rs` / `sevenz_rust2` 四个包都声明 `crate-type = ["cdylib", "rlib", …]`，
            // 链进主库的是 rlib 那一份，cargo-ndk 却把 cdylib 那一份也拷进 jniLibs，于是 APK 里
            // 多出 20,313,904 B（15.96 + 2.42 + 0.71 + 0.28 MiB，全部 stored 不压缩）—— 占整包
            // 70.07 MB 的 29%。判据与前两条同形、三条都过：`libfolio_app.so` 的 NEEDED 只有
            // liblog/libandroid/libdl/libm/libc；整库 `strings` 里搜 `libpdf_oxide` /
            // `liboffice_oxide` / `libdjvu_rs` / `libsevenz_rust2` 全 0 命中（同一个管道搜裸名
            // `pdf_oxide` 命中 286 行，证明不是搜索失灵而是真没有）；这四个包自己 `src/` 里没有
            // `libloading`/`dlopen`，我们图里那份 `libloading` 是 `ash` 载 Vulkan 驱动用的。
            //
            // 第四次是 `.doc` 那一路（2026-10-26 实测）：`rwml` 0.1.5 同样声明
            // `crate-type = ["rlib", "cdylib"]`，APK 里于是多出两份（12,444,520 + 298,672
            // = 12,743,192 B，stored 不压缩），占 62.5 MB 那包的 20%。判据三条同形且都过：
            // NEEDED 只有 liblog/libandroid/libdl/libm/libc；整库 `strings` 搜 `librwml`
            // 0 命中；rwml 的 `src/` 里也没有 `dlopen`/`libloading`。
            //
            // 这一格会自己复发：每加一个「cdylib + rlib 双形」的依赖就多一份副本，而它不出现在
            // 任何编译错误里，只出现在包体积上。判据是 `unzip -l` 里 `lib/` 那一段有没有带
            // `-[0-9a-f]{16}` 哈希后缀的名字 —— 那种名字就是 cargo-ndk 拷进来的副本。
            excludes += listOf(
                "**/libgpui_mobile*.so",
                "**/libtts*.so",
                "**/libpdf_oxide*.so",
                "**/liboffice_oxide*.so",
                "**/libdjvu_rs*.so",
                "**/libsevenz_rust2*.so",
                "**/librwml*.so",
            )
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
