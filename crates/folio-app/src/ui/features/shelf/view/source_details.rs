//! 原件关联与阅读副本是不同事实；一次性输入不显示可持续跟踪。

use std::path::PathBuf;

use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, h_flex, v_flex};
use gpui_kit::{AnyElement, App, IntoElement as _, ParentElement as _, Styled as _, div};

use crate::ui::services::l10n::tr;
use crate::ui::services::source_updates;

pub(in crate::ui::features::shelf) fn section(book_id: i64, cx: &App) -> AnyElement {
    let service = source_updates::handle(cx);
    let status = service
        .as_ref()
        .map(|service| service.read(cx).book(book_id))
        .unwrap_or_default();
    let busy = service
        .as_ref()
        .is_some_and(|service| service.read(cx).is_busy());
    let primary = status.sources.iter().find(|source| source.is_primary);
    let key = match primary {
        None => "source_update.copy_only",
        Some(source) if source.state == "relink-required" => "source_update.relink_required",
        Some(source)
            if matches!(
                source.state.as_str(),
                "unavailable" | "missing" | "unreadable"
            ) =>
        {
            "source_update.unavailable"
        }
        Some(_) => "source_update.linked",
    };
    let mut section = v_flex()
        .w_full()
        .gap_2()
        .child(div().text_sm().child(tr(cx, "source_update.original")))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr(cx, key)),
        );
    if let Some(source) = primary {
        let path = PathBuf::from(&source.locator);
        section = section.child(div().text_sm().truncate().child(source.locator.clone()));
        if source.kind == "local"
            && let Some(parent) = path.parent()
        {
            let parent = parent.to_path_buf();
            section = section.child(
                Button::new("source-open-folder")
                    .outline()
                    .label(tr(cx, "source_update.open_folder").to_string())
                    .on_click(move |_, _, _| {
                        crate::ui::platform::open_folder(&parent);
                    }),
            );
        }
    }
    if let Some(message) = status.message.as_ref() {
        let message = if message.starts_with("source_update.") {
            tr(cx, message).to_string()
        } else {
            message.clone()
        };
        section = section.child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(message),
        );
    }
    if let Some(service) = &service {
        let state = service.read(cx);
        if let Some(progress) = state.progress(book_id) {
            section = section.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().text_sm().child(progress.label(cx)))
                    .child(
                        Button::new("source-cancel")
                            .outline()
                            .disabled(progress.cancelling)
                            .label(tr(cx, "source_update.cancel").to_string())
                            .on_click(|_, _, cx| source_updates::cancel(cx)),
                    ),
            );
        } else if status.message.is_none()
            && let Some(message) = state.last_message()
        {
            section = section.child(div().text_sm().child(tr(cx, message)));
        }
    }
    section
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("source-check")
                        .outline()
                        .disabled(busy)
                        .label(tr(cx, "source_update.check").to_string())
                        .on_click(|_, _, cx| source_updates::check_now(cx)),
                )
                .child(
                    Button::new("source-relink")
                        .outline()
                        .disabled(busy)
                        .label(tr(cx, "source_update.relink").to_string())
                        .on_click(move |_, _, cx| source_updates::pick_replacement(book_id, cx)),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use folio_core::{Config, Store, TaskManager};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AppContext as _, Context, Render, Subscription, TestAppContext, Window, px, size,
    };

    use crate::ui::services::{app_store, config::ConfigHolder, source_updates};

    struct SourceDetails {
        _watch: Subscription,
    }

    impl Render for SourceDetails {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui_kit::IntoElement {
            super::section(42, cx)
        }
    }

    fn wait(cx: &mut TestAppContext, predicate: impl Fn(&gpui_kit::App) -> bool) {
        for _ in 0..300 {
            cx.background_executor.run_until_parked();
            if cx.update(|cx| predicate(cx)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("source update UI did not settle");
    }

    #[gpui_kit::test]
    fn source_revision_cancel_button_keeps_work_busy_until_the_worker_stops(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        cx.background_executor.allow_parking();
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            app_store::install(Store::in_memory().unwrap(), cx);
        });
        let pool = TaskManager::new(1);
        let (release, released) = mpsc::channel();
        let job = pool.submit(move |ctx| {
            ctx.bytes(50, Some(100), "copy");
            released.recv_timeout(Duration::from_secs(10)).unwrap();
            Ok(())
        });
        let cancellation = job.cancel.clone();
        cx.update(|cx| source_updates::track_job_for_test(job, 42, cx));
        let window = cx.open_window(size(px(640.0), px(480.0)), |window, cx| {
            let source = source_updates::handle(cx).unwrap();
            let view = cx.new(|cx| SourceDetails {
                _watch: cx.observe(&source, |_, _, cx| cx.notify()),
            });
            Root::new(view, window, cx)
        });
        wait(cx, |cx| {
            source_updates::handle(cx)
                .unwrap()
                .read(cx)
                .progress(42)
                .is_some_and(|progress| progress.percent == Some(50))
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("source-check").disabled(), Some(true));
            window.click("source-cancel", cx);
            assert!(cancellation.triggered());
            assert!(source_updates::handle(cx).unwrap().read(cx).is_busy());
            window.render_frame(cx);
            assert_eq!(window.find("source-cancel").disabled(), Some(true));
        })
        .unwrap();
        release.send(()).unwrap();
        wait(cx, |cx| {
            !source_updates::handle(cx).unwrap().read(cx).is_busy()
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("source-cancel").is_none());
            assert_ne!(window.find("source-check").disabled(), Some(true));
            assert_eq!(
                source_updates::handle(cx).unwrap().read(cx).last_message(),
                Some("source_update.cancelled")
            );
        })
        .unwrap();
        pool.shutdown();
    }
}
