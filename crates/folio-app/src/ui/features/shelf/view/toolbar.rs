//! 架面顶上那一行：搜索框、三点菜单。
//!
//! 导入的四条来路（选文件 / 链接 / 整机扫描 / OPDS 书源）与「看导入记录」收在三点菜单里，
//! 手机上只剩一颗搜索框 —— 收与不收的判据是窗口档位，见 `columns_for` 与调用点。

use super::super::components::book_menu;
use crate::ui::services::l10n::tr;
use folio_core::Tag;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div, px,
};

use super::{GUTTER, ShelfView};

/// 标签 → 筛选 chip 的本地化键。与标记浮层、卡片角标那两份 match 同一套键名
/// （`shelf.tag_*`），三处各一份薄的 match，改档位名时编译器穷尽检查一起盯。
fn tag_filter_label(tag: Tag) -> &'static str {
    match tag {
        Tag::Unread => "shelf.tag_unread",
        Tag::Reading => "shelf.tag_reading",
        Tag::Finished => "shelf.tag_finished",
    }
}

impl ShelfView {
    /// 标签筛选那一排：三颗状态 chip（未读 / 在读 / 读完），常驻整架那一屏。
    ///
    /// 筛的是**有效状态**：手动挂过标签的书以手动为准（`Tag::effective`），没挂的
    /// 按阅读数据自动判（`ShelfItem::auto_status` —— 没翻开是未读，进度到线是读完，
    /// 其余在读）。挂在顶栏折叠容器之外（见 render 那一段为什么不进那个定高裁切的
    /// 容器），横向留白与搜索行共用 [`GUTTER`]。点一下筛上那一个（实心档），再点一下
    /// 取消；一次只筛一颗。
    ///
    /// 无筛选时三颗都是描边档（ghost）：没有另配「全部」那颗，一颗都不实心就是不筛，
    /// 与扫描清单那一排 chips 的口径一致。
    pub(in crate::ui::features::shelf) fn tag_filter_bar(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity().clone();
        // 三颗 chip **就地搭好**：搭一颗要读一次文案（`cx` 的账），闭包里借不动它 ——
        // 与卡片角标、标记浮层那两处同一个做法。
        let chips: Vec<_> = Tag::ALL
            .iter()
            .map(|tag| {
                let selected = self.tag_filter == Some(*tag);
                let chip_entity = entity.clone();
                let tag = *tag;
                let key = format!("shelf-filter-tag-{}", tag.as_str());
                let label = tr(cx, tag_filter_label(tag)).to_string();
                Button::new(SharedString::from(key))
                    .when(selected, |button| button.secondary())
                    .when(!selected, |button| button.ghost())
                    .small()
                    .label(label.clone())
                    .accessibility_label(label)
                    .on_click(move |_, _, cx| {
                        chip_entity.update(cx, |this, cx| this.toggle_tag_filter(tag, cx))
                    })
            })
            .collect();
        h_flex()
            .w_full()
            .flex_shrink_0()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px(px(GUTTER))
            .pb_2()
            .children(chips)
            // 返回 AnyElement 而不是 `impl IntoElement`：后者在 edition 2024 里会把
            // `cx` / `self` 的借用一起捕获进 opaque，render 那一侧随后还要用 `cx`
            // （顶栏收折、浮层那几块），借用活着就全被挡住 —— 与 `top` 那两条路
            // 当场 `.into_any_element()` 是同一句话。
            .into_any_element()
    }

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
            .child(self.more_button(cx))
    }

    /// 右上角那颗三点按钮：本体不做任何事，点开一条菜单，几种导入方式摆在菜单里。
    ///
    /// 三颗按钮并排把搜索框挤成一条缝，而导入是偶尔才用一次的命令 —— 收进一条
    /// 「⋮」后面，这一行就只剩「搜书」与「更多」两件事。组件是 gpui-kit 的
    /// [`DropdownMenu`]：按钮负责开合，浮层内容是一条 [`PopupMenu`]，与架面卡片
    /// 长按（`ContextMenuExt`）弹出的那一份同源，只是触发从右键/长按换成了点击。
    /// 浮层在 Android 上点得住（`plans/facts.md`，2026-10-11 读者更正过旧的记录），
    /// 所以两端共用这一颗。
    ///
    /// 菜单项各自把命令派回本视图的既有方法（`import` / `start_scan` /
    /// `open_opds` / `open_cloud`），门禁照旧：书库位置没确认过先摆确认面板，
    /// 一批没导完就按不动。分隔线下面那一颗是 `open_import_records`：它不导东西，只把这张
    /// 运行里已经报过的那些行翻回屏幕上（无话可说时按不动）。
    /// 菜单每次打开都会重建一遍（builder 是 `Fn`），所以闭包里先 clone 再 move：
    /// 从捕获的句柄里现摘一份，而不是把上次那一份搬走。
    pub(in crate::ui::features::shelf) fn more_button(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity().clone();
        let network_entity = entity.clone();
        let scan_entity = entity.clone();
        let opds_entity = entity.clone();
        let cloud_entity = entity.clone();
        let record_entity = entity.clone();
        let import_label = tr(cx, "shelf.import");
        let network_label = tr(cx, "shelf.network_import");
        let scan_label = tr(cx, "shelf.scan");
        let opds_label = tr(cx, "shelf.opds");
        let cloud_label = tr(cx, "shelf.cloud");
        let more_label = tr(cx, "shelf.more");
        let record_label = tr(cx, "shelf.record_title");
        // 记录那颗与上面四条不是一件事：那四条是**怎么导**，这一颗是**刚才导的那些怎么样了**。
        // 有没有话可说在这里判一次：无话可说时它按不动，而不是点开一张空卡。
        let records_wanted = self.import_records_wanted();
        Button::new("shelf-more")
            .ghost()
            .small()
            .icon(IconName::EllipsisVertical)
            .tooltip(more_label.clone().to_string())
            .accessibility_label(more_label)
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let entity = entity.clone();
                let network_entity = network_entity.clone();
                let scan_entity = scan_entity.clone();
                let opds_entity = opds_entity.clone();
                let cloud_entity = cloud_entity.clone();
                let record_entity = record_entity.clone();
                menu.min_w(px(book_menu::MIN_WIDTH))
                    .item(
                        PopupMenuItem::new(import_label.clone())
                            .icon(IconName::Plus)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| this.import(cx))
                            }),
                    )
                    // 「网络导入」紧跟本地导入：这一条开的是本机起的 Wi-Fi 传书服务，
                    // 别的设备扫码/开地址把书推进来 —— 与后面两条「从远端拉书」（OPDS、
                    // 网盘）方向相反，所以排在它们之前。
                    .item(
                        PopupMenuItem::new(network_label.clone())
                            .icon(IconName::Network)
                            .on_click(move |_, _, cx| {
                                network_entity.update(cx, |this, cx| this.open_network_import(cx))
                            }),
                    )
                    // 扫描紧跟在「导入」下面：这两条说的是同一件事——从这台机器上已有的文件
                    // 里挑一本进来；网络那两条（OPDS、网盘）排在它们之后。
                    .item(
                        PopupMenuItem::new(scan_label.clone())
                            .icon(IconName::ScanSearch)
                            .on_click(move |_, _, cx| {
                                scan_entity.update(cx, |this, cx| this.start_scan(cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new(opds_label.clone())
                            .icon(IconName::BookOpen)
                            .on_click(move |_, window, cx| {
                                opds_entity.update(cx, |this, cx| this.open_opds(window, cx))
                            }),
                    )
                    // 网盘与 OPDS 同属「书在远端」：一个是自己存的书，一个是别人的目录。
                    .item(
                        PopupMenuItem::new(cloud_label.clone())
                            .icon(IconName::Cloud)
                            .on_click(move |_, window, cx| {
                                cloud_entity.update(cx, |this, cx| this.open_cloud(window, cx))
                            }),
                    )
                    // 分隔线以下不是「怎么导」，是「刚才导的那些怎么样了」：两件事不该排在
                    // 同一口气里。没话可说时这一颗按不动 —— 点开一张空卡比按不动更糟。
                    .separator()
                    .item(
                        PopupMenuItem::new(record_label.clone())
                            .icon(IconName::ListChecks)
                            .disabled(!records_wanted)
                            .on_click(move |_, _, cx| {
                                record_entity.update(cx, |this, cx| this.open_import_records(cx))
                            }),
                    )
            })
    }
}
