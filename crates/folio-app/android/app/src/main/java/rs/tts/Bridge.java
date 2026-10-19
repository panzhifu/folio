// 上游 tts 0.26.3 的 JNI 桥，逐字抄自
// ~/.cargo/registry/.../tts-0.26.3/examples/android/app/src/main/java/rs/tts/Bridge.java。
//
// 为什么必须随包：tts 的 Android 后端定义了一个 `JNI_OnLoad`（android.rs:30），它
// `find_class("rs/tts/Bridge").expect(...)`。GpuiActivity 每次启动都
// System.loadLibrary（GpuiActivity.java:55），所以这个类不在 APK 里就是**加载期崩**，
// 与听书功能有没有被用到无关。包名 `rs.tts` 与方法名都是 JNI 符号名
// （Java_rs_tts_Bridge_*）的一部分，改名等于把 tts 的导出全断掉。
//
// 已知缺陷，故意先不改：下面 `onStop` 的第二个参数是装箱的 `Boolean`，而 SDK 的
// `UtteranceProgressListener.onStop(String, boolean)` 是原始类型 —— 这是**重载不是覆盖**，
// 框架调的是基类那个空实现，所以 tts 的 `on_utterance_stop` 在 Android 上永远不来。
// 而 `onDone` 被 tts 映射进了 `utterance_stop`、`onError` 映射进了 `utterance_end`
// （逐行见 android.rs:80 / :107 / :134 / :161），跟 Linux 那侧正好相反。
// 把 `Boolean` 换成 `boolean` 就能让 `onStop` 真被调到，但那会多出一条喂给
// `utterance_end` 的事件 —— 要改就连同 Rust 侧的映射表一起改，别在这里单独动。
package rs.tts;

import android.speech.tts.TextToSpeech;
import android.speech.tts.UtteranceProgressListener;

@androidx.annotation.Keep
public class Bridge extends UtteranceProgressListener implements TextToSpeech.OnInitListener {
    public int backendId;

    public Bridge(int backendId) {
        this.backendId = backendId;
    }

    public native void onInit(int status);

    public native void onStart(String utteranceId);

    public native void onStop(String utteranceId, Boolean interrupted);

    public native void onDone(String utteranceId);

    public native void onError(String utteranceId) ;

}
