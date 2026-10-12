//! 一格卡片：封面那一格 + 书名 + 作者，以及它自己手上的两桩手势。
//!
//! 卡片单独成文件，是为了让「一张卡长什么样」只有一处可改：[`Card`] 是库行摊平到
//! 界面直接用的样子（封面已解码、作者已并成一行），[`ShelfView::card`] 把一条
//! [`ShelfItem`] 变成它，[`ShelfView::card_element`] 把它画成一格。排格子的事留在
//! [`super::view`] 的 `grid` 里 —— 几列、间距、滚动，与一张卡长什么样无关。
//!
//! 两桩手势各管各的：tap 派发 [`ShelfEvent::Open`] 把书交出去，长按（移动端）或
//! 右键（桌面端）开 [`super::book_menu`] 摆的那条浮层。菜单里没有「打开」，
//! 开书只有 tap 这一处入口；而长按一旦被菜单认领，这一次触摸就不会再派发成 tap。
//!
//! 封面走 gpui 的 `img` 元素，纹理上传由它负责（正文里的 `<img>` 用的是同一条路径，
//! 见 `reader_view`）；解码只在这一处调用 [`crate::ui::shared::images::cover_image`]。
//!
//! 没封面的书**必须**照样出现在架上并占住一格，否则「导入了一本但看不见」和
//! 「根本没导进来」在真机上没法分开 —— 所以那一格画的是书名，不是空白。

use std::sync::Arc;

use folio_core::store::ShelfItem;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Image, InteractiveElement, IntoElement, ObjectFit, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, StyledImage, div, img, px, relative,
};

use crate::ui::shared::images::cover_image;

use super::book_menu;
use super::view::{ShelfEvent, ShelfView};

/// 封面格的宽高比（宽 ÷ 高），出版物的通行比例。
///
/// 行高由它定而不是由图片自己定：图片的真实比例五花八门（方图、长条都有），
/// 让每格按图算高会得到一片参差的边。统一裁成这一档，多出来的部分由
/// `ObjectFit::Cover` 裁掉。
///
/// `pub(super)` 是为 [`super::book_info`] 左栏那张：架上缩略图与点开后的封面是同一张图，
/// 两处各写一个比例迟早会各裁各的。
pub(super) const COVER_RATIO: f32 = 2.0 / 3.0;

/// 多个作者并成一行用的分隔（逻辑上是一个字符的空格 + `·` + 空格）。
///
/// 一张卡下面那一行、以及「信息」面板里那一格，用的都是这一条：读者在架上看见
/// 「甲 · 乙」，点开改完仍该是两个人，而不是一个名叫「甲 · 乙」的人
/// （拆回去见 [`super::book_info::split_authors`]，库里存的仍是换行拼接）。
pub(super) const AUTHOR_SEP: &str = " · ";

/// 一张卡片：库行摊平到界面直接用的样子 —— 封面已解码，作者已并成一行。
pub(super) struct Card {
    pub(super) id: i64,
    pub(super) title: SharedString,
    pub(super) authors: SharedString,
    pub(super) cover: Option<Arc<Image>>,
    /// 已读比例（0.0–1.0）；`None` = 没有可显示的进度（从没打开过，或字数还没算过）。
    /// 库里算它不打开书、不排版，见 [`folio_core::store::ShelfItem::progress`]。
    pub(super) progress: Option<f32>,
}

impl ShelfView {
    /// 一条库行 → 一张卡。封面没解过才解，解不出的那本记住 `None`。
    ///
    /// 解码结果记在架面那份 [`ShelfView::covers`] 里：改一个搜索词会把同一本书再查
    /// 出来一遍，没缓存的话每敲一个字都要把整个架面的 JPEG 重解一遍；`None` 也要存 ——
    /// 不记住这次失败，每查一次都会再试一遍同样的失败。
    pub(super) fn card(&mut self, item: ShelfItem) -> Card {
        let cover = match self.covers.get(&item.id) {
            Some(cached) => cached.clone(),
            None => {
                let decoded = item.cover.as_deref().and_then(cover_image);
                self.covers.insert(item.id, decoded.clone());
                decoded
            }
        };
        Card {
            id: item.id,
            title: item.title.into(),
            authors: item.authors.join(AUTHOR_SEP).into(),
            cover,
            progress: item.progress,
        }
    }

    /// 一张卡 → 一格元素。
    ///
    /// `labels` 是管理菜单那两条的文案：整个架面取一次再逐卡 clone 进来（浮层的
    /// builder 是 `Fn`，菜单每次打开都重建一遍），条目本身在 [`super::book_menu`]。
    pub(super) fn card_element(
        &self,
        card: &Card,
        labels: &book_menu::Labels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let book_id = card.id;
        let title = card.title.clone();
        let authors = card.authors.clone();
        let cover = card.cover.clone();
        let has_cover = cover.is_some();
        let progress = card.progress;
        let placeholder_bg = cx.theme().muted;
        let muted_fg = cx.theme().muted_foreground;
        let accent_fg = cx.theme().primary;
        let tapped = cx.entity().clone();
        let menu_entity = cx.entity().clone();
        // 菜单每次打开都重建一遍，闭包要一份自己的文案拷贝。
        let labels = labels.clone();
        div()
            // 元素 id 用书库主键而不是标题：重新查库它不变，而标题会随导入更新，
            // 拿标题当键会让一张卡片每次改名都丢掉自己的按下态。
            .id(format!("shelf-card-{book_id}"))
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .on_click(move |_, _, cx| tapped.update(cx, |_, cx| cx.emit(ShelfEvent::Open(book_id))))
            .child(
                div()
                    .w_full()
                    .aspect_ratio(COVER_RATIO)
                    .rounded_md()
                    .overflow_hidden()
                    .bg(placeholder_bg)
                    .when_some(cover, |this, image| {
                        this.child(img(image).size_full().object_fit(ObjectFit::Cover))
                    })
                    // 没封面的那格把书名当封面画：一整格空白看起来像这本坏了，
                    // 而它只是没有图。
                    .when(!has_cover, |this| {
                        this.child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .px_2()
                                .text_center()
                                .text_color(muted_fg)
                                .child(title.clone()),
                        )
                    }),
            )
            .child(
                div()
                    .w_full()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(title),
            )
            .child(
                div()
                    .w_full()
                    .text_xs()
                    .text_color(muted_fg)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(authors),
            )
            // 已读进度：一条细槽 + 一段填充，比例由库里的字数算出来（不打开书）。
            //
            // `None` 时**整条不画**，而不是画一条空的：空槽与「读了一点点」
            // 在这么细的一条上分不出来，而把「从没打开过」画成「0%」会让读者
            // 以为这本书他开过。宁可少一行。
            .when_some(progress, |this, ratio| {
                this.child(
                    div()
                        .w_full()
                        .h(px(3.0))
                        .rounded_full()
                        .bg(placeholder_bg)
                        .overflow_hidden()
                        .child(
                            div()
                                .h_full()
                                .rounded_full()
                                .bg(accent_fg)
                                // 宽度按比例给：`relative` 让 1.0 铺满整条槽。
                                .w(relative(ratio.clamp(0.0, 1.0)))
                                // 极小的一段也要看得见：0.3% 宽在 3px 高的槽里
                                // 等于没有，而那本书确实已经开了头。
                                .min_w(px(3.0)),
                        ),
                )
            })
            // 长按（移动端）或右键（桌面端）开管理菜单，条目见 [`super::book_menu`]。
            // 与右上角那颗三点按钮的导入菜单是同一套组件，只是触发从点击换成了长按，
            // 两档浮层同宽（[`book_menu::MIN_WIDTH`]）。
            .context_menu(move |menu, _, _| book_menu::build(menu, &labels, book_id, &menu_entity))
            .into_any_element()
    }
}
