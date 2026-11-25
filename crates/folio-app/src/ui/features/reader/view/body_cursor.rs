//! 正文的指针形状：命中到哪一格、指针就该长什么样，以及承载它的那一层。
//!
//! `cursor` 是界面提示，不改版面；链接没写时补手形这件事也在这里（与点击共用
//! `ReaderSession::cursor_at` 的命中）。

use folio_core::content::format::css::computed::Cursor;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, CursorStyle, Div, InteractiveElement as _,
    MouseMoveEvent, ParentElement as _, Stateful, StatefulInteractiveElement as _, Styled as _,
    Window, div,
};

use super::reading_page::ReadingPage;

/// 书里的 `cursor` → gpui 的指针形状。`auto` 交给平台；链接的 `auto` 已在
/// `ReaderSession::cursor_at` 里折成 `Pointer`。
pub(super) fn style(cursor: Cursor) -> Option<CursorStyle> {
    Some(match cursor {
        Cursor::Auto => return None,
        Cursor::Default => CursorStyle::Arrow,
        Cursor::Pointer => CursorStyle::PointingHand,
        Cursor::Text => CursorStyle::IBeam,
        Cursor::Crosshair => CursorStyle::Crosshair,
        Cursor::Grab => CursorStyle::OpenHand,
        Cursor::Grabbing => CursorStyle::ClosedHand,
        Cursor::NotAllowed => CursorStyle::OperationNotAllowed,
        Cursor::ContextMenu => CursorStyle::ContextualMenu,
        Cursor::Copy => CursorStyle::DragCopy,
        Cursor::Alias => CursorStyle::DragLink,
        Cursor::ColResize => CursorStyle::ResizeColumn,
        Cursor::RowResize => CursorStyle::ResizeRow,
        Cursor::VerticalText => CursorStyle::IBeamCursorForVerticalLayout,
        Cursor::EResize => CursorStyle::ResizeRight,
        Cursor::WResize => CursorStyle::ResizeLeft,
        Cursor::NResize => CursorStyle::ResizeUp,
        Cursor::SResize => CursorStyle::ResizeDown,
        Cursor::EwResize => CursorStyle::ResizeLeftRight,
        Cursor::NsResize => CursorStyle::ResizeUpDown,
        Cursor::NeswResize => CursorStyle::ResizeUpLeftDownRight,
        Cursor::NwseResize => CursorStyle::ResizeUpRightDownLeft,
    })
}

impl ReadingPage {
    /// 鼠标划过正文：按命中到的元素换形状，只在**变了**时重画。
    pub(super) fn track_cursor(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cursor = self
            .reader
            .read(cx)
            .cursor_at(window, cx, event.position)
            .and_then(style);
        if self.body_cursor != cursor {
            self.body_cursor = cursor;
            cx.notify();
        }
    }
}

/// 正文那一层：整屏命中、点按与指针形状。指针形状挂在**这一层自己的 hitbox**
/// 上，于是浮层按钮的 hitbox 压在上面时仍按按钮的那一份走。
pub(super) fn layer(
    tap: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_move: impl Fn(&MouseMoveEvent, &mut Window, &mut App) + 'static,
    child: AnyElement,
    cursor: Option<CursorStyle>,
) -> Stateful<Div> {
    let layer = div()
        .id("reader-body")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .on_click(tap)
        .on_mouse_move(on_move)
        .child(child);
    match cursor {
        Some(cursor) => layer.cursor(cursor),
        None => layer,
    }
}

#[cfg(test)]
mod tests {
    use super::style;
    use folio_core::content::format::css::computed::Cursor;
    use gpui_kit::CursorStyle;

    /// 关键字到 gpui 形状：auto 交给平台，其余逐一对应。
    #[test]
    fn cursor_keywords_map_to_gpui_styles() {
        assert_eq!(style(Cursor::Auto), None);
        assert_eq!(style(Cursor::Pointer), Some(CursorStyle::PointingHand));
        assert_eq!(style(Cursor::Text), Some(CursorStyle::IBeam));
        assert_eq!(
            style(Cursor::NotAllowed),
            Some(CursorStyle::OperationNotAllowed)
        );
    }
}
