use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, h_flex, v_flex};
use gpui_kit::{AnyElement, App, IntoElement as _, ParentElement as _, Styled as _, div};

use crate::ui::components::muted_label;
use crate::ui::services::backup::{self, human_bytes, restore};
use crate::ui::services::l10n::{tf, tr};

pub(super) fn block(cx: &App) -> AnyElement {
    let entity = backup::handle(cx);
    let state = entity.as_ref().map(|entity| entity.read(cx));
    let restoring = state.map(|state| state.restore());
    let busy = state.is_some_and(|state| state.busy());
    let mut block = v_flex().w_full().gap_3();
    if let Some(prepared) = restoring.and_then(|state| state.prepared()) {
        block = block
            .child(muted_label(tr(cx, "backup.restore.ready"), cx))
            .child(muted_label(prepared.root.display().to_string(), cx));
        if crate::ui::platform::CAN_QUIT_FOR_RESTORE {
            block = block.child(
                Button::new("restore-quit")
                    .label(tr(cx, "backup.restore.quit").to_string())
                    .on_click(|_, _, cx| cx.quit()),
            );
        }
        return block.into_any_element();
    }
    block = block.child(
        h_flex()
            .w_full()
            .gap_2()
            .child(
                Button::new("restore-pick")
                    .label(tr(cx, "backup.restore.pick").to_string())
                    .disabled(busy)
                    .on_click(|_, _, cx| restore::pick(cx)),
            )
            .child(muted_label(tr(cx, "backup.restore.description"), cx)),
    );
    if restoring.is_some_and(|state| state.is_busy()) {
        block = block.child(muted_label(tr(cx, "backup.restore.working"), cx));
    }
    if let Some(state) = restoring {
        if let Some(preview) = state.preview() {
            let archive = state
                .archive()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy())
                .unwrap_or_default();
            block = block
                .child(muted_label(archive.to_string(), cx))
                .child(muted_label(
                    tf(
                        cx,
                        "backup.restore.summary",
                        &[
                            ("version", &preview.manifest.app_version),
                            ("size", &human_bytes(cx, preview.unpacked_bytes)),
                        ],
                    ),
                    cx,
                ))
                .child(muted_label(
                    tr(
                        cx,
                        if preview.manifest.include_files {
                            "backup.restore.full"
                        } else {
                            "backup.restore.data_only"
                        },
                    ),
                    cx,
                ));
            if preview.contains_credentials {
                block = block.child(
                    Checkbox::new("restore-credentials")
                        .label(tr(cx, "backup.restore.credentials").to_string())
                        .checked(state.includes_credentials())
                        .disabled(busy)
                        .on_change(|checked, _, cx| restore::include_credentials(cx, *checked)),
                );
            }
            block = block
                .child(muted_label(tr(cx, "backup.restore.switch_warning"), cx))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("restore-cancel")
                                .ghost()
                                .label(tr(cx, "shelf.cancel").to_string())
                                .disabled(busy)
                                .on_click(|_, _, cx| restore::dismiss(cx)),
                        )
                        .child(
                            Button::new("restore-prepare")
                                .primary()
                                .label(tr(cx, "backup.restore.prepare").to_string())
                                .disabled(busy)
                                .on_click(|_, _, cx| restore::prepare(cx)),
                        ),
                );
        }
        if let Some(reason) = state.failure() {
            block = block.child(div().text_sm().text_color(cx.theme().danger).child(tf(
                cx,
                "backup.restore.failed",
                &[("reason", reason)],
            )));
        }
    }
    block.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::block;
    use crate::ui::services::backup::{entity, restore};
    use crate::ui::services::config::ConfigHolder;
    use folio_core::backup::{Options, Roots, create};
    use folio_core::{Config, Store};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AppContext as _, Bounds, Context, Render, Subscription, TestAppContext, Window,
        WindowBounds, WindowOptions, px, size,
    };

    struct RestoreView {
        _watch: Subscription,
    }
    impl Render for RestoreView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui_kit::IntoElement {
            block(cx)
        }
    }

    #[gpui_kit::test]
    fn restore_controls_toggle_credentials_and_cancel_the_preview(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
        });
        let temp = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let mut config = Config::default();
        config.sync.password = "fixture-secret".into();
        let roots = Roots {
            backups: temp.path().join("backups"),
            data: temp.path().into(),
            library: None,
            dicts: None,
            fonts: None,
            themes: None,
        };
        let archive = create(&store, &config, &roots, Options::default(), &mut |_, _| {}).unwrap();
        cx.update(|cx| restore::inspect(cx, archive.path));
        for _ in 0..400 {
            cx.background_executor.run_until_parked();
            if cx.update(|cx| !entity(cx).read(cx).busy()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let (handle, _view) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(720.), px(620.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |_, cx| {
                    cx.new(|cx| {
                        let state = entity(cx);
                        RestoreView {
                            _watch: cx.observe(&state, |_, _, cx| cx.notify()),
                        }
                    })
                },
            )
            .unwrap()
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("restore-credentials").checked(), Some(false));
            window.click("restore-credentials", cx);
            assert_eq!(window.find("restore-credentials").checked(), Some(true));
            assert!(entity(cx).read(cx).restore().includes_credentials());
            window.click("restore-cancel", cx);
            assert!(entity(cx).read(cx).restore().preview().is_none());
            assert!(window.try_find("restore-prepare").is_none());
        })
        .unwrap();
    }
}
