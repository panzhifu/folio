#!/usr/bin/env bash
# A small wrapper around scrcpy's virtual-display mode.
#
# It keeps one session at a time in $FOLIO_SCRCPY_STATE (default
# /tmp/folio-scrcpy): starts scrcpy with --new-display, records the display id
# used by `adb shell input -d` and the SurfaceFlinger token used by
# `adb exec-out screencap -d`, then offers screenshot / tap / swipe / key / text.
#
# scrcpy requires a video sink when --new-display is used with --no-window,
# so every session records a Matroska file (MKV survives a kill without a
# finalization step; MP4 needs its moov atom written on exit).

set -euo pipefail

STATE_DIR="${FOLIO_SCRCPY_STATE:-/tmp/folio-scrcpy}"
DEFAULT_APP="${FOLIO_SCRCPY_APP:-dev.folio.reader}"
DEFAULT_SIZE="${FOLIO_SCRCPY_SIZE:-1080x2400/420}"
DEFAULT_TIME="${FOLIO_SCRCPY_TIME:-300}"

usage() {
    cat <<'EOF'
Usage: scrcpy-android.sh <command> [options]

Commands:
  start [--app PKG|?NAME|none] [--size WxH[/DPI]] [--time SECONDS]
        Start a fresh scrcpy virtual display. Default app: dev.folio.reader,
        default size: 1080x2400/420, default lifetime: 300s. `none` starts
        without an app. `?Name` selects by app label (e.g. '?普通读者').
  screenshot [PATH]
        Capture the virtual display (default: /tmp/folio-scrcpy/shot-<time>.png).
  tap X Y
        Tap on the virtual display.
  swipe X1 Y1 X2 Y2 [DURATION_MS]
        Swipe on the virtual display (default 300 ms).
  key KEYCODE
        Send a key event (e.g. BACK, HOME, KEYCODE_ENTER).
  text TEXT...
        Type ASCII text (spaces become %s; adb input cannot type CJK).
  status
        Show the active session and its two ids.
  log
        Tail the scrcpy log.
  stop
        Stop the session and keep /tmp/folio-scrcpy/session.mkv.

Environment:
  ANDROID_SERIAL        Pick a device when several are attached.
  FOLIO_SCRCPY_STATE    State directory (default /tmp/folio-scrcpy).
  FOLIO_SCRCPY_APP      Default app for `start`.
  FOLIO_SCRCPY_SIZE     Default --new-display size.
  FOLIO_SCRCPY_TIME     Default --time-limit seconds.
EOF
}

die() {
    printf 'scrcpy-android: %s\n' "$*" >&2
    exit 1
}

require_tools() {
    command -v adb >/dev/null 2>&1 || die "adb not found in PATH"
    command -v scrcpy >/dev/null 2>&1 || die "scrcpy not found in PATH"
    [[ "$(adb get-state 2>/dev/null || true)" == "device" ]] ||
        die "no adb device (adb devices -l)"
    if [[ -z "${ANDROID_SERIAL:-}" ]]; then
        local count
        count=$(adb devices | awk 'NR > 1 && $2 == "device" { n++ } END { print n + 0 }')
        [[ "$count" -le 1 ]] ||
            die "multiple adb devices; export ANDROID_SERIAL=<serial> first"
    fi
}

load_state() {
    [[ -f "$STATE_DIR/state.env" ]] || die "no session; run: $0 start"
    # shellcheck disable=SC1091
    . "$STATE_DIR/state.env"
}

session_alive() {
    [[ -n "${SCRCPY_PID:-}" ]] && kill -0 "$SCRCPY_PID" 2>/dev/null
}

new_virtual_token() {
    local before="$STATE_DIR/before-tokens"
    adb shell dumpsys SurfaceFlinger --display-id 2>/dev/null |
        awk '/Virtual display/ { print $2 }' |
        grep -vxF -f "$before" 2>/dev/null |
        tail -1 || true
}

cmd_start() {
    local app="$DEFAULT_APP" size="$DEFAULT_SIZE" time_limit="$DEFAULT_TIME"
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --app) app="${2:?--app needs a value}"; shift 2 ;;
            --size) size="${2:?--size needs a value}"; shift 2 ;;
            --time) time_limit="${2:?--time needs a value}"; shift 2 ;;
            -h|--help) usage; exit 0 ;;
            *) die "unknown start option: $1" ;;
        esac
    done

    require_tools
    mkdir -p "$STATE_DIR"
    if [[ -f "$STATE_DIR/state.env" ]]; then
        # shellcheck disable=SC1091
        . "$STATE_DIR/state.env"
        if session_alive; then
            die "session already running (pid $SCRCPY_PID); run: $0 stop"
        fi
    fi

    rm -f "$STATE_DIR/session.log" "$STATE_DIR/session.mkv" "$STATE_DIR/state.env"
    adb shell dumpsys SurfaceFlinger --display-id 2>/dev/null |
        awk '/Virtual display/ { print $2 }' > "$STATE_DIR/before-tokens" || true

    local args=(
        --new-display="$size"
        --no-window
        --no-audio
        --time-limit="$time_limit"
        --record="$STATE_DIR/session.mkv"
    )
    if [[ -n "$app" && "$app" != "none" ]]; then
        args+=(--start-app="+$app")
    fi

    nohup scrcpy "${args[@]}" > "$STATE_DIR/session.log" 2>&1 &
    local pid=$!
    printf '%s\n' "$pid" > "$STATE_DIR/pid"

    local display_id="" token=""
    for _ in $(seq 1 100); do
        if ! kill -0 "$pid" 2>/dev/null; then
            printf 'scrcpy exited before a display was ready:\n' >&2
            tail -n 30 "$STATE_DIR/session.log" >&2 || true
            exit 1
        fi
        display_id=$(grep -o 'id=[0-9]\+' "$STATE_DIR/session.log" 2>/dev/null |
            tail -1 | cut -d= -f2 || true)
        token=$(new_virtual_token)
        [[ -n "$display_id" && -n "$token" ]] && break
        sleep 0.2
    done

    if [[ -z "$display_id" || -z "$token" ]]; then
        tail -n 30 "$STATE_DIR/session.log" >&2 || true
        kill -INT "$pid" 2>/dev/null || true
        die "timed out waiting for the virtual display"
    fi

    cat > "$STATE_DIR/state.env" <<EOF
SCRCPY_PID=$pid
DISPLAY_ID=$display_id
DISPLAY_TOKEN=$token
EOF

    cat <<EOF
session started
  pid:            $pid
  app:            $app
  size:           $size
  display id:     $display_id  (adb shell input -d $display_id ...)
  screenshot id:  $token  (adb exec-out screencap -d $token ...)
  log:            $STATE_DIR/session.log
  recording:      $STATE_DIR/session.mkv
  lifetime:       ${time_limit}s
EOF
}

cmd_screenshot() {
    load_state
    local path="${1:-$STATE_DIR/shot-$(date +%Y%m%d-%H%M%S).png}"
    if ! adb exec-out screencap -d "$DISPLAY_TOKEN" -p > "$path"; then
        rm -f "$path"
        die "screencap failed; is the session still alive? run: $0 status"
    fi
    if head -c 32 "$path" | grep -q 'Failed to'; then
        cat "$path" >&2
        rm -f "$path"
        die "screencap rejected the display token; run: $0 status"
    fi
    printf '%s\n' "$path"
}

integer() {
    [[ "$1" =~ ^-?[0-9]+$ ]] || die "not an integer: $1"
}

cmd_tap() {
    [[ $# -eq 2 ]] || die "tap needs X Y"
    integer "$1"; integer "$2"
    load_state
    adb shell input -d "$DISPLAY_ID" tap "$1" "$2" >/dev/null
}

cmd_swipe() {
    [[ $# -eq 4 || $# -eq 5 ]] || die "swipe needs X1 Y1 X2 Y2 [DURATION_MS]"
    integer "$1"; integer "$2"; integer "$3"; integer "$4"
    local duration="${5:-300}"
    integer "$duration"
    load_state
    adb shell input -d "$DISPLAY_ID" swipe "$1" "$2" "$3" "$4" "$duration" >/dev/null
}

cmd_key() {
    [[ $# -eq 1 ]] || die "key needs a keycode name"
    load_state
    adb shell input -d "$DISPLAY_ID" keyevent "$1" >/dev/null
}

cmd_text() {
    [[ $# -gt 0 ]] || die "text needs a value"
    load_state
    local text="$*"
    text="${text// /%s}"
    adb shell input -d "$DISPLAY_ID" text "$text" >/dev/null
}

cmd_status() {
    if [[ ! -f "$STATE_DIR/state.env" ]]; then
        echo "no session"
        return 0
    fi
    load_state
    if session_alive; then
        echo "running (pid $SCRCPY_PID)"
    else
        echo "stopped (pid $SCRCPY_PID is gone)"
    fi
    echo "display id:    $DISPLAY_ID"
    echo "screenshot id: $DISPLAY_TOKEN"
    echo "record:        $STATE_DIR/session.mkv"
    echo "log:           $STATE_DIR/session.log"
}

cmd_log() {
    [[ -f "$STATE_DIR/session.log" ]] || die "no session log"
    tail -n 60 "$STATE_DIR/session.log"
}

cmd_stop() {
    load_state
    if session_alive; then
        # SIGTERM, not SIGINT: a background job from a non-interactive shell
        # inherits SIGINT as ignored, so scrcpy would never see it. SIGTERM is
        # handled by scrcpy and lets it remove the virtual display and finalize
        # the recording.
        kill -TERM "$SCRCPY_PID" 2>/dev/null || true
        for _ in $(seq 1 100); do
            session_alive || break
            sleep 0.1
        done
        if session_alive; then
            kill -KILL "$SCRCPY_PID" 2>/dev/null || true
            for _ in $(seq 1 20); do
                session_alive || break
                sleep 0.1
            done
        fi
    fi
    if session_alive; then
        die "could not stop pid $SCRCPY_PID; inspect: $STATE_DIR/session.log"
    fi
    echo "stopped; recording: $STATE_DIR/session.mkv"
}

command="${1:-}"
case "$command" in
    start) shift; cmd_start "$@" ;;
    screenshot) shift; cmd_screenshot "$@" ;;
    tap) shift; cmd_tap "$@" ;;
    swipe) shift; cmd_swipe "$@" ;;
    key) shift; cmd_key "$@" ;;
    text) shift; cmd_text "$@" ;;
    status) cmd_status ;;
    log) cmd_log ;;
    stop) cmd_stop ;;
    -h|--help|help|"") usage ;;
    *) usage >&2; exit 2 ;;
esac
