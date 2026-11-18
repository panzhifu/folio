use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    Styled as _, div,
};

use crate::ui::features::reader::view::reading_page::ReadingPage;
use crate::ui::services::l10n::tr;
use crate::ui::services::source_updates;

impl ReadingPage {
    pub fn goto_record(&mut self, bookmark: bool, id: i64, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.goto_record(bookmark, id, cx));
    }
    pub(in crate::ui::features::reader) fn content_update_notice(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let reader = self.reader.read(cx);
        let pending = reader.content_version.pending.is_some();
        let failure = reader.content_version.failure.clone();
        let progress = source_updates::handle(cx).and_then(|service| {
            let book_id = reader.content_version.opening.or(reader.book_id())?;
            service.read(cx).reader_progress(book_id)
        });
        if !pending && failure.is_none() && progress.is_none() {
            return None;
        }
        let mut content = v_flex().gap_2();
        if let Some(progress) = progress {
            content = content.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().text_sm().child(progress.label(cx)))
                    .child(
                        Button::new("reader-cancel-update")
                            .outline()
                            .disabled(progress.cancelling)
                            .label(tr(cx, "source_update.cancel").to_string())
                            .on_click(|_, _, cx| source_updates::cancel(cx)),
                    ),
            );
        }
        if pending {
            let page = cx.entity();
            content = content.child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .child(tr(cx, "source_update.ready")),
                    )
                    .child(
                        Button::new("reader-adopt-update")
                            .outline()
                            .label(tr(cx, "source_update.switch").to_string())
                            .on_click(move |_, _, cx| {
                                page.update(cx, |this, cx| {
                                    this.reader
                                        .update(cx, |reader, cx| reader.adopt_content_update(cx))
                                })
                            }),
                    ),
            );
        }
        if let Some(failure) =
            failure.filter(|failure| progress.is_none() || failure != "source_update.verifying")
        {
            let message = if failure.starts_with("source_update.") {
                tr(cx, &failure).to_string()
            } else {
                failure
            };
            content = content.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(message),
            );
        }
        Some(
            div()
                .id("reader-content-update")
                .occlude()
                .absolute()
                .top_10()
                .left_4()
                .right_4()
                .p_3()
                .rounded_md()
                .bg(cx.theme().popover)
                .border_1()
                .border_color(cx.theme().border)
                .text_color(cx.theme().popover_foreground)
                .child(content)
                .into_any_element(),
        )
    }
}
