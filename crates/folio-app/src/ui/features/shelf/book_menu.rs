//! 书架卡片上的管理菜单：长按（移动端）或右键（桌面端）弹出的那几条。
//!
//! 菜单单独成文件，是为了让「有哪几条、各带什么图标、什么顺序、点了派给谁」只有一处
//! 可改：[`build`] 摆条目，[`Labels`] 管文案。动作本体仍留在 [`ShelfView`] 的方法里
//! （[`ShelfView::show_info`] 把「信息」那一页压上导航栈、[`ShelfView::remove`] 删那一行、
//! [`ShelfView::toggle_group`] 与 [`ShelfView::new_group`] 管这本书在各组里的名分），
//! 那些改的是架面自己的状态（查库、压页、删行、重查），不属于这一屏浮层。
//!
//! 「打开」不在菜单里：点卡片本身就是打开（[`super::view::ShelfEvent::Open`] 由卡片的
//! tap 派发），菜单只管信息、分组与删除那些次要的事。
//!
//! **分组那一节是开关，不是单向的「加入」**：每一条组一行，这本书已经在其中的打勾，
//! 点一下切出、再点一下切进。一本书可以同时在几个组里，所以没有另配「移出」那类条目 ——
//! 一个开关比两条互斥的条目少一处能写歪。「新建分组…」紧跟在节尾：读者的组多半是
//! 用到这一刻才想起来要建的，让他不必先离开这张卡。
//!
//! 两端共用这一份。框架里 `ContextMenuExt` 是这么设计的：手指没有右键，长按走的
//! 就是右键那条路；而长按一旦被菜单认领，这一次触摸就不会再派发成 tap，不会出现
//! 「长按想删书，顺手把书打开了」。浮层在 Android 上点得住（`plans/facts.md`，
//! 2026-10-11 读者更正过旧的记录）。

use gpui_kit::assets::IconName;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{App, Entity, SharedString, px};

use crate::ui::services::l10n::tr;

use super::view::ShelfView;

/// 架面两处浮层共用的最小宽度（逻辑像素）：卡片的管理菜单与右上角的导入菜单同宽，
/// 短标签的那条不至于比长标签的瘦一圈。
pub(super) const MIN_WIDTH: f32 = 140.0;

/// 这本书在某个分组里的名分：`(组 id, 组名, 在不在里面)`。
pub(super) type GroupRow = (i64, SharedString, bool);

/// 各条的文案。整个架面取一次（[`Labels::resolve`]），逐卡 clone 进 [`build`]。
#[derive(Clone)]
pub(super) struct Labels {
    info: SharedString,
    delete: SharedString,
    /// 分组那一节的节头那一行字（不可点）。
    group: SharedString,
    new_group: SharedString,
}

impl Labels {
    /// 按当前语言查一遍这几条。
    ///
    /// 「信息」复用那一页的标题 `shelf.info_title`：菜单点开的就是那页，一处写
    /// 「书籍信息」一处写「信息」会让读者以为是两件事。
    pub(super) fn resolve(cx: &App) -> Self {
        Self {
            info: tr(cx, "shelf.info_title"),
            delete: tr(cx, "shelf.book_delete"),
            group: tr(cx, "shelf.group"),
            new_group: tr(cx, "shelf.group_new"),
        }
    }
}

/// 把条目摆进浮层：信息在最前，分组那一节跟在后面，删除垫底并用分隔线隔开 ——
/// 破坏性的动作不排在顺手的位置。
///
/// 标签一律图标 + 短词，不插书名：弹出菜单的那张卡就是目标，读者要核对某一本书的
/// 事实（格式、大小、文件位置）走「信息」那一条。
///
/// `shelf` 是架面自己的实体，各条的回调只往它派命令。`context_menu` 的 builder 是
/// `Fn`（菜单每次打开都重建一遍），所以文案在这里按条 clone，句柄也是现摘一份。
///
/// `groups` 由调用方在**菜单打开那一刻**现查（[`ShelfView::group_items`]），所以读者刚
/// 建完的组立刻就在这一节里，不必等架面重查；一个组都没有时整节不出现（一节空条目
/// 比没有这一节更让人以为功能坏了）。
pub(super) fn build(
    menu: PopupMenu,
    labels: &Labels,
    book_id: i64,
    shelf: &Entity<ShelfView>,
    groups: Vec<GroupRow>,
) -> PopupMenu {
    let info = shelf.clone();
    let create = shelf.clone();
    let delete = shelf.clone();
    // 先摆信息与分组那一节，最后才垫上删除：`fold` 是因为组的条数是运行时才知道的。
    let menu = menu
        .min_w(px(MIN_WIDTH))
        .item(
            PopupMenuItem::new(labels.info.clone())
                .icon(IconName::Info)
                // `window` 是给那一页里那两格输入框建状态用的：`InputState::new` 要它。
                .on_click(move |_, window, cx| {
                    info.update(cx, |this, cx| this.show_info(book_id, window, cx));
                }),
        )
        .when(!groups.is_empty(), |menu| {
            menu.item(PopupMenuItem::label(labels.group.clone()))
        });
    let menu = groups.into_iter().fold(menu, |menu, (id, name, inside)| {
        let toggle = shelf.clone();
        menu.item(
            // 打勾 = 这本书已经在这个组里。点下去是切进 / 切出，所以不配图标：
            // 勾本身就是那一行的状态读数，再加一个图标会与之重复。
            PopupMenuItem::new(name)
                .checked(inside)
                .on_click(move |_, _, cx| {
                    toggle.update(cx, |this, cx| this.toggle_group(book_id, id, cx));
                }),
        )
    });
    menu.item(
        PopupMenuItem::new(labels.new_group.clone())
            .icon(IconName::FolderPlus)
            .on_click(move |_, window, cx| {
                create.update(cx, |this, cx| this.new_group(book_id, window, cx));
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new(labels.delete.clone())
            .icon(IconName::Trash)
            .on_click(move |_, _, cx| {
                delete.update(cx, |this, cx| this.remove(book_id, cx));
            }),
    )
}
