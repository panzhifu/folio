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
//! 视图读的生效值是**两层叠出来**的：`用户层 ⊕ 环境变量 ⊕ 当前那本书的差量`。
//! 不含差量的那一层由 [`ConfigHolder::default_reader`] 与 [`ConfigHolder::default_appearance`]
//! 给出，设置屏读写的是它；阅读弹层改的是「这一本」（见
//! [`crate::ui::features::reader::model::ReaderSession`]）。少了这一分，「这一本字太大」
//! 就会被写成「往后每一本都太大」。
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
    Appearance, BookSettings, Config, Env, Language, Layer, Library, Opds, Provenance, Reader,
    Resolved, Speech, Sync, Translation,
};
use gpui_kit::{App, AppContext as _, Entity, Global};

use crate::ui::foundation::error::{Context as _, report};

pub struct ConfigState {
    /// **用户层**：内置默认 ⊕ 用户文件。**落盘写的是它**（§2.1 写回只写用户层）。
    pub user: Config,
    /// **默认层**：用户层 ⊕ 环境变量，**不含任何一本书的差量**。
    ///
    /// 设置屏那一侧读写的是它：那一屏回答的是「没单独调过的书长什么样」。让它等于
    /// [`config`](Self::config) 会让「先读某本书、再去设置屏改字号」把那本书的差量
    /// 固化成读者的默认值。
    pub defaults: Arc<Config>,
    /// **生效值**：默认层 ⊕ 当前挂着的那本书的差量。视图读它。**换 Arc，不改内容**（§2.3）。
    pub config: Arc<Config>,
    /// 阅读层里此刻挂着的那本书的差量；`None` = 没有书开着。
    ///
    /// 差量的**真相在库里**（`books.settings_json`，v26）；这一格只是开着那一本时的
    /// 内存副本，开书时读进来、关书时撤掉。开着书而这一本没被单独设过时它是
    /// `Some(空差量)` 而不是 `None` —— `None` 专指「没有书挂着」，两种情况要分得开。
    pub book: Option<BookSettings>,
    /// [`book`](Self::book) 换过几次。阅读会话按它决定要不要把差量抄进库里那一列，
    /// 于是每一帧的比较只是一个数，不必克隆两张 map
    /// （与 `folio_core::layout::LayoutKey::fonts_rev` 同一手法）。
    pub book_rev: u64,
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
        let entity = cx.new(|_| {
            let mut state = ConfigState {
                user,
                defaults: Arc::new(effective.clone()),
                config: Arc::new(effective),
                book: None,
                book_rev: 0,
                provenance,
                env,
                path,
            };
            // 走一遍重算：没有书挂着时它算出与上面同样的两层，而**入口只有一处** ——
            // 以后加来源层时不必回来改这里第二遍。
            recompute(&mut state);
            state
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

    /// **默认层**的排版输入（不含当前那本书的差量）。设置屏那一屏读写的是它。
    ///
    /// 为什么阅读弹层不能用它：那一格要改的是这一本书，而差量是与默认**逐格比**出来的
    /// （[`BookSettings::record_reader`]）—— 拿生效值当默认去比，读者调第二格时第一格
    /// 就会被记成「这本书的选择」，那是对的；反过来拿默认当生效值显示，滑条就会停在
    /// 读者没看见的位置上。
    pub fn default_reader(cx: &App) -> Reader {
        Self::entity(cx).read(cx).defaults.reader.clone()
    }

    /// **默认层**的外观（不含当前那本书的差量）。
    pub fn default_appearance(cx: &App) -> Appearance {
        Self::entity(cx).read(cx).defaults.appearance.clone()
    }

    /// 阅读层里挂着的那本书的差量；没开着书时是 `None`。
    pub fn book(cx: &App) -> Option<BookSettings> {
        Self::entity(cx).read(cx).book.clone()
    }

    /// [`book`](ConfigState::book) 换过几次 —— 会话用它决定这一帧要不要把差量抄进库，
    /// 不想写的时候一次 map 克隆都不做。
    pub fn book_rev(cx: &App) -> u64 {
        Self::entity(cx).read(cx).book_rev
    }

    /// 换上 / 撤下当前那本书的差量（开书与关书走这一条）。
    ///
    /// **同一份差量原样换上来时什么都不做**（不重算、不 `notify`、`book_rev` 不动）：
    /// 弹层每拖一格都会把整份差量换上来一次，重复通知会让外壳白重画、让落盘那一条
    /// 重新排 400 ms 的队。落库由会话比较 [`book_rev`](ConfigState::book_rev) 之后做。
    pub fn set_book(cx: &mut App, book: Option<BookSettings>) {
        Self::entity(cx).update(cx, |state, cx| {
            if state.book == book {
                return;
            }
            state.book = book;
            state.book_rev += 1;
            recompute(state);
            cx.notify();
        });
    }

    /// 抹掉**这一本书**的差量 —— 弹层底部那一格「恢复本书默认」走的就是这里。
    ///
    /// 只清内存里那一份并把号往后推：落库由会话每帧那一次比较去做（同一头，改一格与
    /// 清整份走的是一条路，不在这里再开一处直接写库的口子）。本来就是默认时不动、
    /// 不 notify —— 空转一次会让外壳重画、让落盘那条重新排队。
    pub fn clear_book(cx: &mut App) {
        Self::entity(cx).update(cx, |state, cx| {
            let Some(patch) = &mut state.book else {
                tracing::warn!("ConfigHolder: 没有书挂着却要恢复本书默认，已忽略");
                return;
            };
            if patch.is_empty() {
                return;
            }
            patch.clear();
            state.book_rev += 1;
            recompute(state);
            cx.notify();
        });
    }

    /// 改**这一本书**的排版输入 —— 阅读弹层那一屏走这一条，不走
    /// [`edit_reader`](Self::edit_reader)（那一条改的是默认层）。
    ///
    /// 闭包拿到的是这一本当前的生效值；改完与默认层逐格比，只把不同的那一格留在差量里。
    /// 于是读者把滑条推回默认位置时那一格自动消失，「所有书都用这个字号」去设置屏改。
    ///
    /// 没有书挂着时什么都不动、留一句日志：这一条只该由阅读层里的控件触发，真出现了
    /// 说明调用点错了 —— 而把它降级成改默认值，会让「这一本的字号」悄悄变成全局选择。
    pub fn edit_book_reader(cx: &mut App, f: impl FnOnce(&mut Reader)) {
        Self::entity(cx).update(cx, |state, cx| {
            let mut effective = state.config.reader.clone();
            f(&mut effective);
            let defaults = state.defaults.reader.clone();
            let Some(patch) = &mut state.book else {
                tracing::warn!("ConfigHolder: 没有书挂着却要改这一本的排版，已忽略");
                return;
            };
            patch.record_reader(&defaults, &effective);
            state.book_rev += 1;
            recompute(state);
            cx.notify();
        });
    }

    /// 改**这一本书**的外观（阅读浅深、背光、翻页方式与时长、划词默认样式）。
    ///
    /// 与 [`edit_book_reader`](Self::edit_book_reader) 同一条形状；`mode` / `light` /
    /// `dark` 那三格只归软件，[`BookSettings::record_appearance`] 那一头就不让它们进差量。
    pub fn edit_book_appearance(cx: &mut App, f: impl FnOnce(&mut Appearance)) {
        Self::entity(cx).update(cx, |state, cx| {
            let mut effective = state.config.appearance.clone();
            f(&mut effective);
            let defaults = state.defaults.appearance.clone();
            let Some(patch) = &mut state.book else {
                tracing::warn!("ConfigHolder: 没有书挂着却要改这一本的外观，已忽略");
                return;
            };
            patch.record_appearance(&defaults, &effective);
            state.book_rev += 1;
            recompute(state);
            cx.notify();
        });
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

    /// OPDS 书源列表。浏览页建屏时读一次。
    pub fn opds(cx: &App) -> Opds {
        Self::snapshot(cx).opds.clone()
    }

    /// 每一项的配置来自哪一层（§2.1）。诊断 dump 用。
    ///
    /// 当前那本书盖掉的格子在这里念 [`Layer::Book`]：dump 打的是**生效值**，而那一格
    /// 来自这一本书而不是 `config.json`。不标就等于在「我明明在设置屏改了字号、这本
    /// 书怎么不动」唯一能说清的地方说假话。
    pub fn provenance(cx: &App) -> Provenance {
        let state = Self::entity(cx).read(cx);
        let Some(book) = &state.book else {
            return state.provenance.clone();
        };
        let mut provenance = state.provenance.clone();
        for (section, keys) in [
            ("reader", &book.reader),
            ("appearance", &book.appearance),
        ] {
            for key in keys.keys() {
                provenance.mark(format!("{section}.{key}"), Layer::Book);
            }
        }
        provenance
    }

    /// 唯一的**默认层**写入口：改的是用户层（§2.1），改完重算两层，并同步全局 locale。
    ///
    /// 规整是因为面板里填进 9000 号字会把排版算成零高或无限宽，而认不出的浅深选择
    /// 该落回「跟随系统」；语言那一步是因为 `rust_i18n` 只认全局值，不认我们这份配置。
    ///
    /// **它改的是一本书也没单独设过的那些格子**：阅读弹层那一屏要改的是「这一本」，
    /// 走 [`crate::ui::features::reader::model::ReaderSession`] 那两个按书写入的方法，
    /// 不然「这一本字太大」会变成「往后每一本都太大」。
    ///
    /// **收尾要 `cx.notify()`**：观察者（外壳那份落盘 + 重画、欢迎页门禁）只在被观察实体
    /// `notify` 时才收到回调 —— `Entity::update` 本身不会替我们通知。少了这一句，配置
    /// 改得动、读得回，却永远不落盘、界面也不重画（这一句就是为修掉那个问题而加的）。
    pub fn edit(cx: &mut App, f: impl FnOnce(&mut Config)) {
        Self::entity(cx).update(cx, |state, cx| {
            // 改**用户层**：生效值是「用户层 ⊕ 环境变量 ⊕ 那本书的差量」算出来的，
            // 不能反过来改它 —— 那样一次编辑就会把环境变量的值（或一本书的选择）
            // 固化进落盘内容（§2.1 第 3 条要防的正是这个）。
            let mut user = state.user.clone();
            f(&mut user);
            let user = user.normalized();
            user.language.locale.apply();
            state.user = user;
            // 重算两层，换整个 Arc（§2.3）：手上有旧 Arc 的一方不受影响。
            recompute(state);
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

    pub fn edit_opds(cx: &mut App, f: impl FnOnce(&mut Opds)) {
        Self::edit(cx, |config| f(&mut config.opds));
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
        //
        // 当前那本书的差量**不在这一份里** —— 它跟着书走，落在库的 `books.settings_json`
        // （v26），由阅读会话在每一次改动时写。把书的选择烤进配置文件，等于让读过的那本
        // 书决定所有没单独设过的书长什么样。
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

/// 重算那两层：**默认层** = 用户层 ⊕ 环境变量，**生效值** = 默认层 ⊕ 当前那本书的差量。
///
/// 只有这一处算得出来，两个写入口（[`ConfigHolder::edit`] 与 [`ConfigHolder::set_book`]）
/// 都走它 —— 「读到的生效值」与「刚改的那一格」之间不存在第二条路径。
///
/// **每改一次算一遍，而不是让每个读点自己算**：阅读器每一帧都要读偏好
/// （`reader::model::session::sync_prefs`），而叠一层差量走的是 serde JSON —— 读点自己算
/// 等于把「每帧序列化整份 `Reader` 再哈希」那一趟请回来，排版键那里为同样的写法付过账
/// （见 `folio_core::layout::LayoutInputs`）。没有书挂着时两层共用同一个 `Arc`，
/// 一次克隆都不做。
fn recompute(state: &mut ConfigState) {
    let mut defaults = state.user.clone();
    state.env.apply(&mut defaults);
    let defaults = Arc::new(defaults);
    let effective = match &state.book {
        None => Arc::clone(&defaults),
        Some(book) => Arc::new(Config {
            reader: book.effective_reader(&defaults.reader),
            appearance: book.effective_appearance(&defaults.appearance),
            ..(*defaults).clone()
        }),
    };
    state.defaults = defaults;
    state.config = effective;
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
    use folio_core::{BookSettings, Config, Layer, Mode};
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

    /// 挂上一本书的差量：生效值跟着走，而**默认层一个字都不动**；撤掉之后生效值回默认。
    ///
    /// 「阅读页夜里读，关掉书书架还是白天」这件事的形状就在这里，而不在外壳：外壳只是
    /// 读生效值，「写进去的是哪一层」才决定关掉书之后屏幕上是什么。
    ///
    /// 那一格 `mode` 被界面**顺手**改成夜里 —— 差量不许带走它，这条断言就是钉这一点的。
    #[gpui_kit::test]
    fn a_book_overlay_moves_the_effective_values_and_leaves_the_defaults(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| ConfigHolder::install(Config::default(), cx));
        let base_reader = cx.update(|cx| ConfigHolder::default_reader(cx));
        let base_appearance = cx.update(|cx| ConfigHolder::default_appearance(cx));

        let mut wanted_reader = base_reader.clone();
        wanted_reader.font_size = 26.0;
        let mut wanted_appearance = base_appearance.clone();
        wanted_appearance.reading_mode = Some(Mode::Dark);
        wanted_appearance.mode = Mode::Dark;

        let mut patch = BookSettings::default();
        patch.record_reader(&base_reader, &wanted_reader);
        patch.record_appearance(&base_appearance, &wanted_appearance);
        cx.update(|cx| ConfigHolder::set_book(cx, Some(patch)));

        let effective = cx.update(|cx| (ConfigHolder::reader(cx), ConfigHolder::appearance(cx)));
        assert_eq!(effective.0.font_size, 26.0, "这本书的字号该落在生效值上");
        assert_eq!(effective.1.reading_mode, Some(Mode::Dark));
        assert_eq!(
            effective.1.mode, base_appearance.mode,
            "一本书不许改到软件外观的浅深"
        );
        let defaults = cx.update(|cx| (ConfigHolder::default_reader(cx), ConfigHolder::default_appearance(cx)));
        assert_eq!(defaults.0, base_reader, "默认层该原样");
        assert_eq!(defaults.1, base_appearance, "默认层该原样");

        // 关书：差量撤下，生效值整份回到默认层。
        cx.update(|cx| ConfigHolder::set_book(cx, None));
        assert_eq!(cx.update(|cx| ConfigHolder::reader(cx)), base_reader);
        assert_eq!(
            cx.update(|cx| ConfigHolder::appearance(cx).reading_mode),
            None
        );
    }

    /// 挂着书的时候改**默认**：这本书没盖过的格子跟着新默认走，盖过的那一格留着。
    ///
    /// 这一条就是「只存差量」而不是「第一次改动就快照整份」要买的东西 —— 快照那一版
    /// 写出来，老书从此不再跟随默认，而读者看不出为什么。
    #[gpui_kit::test]
    fn the_overlay_follows_the_default_it_overlays(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| ConfigHolder::install(Config::default(), cx));
        let base = cx.update(|cx| ConfigHolder::default_reader(cx));
        let mut wanted = base.clone();
        wanted.font_size = 26.0;
        let mut patch = BookSettings::default();
        patch.record_reader(&base, &wanted);
        cx.update(|cx| ConfigHolder::set_book(cx, Some(patch)));

        cx.update(|cx| ConfigHolder::edit_reader(cx, |p| p.line_height = 2.0));
        let effective = cx.update(|cx| ConfigHolder::reader(cx));
        assert_eq!(effective.line_height, 2.0, "这本书没盖行距，该跟着新默认");
        assert_eq!(effective.font_size, 26.0, "盖过的那一格不许被默认改动冲掉");
        assert_eq!(
            cx.update(|cx| ConfigHolder::default_reader(cx)).font_size,
            base.font_size,
            "落盘那份不该被这本书的字号污染"
        );
    }

    /// 被这本书盖过的格子在来源表里念 `book`，没盖过的还念它本来的层。
    ///
    /// 诊断 dump 打的是**生效值**，所以来路说错等于「我明明在设置屏改了字号」这一类
    /// 事故没有解药。
    #[gpui_kit::test]
    fn the_provenance_names_the_book_layer_only_for_overridden_fields(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| ConfigHolder::install(Config::default(), cx));
        let base = cx.update(|cx| ConfigHolder::default_reader(cx));
        let mut wanted = base.clone();
        wanted.font_size = 26.0;
        let mut patch = BookSettings::default();
        patch.record_reader(&base, &wanted);
        cx.update(|cx| ConfigHolder::set_book(cx, Some(patch)));

        let provenance = cx.update(|cx| ConfigHolder::provenance(cx));
        assert_eq!(provenance.layer_of("reader.font_size"), Layer::Book);
        assert_eq!(
            provenance.layer_of("reader.line_height"),
            Layer::Default,
            "没盖过的格子不该被报成这本书的选择"
        );
    }

    /// 同一份差量原样换第二次**不 notify**。
    ///
    /// 弹层每拖一格滑条都会把整份差量换上来一次；少了这道比较，落盘观察者每次都要
    /// 重新排 400 ms 的队，外壳也跟着白重画。
    #[gpui_kit::test]
    fn re_applying_the_same_overlay_notifies_once(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let seen = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let patch = cx.update(|cx| {
            let holder = ConfigHolder::install(Config::default(), cx);
            let counted = seen.clone();
            cx.observe(&holder, move |_, _| counted.set(counted.get() + 1))
                .detach();
            let base = ConfigHolder::default_reader(cx);
            let mut wanted = base.clone();
            wanted.font_size = 26.0;
            let mut patch = BookSettings::default();
            patch.record_reader(&base, &wanted);
            patch
        });
        assert_eq!(seen.get(), 0, "装配置不该被算成一次差量通知");

        // 三步各自一次 `update`：观察者的回调在本次 update 收尾之后才送达，
        // 断言写在同一个闭包里只会量到 0。
        cx.update(|cx| ConfigHolder::set_book(cx, Some(patch.clone())));
        assert_eq!(seen.get(), 1, "换上差量该通知一次");
        cx.update(|cx| ConfigHolder::set_book(cx, Some(patch)));
        assert_eq!(seen.get(), 1, "同一份差量原样换第二次不该再通知");
        cx.update(|cx| ConfigHolder::set_book(cx, None));
        assert_eq!(seen.get(), 2, "撤下差量是另一件事，该通知");
    }
}
