//! 桌面阅读全屏开关回归：默认窗口态，顶栏请求才进入系统全屏且能退出。

use crate::ui::features::reader::ReadingPageEvent;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext as _, TestAppContext};

use super::tests::booted::{boot, open_test_reading};

#[gpui_kit::test]
fn reader_fullscreen_stays_off_until_the_top_bar_asks_for_it(cx: &mut TestAppContext) {
    let (handle, shell) = boot(cx, true);
    open_test_reading(&shell, cx);
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("窗口应能渲染");
    assert!(!shell.read_with(cx, |this, _| this.fullscreen_wanted));
    let page = shell.read_with(cx, |this, _| this.reading.as_ref().unwrap().page.clone());
    page.update(cx, |_, cx| cx.emit(ReadingPageEvent::SetFullscreen(true)));
    cx.run_until_parked();
    cx.update_window(handle, |_, window, _| {
        assert!(window.is_fullscreen(), "顶栏请求必须真的切到系统全屏");
    })
    .expect("窗口状态应可读");
    shell.read_with(cx, |this, _| assert_eq!(this.fullscreen_asked, Some(true)));

    page.update(cx, |_, cx| cx.emit(ReadingPageEvent::SetFullscreen(false)));
    cx.run_until_parked();
    cx.update_window(handle, |_, window, _| {
        assert!(!window.is_fullscreen(), "切回小屏必须真的退出系统全屏");
    })
    .expect("窗口状态应可读");
}
