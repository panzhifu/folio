//! 阅读偏好的运行时模型。
//!
//! `SettingField` 的取值对只拿到 `&App` / `&mut App`，所以偏好必须挂在某个
//! `Global` 上才能被设置面板读写。但 `cx.global_mut(..)` 不会通知任何视图，
//! 而阅读器需要在改字号后立刻重排 —— 因此 Global 里存的不是偏好本身，
//! 而是偏好的实体句柄：视图对它 `observe`，写入方 `update` 它，两边都通。

use folio_core::Prefs;
use gpui_kit::{App, AppContext as _, Entity, Global};

pub struct PrefsState {
    pub prefs: Prefs,
}

pub struct PrefsHolder(pub Entity<PrefsState>);

impl Global for PrefsHolder {}

impl PrefsHolder {
    pub fn install(prefs: Prefs, cx: &mut App) -> Entity<PrefsState> {
        let entity = cx.new(|_| PrefsState {
            prefs: prefs.clamped(),
        });
        cx.set_global(PrefsHolder(entity.clone()));
        entity
    }

    fn entity(cx: &App) -> Entity<PrefsState> {
        cx.global::<PrefsHolder>().0.clone()
    }

    pub fn get(cx: &App) -> Prefs {
        Self::entity(cx).read(cx).prefs.clone()
    }

    /// 写入即夹取，避免面板里填进 9000 号字把排版算成零高或无限宽。
    pub fn edit(cx: &mut App, f: impl FnOnce(&mut Prefs)) {
        Self::entity(cx).update(cx, |state, _| {
            f(&mut state.prefs);
            state.prefs = state.prefs.clone().clamped();
        });
    }
}
