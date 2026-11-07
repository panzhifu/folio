//! 书架进入多选态时顶替底部导航的那条工具栏。
//!
//! 形状与 [`super::bottom_bar`] 同一个胶囊、同一套留白，只是那几格目的地换成这一批书的
//! 动作：退出、全选 / 全不选、加入分组、信息（只选一本时）、上传到网盘、删除。计数摆在
//! 正中间，它是这一屏唯一要读的读数。
//!
//! 这里**只画按钮**：动作本体在 [`crate::ui::features::shelf::ShelfView`] —— 它知道
//! 哪些书被勾中、书库在哪儿、删完要不要重查。外壳把两边接起来，与打开一本书走同一条路。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Sizable as _};
use gpui_kit::{
    Anchor, App, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, Window, div,
    px,
};

use super::surface::floating;
use super::{BAR_HEIGHT, NAV_MARGIN};
use crate::ui::services::l10n::{tf, tr};

/// 多选工具栏能做的一件事。分组那一档带组 id：菜单里每一条组各是一个动作。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectionAction {
    Exit,
    ToggleAll,
    Group(i64),
    Info,
    Upload,
    Delete,
}

/// 画这条工具栏。`groups` 是当前库里的组；为空时「加入分组」按不动（读者先去单本菜单
/// 里建组，比在这里摆一条空的无处可去更好）。**按值收**而不是切片：那条菜单闭包要把
/// 清单 own 进 `'static` 闭包里，切片会把这一格函数的生命周期一并绑到返回的元素上，
/// 调用方手上那份组清单就得活得比这一帧长。
///
/// `all_selected` 决定第二格是「全选」还是「全不选」；`single` 只在恰好勾了一本时给出，
/// 其余时候「信息」那一格不摆 —— 一页信息页一次只讲一本。
pub fn selection_bar<F>(
    cx: &mut App,
    count: usize,
    all_selected: bool,
    single: bool,
    groups: Vec<(i64, SharedString)>,
    bottom: f32,
    on_action: F,
) -> impl IntoElement
where
    F: Fn(SelectionAction, &mut Window, &mut App) + Clone + 'static,
{
    let exit_label = tr(cx, "shelf.selection_exit");
    let toggle_label = if all_selected {
        tr(cx, "shelf.select_none")
    } else {
        tr(cx, "shelf.select_all")
    };
    let group_label = tr(cx, "shelf.selection_group");
    let info_label = tr(cx, "shelf.info_title");
    let upload_label = tr(cx, "shelf.selection_upload");
    let delete_label = tr(cx, "shelf.book_delete");
    let count_text = tf(cx, "shelf.selection_count", &[("n", &count)]);

    let exit = on_action.clone();
    let toggle = on_action.clone();
    let info = on_action.clone();
    let upload = on_action.clone();
    let delete = on_action.clone();
    let group_rows = groups;
    let group_on = on_action.clone();

    let mut bar = floating(cx)
        .id("folio-selection-bar")
        .absolute()
        .left(px(NAV_MARGIN))
        .right(px(NAV_MARGIN))
        .bottom(px(bottom))
        .h(px(BAR_HEIGHT))
        .flex()
        .items_center()
        .gap_1()
        .px(px(4.0))
        .occlude()
        .child(
            Button::new("shelf-selection-exit")
                .ghost()
                .small()
                .icon(IconName::X)
                .tooltip(exit_label.clone().to_string())
                .accessibility_label(exit_label)
                .on_click(move |_, window, cx| exit(SelectionAction::Exit, window, cx)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_center()
                .text_sm()
                .text_ellipsis()
                .child(count_text),
        )
        .child(
            Button::new("shelf-selection-all")
                .ghost()
                .small()
                .icon(if all_selected {
                    IconName::SquareCheck
                } else {
                    IconName::ListChecks
                })
                .tooltip(toggle_label.clone().to_string())
                .accessibility_label(toggle_label)
                .on_click(move |_, window, cx| toggle(SelectionAction::ToggleAll, window, cx)),
        )
        .child(
            Button::new("shelf-selection-group")
                .ghost()
                .small()
                .icon(IconName::FolderPlus)
                .tooltip(group_label.clone().to_string())
                .accessibility_label(group_label)
                .disabled(group_rows.is_empty())
                .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                    let rows = group_rows.clone();
                    let on = group_on.clone();
                    // 菜单每次打开都重建：组清单在闭包里现拷一份，不能把上一次的搬走。
                    rows.into_iter().fold(menu, |menu, (id, name)| {
                        let on = on.clone();
                        menu.item(PopupMenuItem::new(name).on_click(move |_, window, cx| {
                            on(SelectionAction::Group(id), window, cx)
                        }))
                    })
                }),
        );

    if single {
        bar = bar.child(
            Button::new("shelf-selection-info")
                .ghost()
                .small()
                .icon(IconName::Info)
                .tooltip(info_label.clone().to_string())
                .accessibility_label(info_label)
                .on_click(move |_, window, cx| info(SelectionAction::Info, window, cx)),
        );
    }

    bar.child(
        Button::new("shelf-selection-upload")
            .ghost()
            .small()
            .icon(IconName::CloudUpload)
            .tooltip(upload_label.clone().to_string())
            .accessibility_label(upload_label)
            .on_click(move |_, window, cx| upload(SelectionAction::Upload, window, cx)),
    )
    .child(
        Button::new("shelf-selection-delete")
            .ghost()
            .small()
            .icon(IconName::Trash)
            .tooltip(delete_label.clone().to_string())
            .accessibility_label(delete_label)
            .on_click(move |_, window, cx| delete(SelectionAction::Delete, window, cx)),
    )
}
