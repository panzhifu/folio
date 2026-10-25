---
name: scrcpy-android
description: Drive an Android app on a scrcpy virtual display for screenshots, taps, swipes and key input without taking over the physical phone screen. Use when inspecting or testing a mobile UI over adb, when the user asks to screenshot a phone app without occupying the phone, or when a task needs repeated Android screen captures and input.
---

# Scrcpy Android（虚拟显示）

用 scrcpy 的 `--new-display` 在 Android 设备上开一块**独立虚拟显示**：APP、截图与
触摸都落在这块显示上，手机主屏仍归读者使用。适用于「看手机上的界面 / 来回点几屏 /
录一段交互」这类需要真实设备的活。

> 前提：所有权与同意。只对用户明确交给你操作的设备使用。截图会看到真实内容，
> `--start-app=+` 会 force-stop 那个 APP（主屏上开着也会被关掉）。

## 先看环境

```bash
adb devices -l                 # 恰好一台 device；多台时先 export ANDROID_SERIAL=<serial>
scrcpy --version               # 需要 5.x；--new-display 需要 Android 12+
adb shell getprop ro.build.version.release
```

## 一条命令跑起来

本 skill 自带脚本，状态落在 `/tmp/folio-scrcpy/`：

```bash
SKILL=.agents/skills/scrcpy-android/scripts/scrcpy-android.sh

# 开一块 1080x2400 / 420dpi 的虚拟显示，并在上面启动 Folio（默认）
$SKILL start

# 或指定 APP：包名，或 scrcpy 的 ?名字（中文名也行）
$SKILL start --app '?普通读者'
$SKILL start --app dev.folio.reader --size 1080x2400/420 --time 300

# 截图 / 点击 / 滑动 / 按键 / 输入
$SKILL screenshot /tmp/shot.png
$SKILL tap 540 1200
$SKILL swipe 540 1900 540 1200 400
$SKILL key BACK
$SKILL text "hello world"

$SKILL status
$SKILL stop                    # 结束会话（录像是 MKV，强杀也能读）
```

脚本会打印两套号，别混：

- **逻辑 display id**（如 `5`）→ 给 `adb shell input -d` 用；
- **SurfaceFlinger display token**（一长串数字）→ 给 `adb exec-out screencap -d` 用。

## 手工版（脚本坏了时照着走）

```bash
# 1. 启动。--new-display 不能只配 --no-window（--no-window 隐含无视频），
#    必须同时录一份，视频管线才在。+ 表示启动前 force-stop。
scrcpy --new-display=1080x2400/420 \
  --start-app='+?普通读者' \
  --no-window --no-audio --time-limit=300 \
  --record=/tmp/scrcpy-session.mkv &

# 2. 从日志取逻辑 display id
#    [server] INFO: New display: 1080x2400/420 (id=5)

# 3. 从 SurfaceFlinger 取截图用的 token（可能有多块，取最新那一块 Virtual display）
adb shell dumpsys SurfaceFlinger --display-id

# 4. 截图（token 是长数字，不是逻辑 id）
adb exec-out screencap -d <token> -p > /tmp/shot.png

# 5. 点击 / 滑动（逻辑 id）
adb shell input -d 5 tap 540 1200
adb shell input -d 5 swipe 540 1900 540 1200 400

# 6. 结束；scrcpy 收到 SIGTERM 后移除虚拟显示。MKV 不依赖收尾，强杀也读得动
kill %1
```

## 坑（都实测过）

- **`--no-window` 与 `--new-display` 不能单独同用**：前者隐含关闭视频，scrcpy 会报
  `--new-display is incompatible with --no-video`。加 `--record` 就有了视频 sink，
  仍然不会开窗口。
- **逻辑 id ≠ 截图 token**。`screencap -d 5` 会报 `Display Id '5' is not valid`；
  要用 `dumpsys SurfaceFlinger --display-id` 里那一长串。
- **旧虚拟显示可能残留**：脚本在启动前记下已有 token，只认新出现的那一块；手工排查
  时也取最后一条 `Virtual display`。
- **锁屏**：虚拟显示上也可能先看到锁屏。需要时 `adb shell wm dismiss-keyguard`
  一次 —— 这动的是**主屏的锁**，先确认用户同意。
- **不要 `adb shell uiautomator dump` 去看虚拟显示**：它不看 `-d`，导出的往往是
  主屏当前界面，可能把用户没打算给你的内容带出来。用虚拟显示的截图。
- **`--start-app=+pkg` 会 force-stop 这个 APP**，主屏上开着的那一份也会被关。只需要
  看界面、不想动主屏上正在用的 APP 时，用不带 `+` 的 `--start-app` 或换一个测试 APP。
- **动画抓不住单帧**：截图有延迟，翻页/滑动这类 200–300ms 的动作用 `--record` 录下来，
  再抽帧：
  ```bash
  ffmpeg -ss 61.2 -t 1.6 -i /tmp/scrcpy-session.mkv \
    -vf "fps=25,scale=270:-1,tile=5x8" -frames:v 1 /tmp/sheet.png
  ```
- 脚本默认 `--time 300` 秒后自动结束；长任务显式加大。会话结束后 `start` 会拒绝复用
  旧状态，重新开一块新的。

## 什么时候不该用它

- 只是读一张已经给到的截图 / 用户已经发了图片：直接 `Read` 那张图。
- 不需要真实设备：优先用桌面窗口、headless UI 测试或仓库夹具。
- 设备没授权、或截图内容可能包含用户隐私且用户没同意：不做。

## 本仓库的验证记录

2026-10-25，MEIZU 20 / Android 16 / scrcpy 5.0.1：用虚拟显示同时验证了参考 APP
「普通读者」的阅读设置与 Folio 的阅读页；手机主屏没有被占用，翻页动画用录屏抽帧
确认。脚本入口与手工命令都在上面。
