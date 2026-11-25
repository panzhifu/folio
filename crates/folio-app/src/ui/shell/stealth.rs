//! 低干扰阅读的动作。动作与键位分离：前者是语义，后者可在平台与设置里换。

use gpui_kit::actions;
use gpui_kit::{App, Context, KeyBinding, Subscription, Window};

use super::AppShell;
use crate::ui::services::stealth as stealth_service;

actions!(folio_shell, [ToggleStealth, BossHide, RestoreFromHide,]);

/// 最小化后从任务栏或系统手势恢复不会派发应用内动作，用窗口激活补上恢复路径。
pub(super) fn observe_activation(window: &mut Window, cx: &mut Context<AppShell>) -> Subscription {
    cx.observe_window_activation(window, |_, window, cx| {
        if window.is_window_active() {
            stealth_service::restore_from_activation(cx);
        }
    })
}

/// 应用内快捷键。跨应用全局热键当前不可证实，不假装绑定成功。
pub(super) fn bind_keys(cx: &mut Context<AppShell>) {
    cx.bind_keys([
        KeyBinding::new("cmd-alt-s", ToggleStealth, None),
        KeyBinding::new("ctrl-alt-s", ToggleStealth, None),
        KeyBinding::new("cmd-alt-h", BossHide, None),
        KeyBinding::new("ctrl-alt-h", BossHide, None),
    ]);
}

/// 动作由外壳兜底：键盘焦点落在哪里都能切。
pub(super) fn register_actions(cx: &mut Context<AppShell>) {
    let weak = cx.entity().downgrade();
    App::on_action(&mut *cx, move |_: &ToggleStealth, cx: &mut App| {
        if let Some(shell) = weak.upgrade() {
            shell.update(cx, |this, cx| this.toggle_stealth(cx));
        }
    });
    let weak = cx.entity().downgrade();
    App::on_action(&mut *cx, move |_: &BossHide, cx: &mut App| {
        if let Some(shell) = weak.upgrade() {
            shell.update(cx, |this, cx| this.boss_hide(cx));
        }
    });
    let weak = cx.entity().downgrade();
    App::on_action(&mut *cx, move |_: &RestoreFromHide, cx: &mut App| {
        if let Some(shell) = weak.upgrade() {
            shell.update(cx, |this, cx| this.restore_from_hide(cx));
        }
    });
}

impl AppShell {
    /// 低干扰阅读：事件交给服务，隐藏前先结掉这一段的阅读位置。
    fn stealth_dispatch(&mut self, event: stealth_service::StealthEvent, cx: &mut Context<Self>) {
        if cx.try_global::<stealth_service::StealthService>().is_none() {
            return;
        }
        let boss_hide = matches!(event, stealth_service::StealthEvent::BossHide);
        let entering_quiet = boss_hide
            || matches!(event, stealth_service::StealthEvent::Toggle)
                && !stealth_service::is_quiet(cx);
        if boss_hide {
            if let Some(layer) = self.reading.as_ref() {
                layer.page.update(cx, |page, cx| page.flush_position(cx));
            }
        }
        if entering_quiet {
            if let Some(layer) = self.reading.as_ref() {
                layer.page.update(cx, |page, cx| {
                    // 浮层一层层收：图片查看器、脚注、目录、设置、顶栏底栏各有自己的
                    // 收场顺序，这里只重复走公开的 `dismiss_chrome`，不另排一套。
                    for _ in 0..12 {
                        if !page.dismiss_chrome(cx) {
                            break;
                        }
                    }
                });
            }
        }
        stealth_service::dispatch_with_window(event, cx);
    }

    fn toggle_stealth(&mut self, cx: &mut Context<Self>) {
        self.stealth_dispatch(stealth_service::StealthEvent::Toggle, cx);
    }

    fn boss_hide(&mut self, cx: &mut Context<Self>) {
        self.stealth_dispatch(stealth_service::StealthEvent::BossHide, cx);
    }

    fn restore_from_hide(&mut self, cx: &mut Context<Self>) {
        self.stealth_dispatch(stealth_service::StealthEvent::Restore, cx);
    }
}
