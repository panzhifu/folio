//! 架面顶上那一行：搜索框、三点菜单、建组按钮。
//!
//! 三种导入方式（选文件 / URL / 整机扫描）收在三点菜单里，手机上只剩一颗搜索框 ——
//! 收与不收的判据是窗口档位，见 `columns_for` 与调用点。

use super::super::components::book_menu;
use crate::ui::services::l10n::tr;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{Selectable, Sizable as _, h_flex};
use gpui_kit::{Anchor, Context, IntoElement, ParentElement, Styled, div, px};

use super::{GUTTER, GroupFilter, ShelfView};

impl ShelfView {
    /// 桌面/平板的工具栏：钉在架面最上方，搜索框与三点按钮靠右。
    ///
    /// 不复用手机上那一行，因为两者要回答的问题不同：手机上宽度只够一条满幅的框，
    /// 框被拉满就是「先搜」；桌面上把同一颗框拉到 1300 像素宽，它就从一条命令
    /// 变成一块空白。所以这里给它一个和「书名或作者」这句话相称的定宽，
    /// 与三点按钮一起收在右上角。
    ///
    /// 左右留白与网格共用 [`GUTTER`]：两行不共用一条对齐脊线的话，
    /// 搜索框的左边会莫名其妙地比第一格封面缩进一截。
    pub(in crate::ui::features::shelf) fn toolbar(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .w_full()
            .flex_shrink_0()
            .items_center()
            .justify_end()
            .gap_2()
            .px(px(GUTTER))
            .py_2()
            .child(
                // 工具栏按 compact 档排：框和按钮同一档，否则一行里会长出两种高度。
                // 框的 small 档在 `boot` 建组件时就定下了。
                div().w_64().child(self.search.clone()),
            )
            .child(self.group_cell(cx))
            .child(self.more_button(cx))
    }

    /// 搜索框那一行：框占满剩下的宽度，三点按钮钉在右边。
    pub(in crate::ui::features::shelf) fn search_bar(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px(px(GUTTER))
            .py_2()
            .child(div().flex_1().min_w_0().child(self.search.clone()))
            .child(self.group_cell(cx))
            .child(self.more_button(cx))
    }

    /// 右上角那颗三点按钮：本体不做任何事，点开一条菜单，三种导入方式摆在菜单里。
    ///
    /// 三颗按钮并排把搜索框挤成一条缝，而导入是偶尔才用一次的命令 —— 收进一条
    /// 「⋮」后面，这一行就只剩「搜书」与「更多」两件事。组件是 gpui-kit 的
    /// [`DropdownMenu`]：按钮负责开合，浮层内容是一条 [`PopupMenu`]，与架面卡片
    /// 长按（`ContextMenuExt`）弹出的那一份同源，只是触发从右键/长按换成了点击。
    /// 浮层在 Android 上点得住（`plans/facts.md`，2026-10-11 读者更正过旧的记录），
    /// 所以两端共用这一颗。
    ///
    /// 菜单项各自把命令派回本视图的既有方法（`import` / `open_url_panel` /
    /// `start_scan`），门禁照旧：书库位置没确认过先摆确认面板，一批没导完就按不动。
    /// 菜单每次打开都会重建一遍（builder 是 `Fn`），所以闭包里先 clone 再 move：
    /// 从捕获的句柄里现摘一份，而不是把上次那一份搬走。
    pub(in crate::ui::features::shelf) fn more_button(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity().clone();
        let url_entity = entity.clone();
        let scan_entity = entity.clone();
        let import_label = tr(cx, "shelf.import");
        let scan_label = tr(cx, "shelf.scan");
        let url_label = tr(cx, "shelf.url_import");
        let more_label = tr(cx, "shelf.more");
        Button::new("shelf-more")
            .ghost()
            .small()
            .icon(IconName::EllipsisVertical)
            .tooltip(more_label.clone().to_string())
            .accessibility_label(more_label)
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let entity = entity.clone();
                let url_entity = url_entity.clone();
                let scan_entity = scan_entity.clone();
                menu.min_w(px(book_menu::MIN_WIDTH))
                    .item(
                        PopupMenuItem::new(import_label.clone())
                            .icon(IconName::Plus)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| this.import(cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new(url_label.clone())
                            .icon(IconName::Link)
                            .on_click(move |_, window, cx| {
                                url_entity.update(cx, |this, cx| this.open_url_panel(window, cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new(scan_label.clone())
                            .icon(IconName::ScanSearch)
                            .on_click(move |_, _, cx| {
                                scan_entity.update(cx, |this, cx| this.start_scan(cx))
                            }),
                    )
            })
    }

    /// 顶部那颗分组筛选：图标一颗，筛着某一组时吃选中态。
    ///
    /// 手机上那一行只剩「满幅框 + 两颗图标」的宽度，所以这里不带文字；点开那一条菜单里
    /// 写着筛的是哪一组，组名后面缀着书数（「想读 · 12」），读者不必点进去才知道有几本。
    pub(in crate::ui::features::shelf) fn group_button(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity().clone();
        let label = tr(cx, "shelf.group");
        let all_label = tr(cx, "shelf.group_all");
        let ungrouped_label = tr(cx, "shelf.ungrouped");
        let collections = self.collections.clone();
        let current = self.group;
        Button::new("shelf-group-filter")
            .ghost()
            .small()
            .icon(IconName::FolderTree)
            .selected(current != GroupFilter::All)
            .tooltip(label.clone().to_string())
            .accessibility_label(label)
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let menu = menu
                    .min_w(px(book_menu::MIN_WIDTH))
                    .item(
                        PopupMenuItem::new(all_label.clone())
                            .checked(current == GroupFilter::All)
                            .on_click({
                                let entity = entity.clone();
                                move |_, _, cx| {
                                    entity.update(cx, |this, cx| {
                                        this.set_group(GroupFilter::All, cx)
                                    });
                                }
                            }),
                    )
                    .item(
                        PopupMenuItem::new(ungrouped_label.clone())
                            .checked(current == GroupFilter::Ungrouped)
                            .on_click({
                                let entity = entity.clone();
                                move |_, _, cx| {
                                    entity.update(cx, |this, cx| {
                                        this.set_group(GroupFilter::Ungrouped, cx)
                                    });
                                }
                            }),
                    );
                // 一个组都还没建时这一节整块不出现：条目只有「全部分组 / 未分组」两条的
                // 菜单，读起来像坏了。
                if collections.is_empty() {
                    return menu;
                }
                let menu = menu.separator();
                // 菜单每开一遍重建一次（builder 是 `Fn`），所以这一份清单每次现摘一份。
                collections.clone().into_iter().fold(menu, |menu, c| {
                    let entity = entity.clone();
                    let name = format!("{} · {}", c.name, c.book_count);
                    menu.item(
                        PopupMenuItem::new(name)
                            .checked(current == GroupFilter::Collection(c.id))
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| {
                                    this.set_group(GroupFilter::Collection(c.id), cx)
                                });
                            }),
                    )
                })
            })
    }
}
