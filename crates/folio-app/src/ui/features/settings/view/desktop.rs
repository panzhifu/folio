//! 设置整页：主窗口里的一屏，不是独立窗口，也不是对话框。
//!
//! 这一屏以前是一份独立的开-or-聚焦小窗口（`settings/window.rs`）。删掉它的理由：
//! `Settings` 组件内部是一个 `h_resizable`（页面导航 + 正文两栏），它要一个**确定的**盒子 ——
//! 而主窗口的内容区正是这样一个盒子，不必再开一个 OS 窗口去拿。少一个窗口也少一处
//! 「这功能那端有没有」的问题，`open` / `open_at` / `close` 和那份记窗口句柄的 `Global`
//! 跟着一起没了：这一屏和书架、统计、笔记是同一格里并排的四个目的地之一。
//!
//! 页面导航那一栏比框架默认（250px）窄，也比参照的那份（190px）窄：这一页旁边还站着应用
//! 自己的导航栏（48 或 260px），而独立窗口的旁边没有它。正文窄到 480px 以下时组件会把
//! 每项字段竖着堆起来（框架自己的窄档排版，不是坏了），窗口拉宽就回到一行一项。

use gpui_kit::component::group_box::GroupBoxVariant;
use gpui_kit::component::setting::{SelectIndex, Settings};
use gpui_kit::component::{ActiveTheme, v_flex};
use gpui_kit::{
    Context, ElementId, InteractiveElement, IntoElement, ParentElement, Render, Styled,
    Subscription, TestSupportExt, Window, px,
};

use super::{SettingsPage, pages};

/// 页面导航那一栏的宽与可调范围。
const SIDEBAR_WIDTH: f32 = 170.0;
const SIDEBAR_MIN: f32 = 140.0;
const SIDEBAR_MAX: f32 = 220.0;

/// 「我的」那一个目的地，以及阅读页「设置」那一格所指向的一屏。
pub struct SettingsView {
    /// 对「存储」那一屏读数实体的订阅。**必须是个字段**：gpui 的 `Subscription` 一掉就
    /// 退订（`impl Drop for Subscription` 里直接调 `unsubscribe`），绑成局部变量等于没订阅。
    /// 少了它，后台扫完那一趟永远不会让这一屏重画。
    _storage_watch: Subscription,
    /// 对「检查更新」状态实体的订阅，理由与上面那条一模一样：一次 GitHub 往返在**后台线程**
    /// 上结束，那一格要从「正在检查…」变成结果，只能靠实体 `notify` 唤醒这一屏。
    /// 少了这条订阅，症状是按钮按下去就永远停在「正在检查…」，而日志里那次检查是成功的。
    _update_watch: Subscription,
    /// 对「反馈日志」状态实体的订阅。同上一条：读盘与拼快照在后台线程上跑，
    /// 那一格要从「正在整理日志…」变成结果，只能靠实体 notify 唤醒这一屏。
    _feedback_watch: Subscription,
    /// 要定位到的那一页。**只在第一次渲染时取用** —— 组件的选择状态挂在
    /// `window.use_keyed_state(id)` 上，之后再传 `default_selected_index` 也没用，
    /// 所以取走之后就置 `None`，屏内换页由组件自己管。
    initial: Option<usize>,
}

impl SettingsView {
    /// `cx` 只为两条订阅：「存储」那一页的每一格现读读数实体，「关于」那一格现读更新状态 ——
    /// 静态设置页拿不住 `Task`，后台跑完的那一趟要靠实体 `notify` 唤醒这一屏。理由与
    /// [`CompactSettings::new`] 同一份，两边都不等外壳先装句柄：`entity` 没有就建。
    pub fn new(page: SettingsPage, cx: &mut Context<Self>) -> Self {
        let storage = crate::ui::services::storage::entity(cx);
        let watch = cx.observe(&storage, |_, _, cx| cx.notify());
        let update = crate::ui::services::update::entity(cx);
        let update_watch = cx.observe(&update, |_, _, cx| cx.notify());
        let feedback = crate::ui::services::feedback::entity(cx);
        let feedback_watch = cx.observe(&feedback, |_, _, cx| cx.notify());
        Self {
            _storage_watch: watch,
            _update_watch: update_watch,
            _feedback_watch: feedback_watch,
            initial: Some(page.index()),
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 组件的键按实例一份：同一扇窗口里可以同时站着两屏（「我的」那一格的根，
        // 和从阅读页压进来的那一屏），共用键就是共用选择状态与共用那条可拖拽的分隔线 ——
        // 于是第二屏会无视它自己带来的那一页，停在第一屏停过的地方。
        let settings = Settings::new(ElementId::from(("folio-settings", cx.entity().entity_id())))
            .with_group_variant(GroupBoxVariant::Fill)
            .sidebar_width(px(SIDEBAR_WIDTH))
            .sidebar_size_range(px(SIDEBAR_MIN)..px(SIDEBAR_MAX))
            .pages(pages(cx));
        // `take` 一次：第二帧起这一页的选择就交给组件里的 keyed state。
        let settings = match self.initial.take() {
            Some(page_ix) => settings.default_selected_index(SelectIndex {
                page_ix,
                group_ix: None,
            }),
            None => settings,
        };
        v_flex()
            .id("folio-settings")
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // 宿主测试按这一格确认「我的」那一个目的地画的确实是设置整页。
            .test_support()
            .child(settings)
    }
}

#[cfg(test)]
mod tests {
    //! 这一屏能钉住的是**「有一份账 ⇒ 那一个大读数真画得出来」**，含一条反向对照。
    //!
    //! 它钉不到「实体 notify 把这一屏叫醒」那一步：宿主测试里 `render_frame` 本来就会把视图
    //! 树重跑一遍，所以把观察者回调改成空函数，这条照样绿（试过）。那条链只能由源头保证 ——
    //! gpui 的 `Subscription::drop` 直接调 `unsubscribe`（`gpui-pre/src/subscription.rs`），
    //! 所以 [`SettingsView::_storage_watch`] 与 [`super::compact::CompactSettings`] 那份都必须是
    //! **字段**而不是局部变量；外壳里那五份 `_config_watch`/`_shelf_watch` 同一形状。

    use super::{SettingsPage, SettingsView};
    use crate::ui::services::config::ConfigHolder;
    use crate::ui::services::storage::{self, Snapshot};
    use folio_core::Config;
    use folio_core::infra::usage::{Area, Entry, Usage};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, TestAppContext, px, size};

    /// 一份只填了两类东西的账。书库位置没确认过（`Config::default()` 那一档），
    /// 所以这里不依赖真磁盘 —— 这一条门问的是「有没有被叫醒」，不是「量得准不准」。
    fn ledger() -> Snapshot {
        let mut usage = Usage {
            entries: Area::ALL
                .iter()
                .map(|area| Entry {
                    area: *area,
                    bytes: 0,
                    files: 0,
                })
                .collect(),
            total: 0,
            unreadable: 0,
        };
        for (area, bytes) in [(Area::Database, 4096u64), (Area::Logs, 1024)] {
            let entry = usage
                .entries
                .iter_mut()
                .find(|entry| entry.area == area)
                .unwrap();
            entry.bytes = bytes;
            entry.files = 1;
        }
        usage.total = 5120;
        Snapshot {
            usage,
            freeable: Some(0),
        }
    }

    #[gpui_kit::test]
    async fn the_storage_page_draws_the_readout_once_a_ledger_lands(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        // 这一屏的实体由 `Root` 留住，测试不需要另一份句柄：要问的是**画出来没有**。
        let handle = cx
            .open_window(size(px(900.), px(700.)), |window, cx| {
                ConfigHolder::install(Config::default(), cx);
                // 落点直接给「存储」那一页：组件第一次渲染就选中它，那些自定义行才会进树。
                let settings = cx.new(|cx| SettingsView::new(SettingsPage::Storage, cx));
                Root::new(settings, window, cx)
            })
            .into();

        let shown = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find("storage-total").is_some()
            })
            .expect("窗口应能渲染")
        };

        // 先把对照做出来：空实体 + 渲染一帧 ⇒ 没有那一个大读数。少了这一步，后面那句
        // `is_some()` 在什么都没接上的时候也可能是绿的。
        assert!(
            !shown(cx),
            "还没有账的时候那一屏不该有大读数 —— 这一句反过来就说明那个标记永远在，夹具不可信"
        );

        cx.update(|cx| {
            let store = storage::entity(cx);
            store.update(cx, |state, cx| {
                state.set_snapshot_for_test(ledger());
                cx.notify();
            });
        });
        assert!(
            shown(cx),
            "账交上去之后那一格大读数该画出来：它不在，说明这一页根本没渲染"
        );
    }
}
