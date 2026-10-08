use gpui_kit::component::*;
use gpui_kit::*;

struct Welcome;

impl Render for Welcome {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .v_flex()
            .items_center()
            .justify_center()
            .child("Folio")
    }
}

fn main() {
    gpui_kit::application().run(|cx: &mut App| {
        gpui_kit::init(cx);
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("Folio".into()),
                ..Default::default()
            }),
            // gpui-pre 默认继承 Zed 的 app_id，Wayland 下会导致窗口分组 / 图标 / .desktop 关联错位。
            app_id: Some("dev.folio.Reader".into()),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |_window, cx| cx.new(|_cx| Welcome))
            .expect("failed to open Folio window");
    });
}
