//! 应用配置的运行时模型：把 [`Config`] 挂成一个可观察的实体，并负责落盘。
//!
//! `SettingField` 的取值对只拿到 `&App` / `&mut App`，所以配置必须挂在某个
//! `Global` 上才能被设置面板读写。但 `cx.global_mut(..)` 不会通知任何视图，
//! 而阅读器需要在改字号后立刻重排 —— 因此 Global 里存的不是配置本身，
//! 而是配置的实体句柄：视图对它 `observe`，写入方 `update` 它，两边都通。
//!
//! 读写的形状是**持 `Arc<Config>`，变更换整个 Arc**（`plans/基础设施.md` §2.3）：
//! `edit` 在旧值上克隆、改、规整，再整体换掉，**绝不原地改**。这样持旧 Arc 的一方
//! （比如一个正在跑的排版作业）继续看到旧值，一帧之内不会撕裂；需要在一帧里一致地
//! 读多处时，抓一次 [`ConfigHolder::snapshot`] 就行。
//!
//! 还有一件事在这里做而不是在别处：界面语言对 `rust_i18n` 而言是**进程全局**的，
//! 而它的真源是 `config.language.locale`。所有写入都过 [`ConfigHolder::edit`]，
//! 所以「改完配置顺手把全局 locale 同步一次」只有一处需要负责。
//!
//! 落盘目标是另一个 Global [`ConfigPath`]，默认 `None`。测试里保持 `None`，
//! 于是 `save` 直接跳过 —— 测试绝不会写到用户的真实配置目录。

use std::path::PathBuf;
use std::sync::Arc;

use folio_core::{
    Appearance, Config, Env, Language, Library, Provenance, Reader, Resolved, Speech, Sync,
    Translation,
};
use gpui_kit::{App, AppContext as _, Entity, Global};

use crate::ui::foundation::error::{Context as _, report};

pub struct ConfigState {
    /// **用户层**：内置默认 ⊕ 用户文件。**落盘写的是它**（§2.1 写回只写用户层）。
    pub user: Config,
    /// **生效值**：用户层 ⊕ 环境变量。视图读它。**换 Arc，不改内容**（§2.3）。
    pub config: Arc<Config>,
    /// 每一项由哪一层定下来（诊断用，§2.1）。
    pub provenance: Provenance,
    /// 读盘时那份环境变量覆盖层：编辑后要重新叠一次才能算出新的生效值。
    pub env: Env,
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
    /// 装配置（凭一份「生效值」，不带环境覆盖）—— 测试与旧调用方用：用户层 == 生效值。
    pub fn install(config: Config, cx: &mut App) -> Entity<ConfigState> {
        let config = config.normalized();
        Self::install_inner(
            Resolved {
                user: config.clone(),
                effective: config,
                provenance: Provenance::default(),
            },
            Env::default(),
            cx,
        )
    }

    /// 装一份**读过盘**的配置：保留用户层与 provenance（§2.1）。入口用它 —— 用户层是
    /// 落盘内容，provenance 是诊断内容，都得从读盘那一刻一起交进来。
    pub fn install_resolved(resolved: Resolved, cx: &mut App) -> Entity<ConfigState> {
        Self::install_inner(resolved, Env::from_env(), cx)
    }

    fn install_inner(resolved: Resolved, env: Env, cx: &mut App) -> Entity<ConfigState> {
        let path = cx.try_global::<ConfigPath>().and_then(|p| p.0.clone());
        let Resolved {
            user,
            effective,
            provenance,
        } = resolved;
        // 启动时同步一次：框架组件（下拉框、搜索框）读的是同一个全局 locale。
        effective.language.locale.apply();
        let entity = cx.new(|_| ConfigState {
            user,
            config: Arc::new(effective),
            provenance,
            env,
            path,
        });
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

    /// 当前这份配置的句柄。需要在一帧里**一致地**读多处时抓一次这个 —— 换 Arc 不会改到
    /// 手里这份（§2.3）。只要一小节就用下面那几个 `reader` / `appearance` / …。
    pub fn snapshot(cx: &App) -> Arc<Config> {
        Arc::clone(&Self::entity(cx).read(cx).config)
    }

    pub fn reader(cx: &App) -> Reader {
        Self::snapshot(cx).reader.clone()
    }

    pub fn appearance(cx: &App) -> Appearance {
        Self::snapshot(cx).appearance.clone()
    }

    pub fn language(cx: &App) -> Language {
        Self::snapshot(cx).language.clone()
    }

    /// 翻译那一节。会话开翻译弹层、换目标语言时读它。
    pub fn translation(cx: &App) -> Translation {
        Self::snapshot(cx).translation.clone()
    }

    /// 书库位置那一节。导入前要先问它确认过没有（见 `library::target_dir`）。
    pub fn library(cx: &App) -> Library {
        Self::snapshot(cx).library.clone()
    }

    /// 听书那一节。会话每开一段念之前读一次（语速/音高随改随生效，不用另发命令）。
    pub fn speech(cx: &App) -> Speech {
        Self::snapshot(cx).speech.clone()
    }

    /// WebDAV 同步那一节。同步服务每次点击时读一次。
    pub fn sync(cx: &App) -> Sync {
        Self::snapshot(cx).sync.clone()
    }

    /// 每一项的配置来自哪一层（§2.1）。诊断 dump 用。
    pub fn provenance(cx: &App) -> Provenance {
        Self::entity(cx).read(cx).provenance.clone()
    }

    /// 唯一的写入口：改的是**用户层**（§2.1），改完重算生效值，并同步全局 locale。
    ///
    /// 规整是因为面板里填进 9000 号字会把排版算成零高或无限宽，而认不出的浅深选择
    /// 该落回「跟随系统」；语言那一步是因为 `rust_i18n` 只认全局值，不认我们这份配置。
    ///
    /// **收尾要 `cx.notify()`**：观察者（外壳那份落盘 + 重画、欢迎页门禁）只在被观察实体
    /// `notify` 时才收到回调 —— `Entity::update` 本身不会替我们通知。少了这一句，配置
    /// 改得动、读得回，却永远不落盘、界面也不重画（这一句就是为修掉那个问题而加的）。
    pub fn edit(cx: &mut App, f: impl FnOnce(&mut Config)) {
        Self::entity(cx).update(cx, |state, cx| {
            // 改**用户层**：生效值是「用户层 ⊕ 环境变量」算出来的，不能反过来改它 ——
            // 那样一次编辑就会把环境变量的值固化进落盘内容（§2.1 第 3 条要防的正是这个）。
            let mut user = state.user.clone();
            f(&mut user);
            let user = user.normalized();
            user.language.locale.apply();
            state.user = user;
            // 重算生效值，换整个 Arc（§2.3）：手上有旧 Arc 的一方不受影响。
            let mut effective = state.user.clone();
            state.env.apply(&mut effective);
            state.config = Arc::new(effective);
            cx.notify();
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

    pub fn edit_translation(cx: &mut App, f: impl FnOnce(&mut Translation)) {
        Self::edit(cx, |config| f(&mut config.translation));
    }

    pub fn edit_sync(cx: &mut App, f: impl FnOnce(&mut Sync)) {
        Self::edit(cx, |config| f(&mut config.sync));
    }

    pub fn edit_library(cx: &mut App, f: impl FnOnce(&mut Library)) {
        Self::edit(cx, |config| f(&mut config.library));
    }

    pub fn edit_speech(cx: &mut App, f: impl FnOnce(&mut Speech)) {
        Self::edit(cx, |config| f(&mut config.speech));
    }

    /// 落盘。`path` 为 `None`（测试）时跳过；写失败只打日志，不让 UI 崩。
    ///
    /// 这里是「改了但没存住」唯一会露出来的地方：屏幕上当场就生效了，重启才发现回到旧值，
    /// 所以链上必须留得住是哪一步（建目录 / 写临时文件 / rename）失败的。
    pub fn save(cx: &App) {
        // 落盘写的是**用户层**，不是生效值（§2.1 第 3 条）：把生效值整体烤回文件，等于把
        // 环境变量的值固化成读者的选择，之后默认值的升级就对他永远失效了。
        let (user, path) = {
            let state = Self::entity(cx).read(cx);
            (state.user.clone(), state.path.clone())
        };
        let Some(path) = path else {
            return;
        };
        if let Err(err) = user
            .save_to(&path)
            .with_context(|| format!("保存到 {}", path.display()))
        {
            report("配置落盘", &err);
        }
    }
}

#[cfg(test)]
mod tests {
    //! 配置实体在**已启动的外壳里**的两件事：越界偏好被夹住、偏好改完立刻读得回来。
    //!
    //! 两条都要一个真的 `App`：`ConfigHolder` 是挂在 `Global` 上的实体，不走启动流程
    //! 就造不出「视图能看到的那份配置」。`ConfigPath` 不设 → `save` 直接跳过，
    //! 所以测试绝不会写到用户真实的配置目录。
    //!
    //! 两个用例各开各的窗口（尺寸不同：夹取那条用 600×400），所以不共用脚手架。

    use super::ConfigHolder;
    use crate::ui::shell::AppShell;
    use folio_core::Config;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext, px, size};

    /// 越界的偏好值必须被夹住，否则排版会算出零高或无限宽。
    #[gpui_kit::test]
    fn out_of_range_preferences_are_clamped(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let store = folio_core::Store::in_memory().unwrap();
        let handle = cx.open_window(size(px(600.), px(400.)), |window, cx| {
            crate::ui::services::app_store::install(store, cx);
            ConfigHolder::install(Config::default(), cx);
            Root::new(cx.new(|cx| AppShell::boot(window, cx)), window, cx)
        });
        let handle: AnyWindowHandle = handle.into();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();

        cx.update(|cx| {
            ConfigHolder::edit_reader(cx, |p| {
                p.font_size = 9000.0;
                p.line_height = 0.0;
            });
        });
        let prefs = cx.update(|cx| ConfigHolder::reader(cx));
        assert_eq!(prefs.font_size, 72.0, "字号应被夹到上限");
        assert_eq!(prefs.line_height, 1.0, "行距应被夹到下限");
    }

    /// 外观模式走的是配置实体：改完立刻能读回来（落盘由 `ConfigPath` 决定，测试里不落盘）。
    #[gpui_kit::test]
    fn editing_the_appearance_updates_the_holder(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let handle: AnyWindowHandle = cx
            .open_window(size(px(900.), px(600.)), |window, cx| {
                crate::ui::services::app_store::install(
                    folio_core::Store::in_memory().expect("内存库"),
                    cx,
                );
                ConfigHolder::install(Config::default(), cx);
                let view = cx.new(|cx| AppShell::boot(window, cx));
                Root::new(view, window, cx)
            })
            .into();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        cx.update(|cx| {
            ConfigHolder::edit_appearance(cx, |a| a.mode = folio_core::Mode::Dark);
        });
        let mode = cx.update(|cx| ConfigHolder::appearance(cx).mode);
        assert_eq!(mode, folio_core::Mode::Dark);
    }

    /// 回归门：改配置必须通知观察者。
    ///
    /// 外壳把「落盘 + 重画」和欢迎页门禁都挂在对配置实体的 `observe` 上，而
    /// `Entity::update` 本身**不会**触发观察者 —— 少了一次 `cx.notify()`，配置就改得动、
    /// 读得回，却永远不落盘、界面也不重画。这条钉住的就是那句 `notify`。
    #[gpui_kit::test]
    fn editing_config_notifies_its_observers(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let seen = std::rc::Rc::new(std::cell::Cell::new(0usize));
        cx.update(|cx| {
            let holder = ConfigHolder::install(Config::default(), cx);
            let seen2 = seen.clone();
            cx.observe(&holder, move |_, _| seen2.set(seen2.get() + 1))
                .detach();
            ConfigHolder::edit_reader(cx, |p| p.font_size = 20.0);
        });
        assert!(seen.get() > 0, "改配置应通知观察者，实际 0 次");
    }
}
