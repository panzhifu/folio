// Root Gradle project for Folio on Android.
//
// The native library is produced by cargo-ndk from the `folio-app` crate one
// directory up (lib → libfolio_app.so), into app/src/main/jniLibs; this project
// only packages it.
//
//   cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build --release
//   ./gradlew assembleDebug
//   adb install -r app/build/outputs/apk/debug/app-debug.apk

buildscript {
    repositories {
        google()
        mavenCentral()
    }
    dependencies {
        classpath("com.android.tools.build:gradle:9.1.0")
    }
}

tasks.register("clean", Delete::class) {
    delete(rootProject.layout.buildDirectory)
}
