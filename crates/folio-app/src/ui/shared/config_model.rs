//! 应用配置的运行时模型：把 [`Config`] 挂成一个可观察的实体，并负责落盘。
//!
//! `SettingField` 的取值对只拿到 `&App` / `&mut App`，所以配置必须挂在某个
//! `Global` 上才能被设置面板读写。但 `cx.global_mut(..)` 不会通知任何视图，
//! 而阅读器需要在改字号后立刻重排 —— 因此 Global 里存的不是配置本身，
//! 而是配置的实体句柄：视图对它 `observe`，写入方 `update` 它，两边都通。
//!
//! 还有一件事在这里做而不是在别处：界面语言对 `rust_i18n` 而言是**进程全局**的，
//! 而它的真源是 `config.language.locale`。所有写入都过 [`ConfigHolder::edit`]，
//! 所以「改完配置顺手把全局 locale 同步一次」只有一处需要负责。
//!
//! 落盘目标是另一个 Global [`ConfigPath`]，默认 `None`。测试里保持 `None`，
//! 于是 `save` 直接跳过 —— 测试绝不会写到用户的真实配置目录。

use std::path::PathBuf;

use folio_core::{Account, Appearance, Config, Language, Reader};
use gpui_kit::{App, AppContext as _, Entity, Global};

pub struct ConfigState {
    pub config: Config,
    /// 保存目标；`None` 表示不落盘。
    pub path: Option<PathBuf>,
}

pub struct ConfigHolder(pub Entity<ConfigState>);

impl Global for ConfigHolder {}

/// 配置文件的保存目标。由 `main` 在开窗前设置；测试不设 → 不落盘。
#[derive(Default)]
pub struct ConfigPath(pub Option<PathBuf>);

impl Global for ConfigPath {}

impl ConfigHolder {
    pub fn install(config: Config, cx: &mut App) -> Entity<ConfigState> {
        let path = cx.try_global::<ConfigPath>().and_then(|p| p.0.clone());
        let config = config.normalized();
        // 启动时同步一次：框架组件（下拉框、搜索框）读的是同一个全局 locale。
        config.language.locale.apply();
        let entity = cx.new(|_| ConfigState { config, path });
        cx.set_global(ConfigHolder(entity.clone()));
        entity
    }

    /// 配置实体的句柄。视图对它 `observe` 才能在改偏好后被唤醒；
    /// 桌面外壳与移动端外壳都各留一份订阅，drop 掉就等于取消订阅。
    pub fn handle(cx: &App) -> Entity<ConfigState> {
        Self::entity(cx)
    }

    fn entity(cx: &App) -> Entity<ConfigState> {
        cx.global::<ConfigHolder>().0.clone()
    }

    pub fn reader(cx: &App) -> Reader {
        Self::entity(cx).read(cx).config.reader.clone()
    }

    pub fn appearance(cx: &App) -> Appearance {
        Self::entity(cx).read(cx).config.appearance.clone()
    }

    pub fn language(cx: &App) -> Language {
        Self::entity(cx).read(cx).config.language.clone()
    }

    pub fn account(cx: &App) -> Account {
        Self::entity(cx).read(cx).config.account.clone()
    }

    /// 唯一的写入口：写入即规整，并同步全局 locale。
    ///
    /// 规整是因为面板里填进 9000 号字会把排版算成零高或无限宽，而认不出的浅深选择
    /// 该落回「跟随系统」；语言那一步是因为 `rust_i18n` 只认全局值，不认我们这份配置。
    pub fn edit(cx: &mut App, f: impl FnOnce(&mut Config)) {
        Self::entity(cx).update(cx, |state, _| {
            f(&mut state.config);
            state.config = state.config.clone().normalized();
            state.config.language.locale.apply();
        });
    }

    pub fn edit_reader(cx: &mut App, f: impl FnOnce(&mut Reader)) {
        Self::edit(cx, |config| f(&mut config.reader));
    }

    pub fn edit_appearance(cx: &mut App, f: impl FnOnce(&mut Appearance)) {
        Self::edit(cx, |config| f(&mut config.appearance));
    }

    pub fn edit_language(cx: &mut App, f: impl FnOnce(&mut Language)) {
        Self::edit(cx, |config| f(&mut config.language));
    }

    pub fn edit_account(cx: &mut App, f: impl FnOnce(&mut Account)) {
        Self::edit(cx, |config| f(&mut config.account));
    }

    /// 落盘。`path` 为 `None`（测试）时跳过；写失败只打日志，不让 UI 崩。
    pub fn save(cx: &App) {
        let (config, path) = {
            let state = Self::entity(cx).read(cx);
            (state.config.clone(), state.path.clone())
        };
        let Some(path) = path else {
            return;
        };
        if let Err(e) = config.save_to(&path) {
            eprintln!("保存配置失败：{e}");
        }
    }
}
