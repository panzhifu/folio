//! 检查更新的应用侧：一份状态、一个入口、一条「查完了要重绘」的链。
//!
//! ## 为什么状态挂 `Entity` 而不是一个 plain `Global`
//!
//! 设置页每一格的取值闭包是在**重绘时**被调的，而「谁改了 `Global` 里的值」不会让任何窗口
//! 重画 —— `cx.global_mut(..)` 不通知视图（同一件事在 `services/config.rs` 顶部写过：所以那边
//! 挂的是配置的**实体句柄**）。检查更新是一次异步往返，回来的那一刻必须让那一格自己变字，
//! 所以这里只能是 `Entity<UpdateState>`：写入方 `update` 它、视图 `observe` 它。
//!
//! ## 为什么懒建，而不在开机时装
//!
//! 与 [`crate::ui::services::storage`] 同一条理由，抄它的形状：建实体要 `&mut App`，而**按钮
//! 回调给的正是 `&mut App`**，所以「没装过」这一档在 [`entity`] 里就地补上，比在桌面与移动
//! 两条 boot 路径里各加一行更不容易漏（那两条 boot 是**重复的两份**，没有共用函数）。
//! 唯一的硬要求是：设置那两屏的构造里要 `cx.observe` 这个实体 —— 观察者挂在构造上，
//! 先有句柄才观察得到，见 `settings/view/desktop.rs` 与 `compact.rs`。
//!
//! ## 这一层不发任何请求，除非读者按了那颗按钮
//!
//! 没有开机检查、没有定时轮询、没有遥测。GitHub 的匿名配额是按 IP 60 次/小时，读者的每一次
//! 点击都在花他自己的配额 —— 这件事的默认值只能是「不点就不发生」。

use std::sync::{Arc, OnceLock};

use crate::ui::platform;
use crate::ui::services::l10n::tr;
use folio_core::net::remote::Transport;
use folio_core::net::update::{self, Asset, Outcome, Release};
use gpui_kit::{App, AppContext as _, Context, Entity, Global, SharedString, WeakEntity, Window};

/// 那一格当前该念哪一句。
///
/// `Failed` 里装的是 **core 那个三档错误**而不是已经翻好的句子：文案属于界面，
/// 「为什么失败」的判据属于协议层，两边各自演化时不该互相牵住。
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Status {
    /// 还没查过（进程起来就是这个）。
    #[default]
    Idle,
    Checking,
    /// 已经最新。
    Current,
    /// 有新版本，正文在 [`UpdateState::notes`]。
    Newer,
    Failed(update::Error),
}

/// 检查更新这一件事的全部状态。
/// 下载那一段走到哪了。与 [`Status`]（**有没有**新版本）是两件事，所以两个字段。
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Download {
    #[default]
    Idle,
    /// 已下到的字节数与包的总大小（总大小未知时为 0，界面上那句就不报百分比）。
    Running { done: u64, total: u64 },
    /// 下完且校验过，等读者点「安装」。
    Ready,
    /// 已把包交给系统安装器；真正的确认在系统那一屏。
    Installing,
    /// 需要读者去系统设置里开「安装未知应用」的权限。
    NeedsPermission,
    /// 校验不过、下坏了、或这一端没有包。带一句原因键。
    Failed(DownloadFailure),
}

/// 下载失败的原因，四档各有一句人话。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadFailure {
    /// 这一端压根没有可下的包（挑资产挑不到）。
    NoAsset,
    /// 传输断了 / 取消了。
    Transport,
    /// **SHA-256 对不上**。这一档绝不能与上面那档同句：它意味着拿到的包不是发出去的那个。
    Checksum,
    /// 落盘失败（目录建不出、磁盘满）。
    Io,
}

#[derive(Debug, Default)]
pub struct UpdateState {
    status: Status,
    download: Download,
    /// 校验过、等着交给安装器的那个包的路径。只有 [`Download::Ready`] 时有值 ——
    /// 存下来是因为第二下（「安装」）不该再去猜文件名，也不该重新算一遍摘要。
    apk: Option<std::path::PathBuf>,
    /// 上一次成功拿到的那条 release。**只有 `Newer` 时它才是新版本**：
    /// 查完是「已最新」时它留着的是当前这版对应的那条，「更新日志」那一格正是靠这一点
    /// 能在不先检查更新的情况下显示当前版的正文。
    latest: Option<Release>,
    /// **手上这一版**的 release 正文（按 `v{版本}` 那条 tag 取的）。
    ///
    /// 与 `latest` 分开存是有理由的：装了 0.1.1 而最新是 0.2.0 时，`latest` 讲的是 0.2.0，
    /// 拿它当「你这版的说明」给读者看是一句假话。
    notes: Option<String>,
    /// 正在取 `notes`。这一格为真的是「别发第二次」——配额按次数算。
    notes_loading: bool,
}

impl UpdateState {
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// 正在跑一次检查？那颗按钮据此禁用自己 —— 连点两次会发两个请求，
    /// 而 GitHub 的配额是按次数算的。
    pub fn busy(&self) -> bool {
        matches!(self.status, Status::Checking)
    }

    /// release 正文（Markdown）。没查过时 `None`。
    pub fn notes(&self) -> Option<&str> {
        self.latest.as_ref().map(|release| release.notes.as_str())
    }

    pub fn download(&self) -> &Download {
        &self.download
    }

    /// 查到的那一版是什么（`Current` 与 `Idle` 时 `None`）。
    pub fn found(&self) -> Option<&Release> {
        match self.status {
            Status::Newer => self.latest.as_ref(),
            _ => None,
        }
    }

    /// 这一端有没有可下的包 —— 有，才谈得上「下载」。
    pub fn asset(&self) -> Option<&Asset> {
        self.latest.as_ref().and_then(update::asset_for_platform)
    }

    /// 起一次检查。已经在跑就什么都不做（见 [`UpdateState::busy`]）。
    fn check_now(&mut self, cx: &mut Context<UpdateState>) {
        if self.busy() {
            return;
        }
        self.status = Status::Checking;
        cx.notify();

        let transport = transport();
        let local = env!("CARGO_PKG_VERSION").to_owned();
        // 出网这件事**不在界面线程上做**：`Transport` 是同步的（见 `net/update.rs` 顶部），
        // 一次 GitHub 往返慢的时候能让窗口卡住。丢给后台线程池，回来再落状态。
        let work = cx.background_spawn(async move { update::check(transport.as_ref(), &local) });
        cx.spawn(async move |this: WeakEntity<UpdateState>, cx| {
            let outcome = work.await;
            let _ = this.update(cx, |state, cx| state.settle(outcome, cx));
        })
        .detach();
    }

    fn settle(&mut self, outcome: Result<Outcome, update::Error>, cx: &mut Context<UpdateState>) {
        match outcome {
            Ok(Outcome::Newer(release)) => {
                tracing::info!("发现新版本 {}", release.version);
                self.latest = Some(release);
                self.status = Status::Newer;
            }
            Ok(Outcome::Current) => {
                // 「已最新」也要把那条 release 留住：它就是当前版的更新日志。
                tracing::info!("已经是最新（{}）", env!("CARGO_PKG_VERSION"));
                self.status = Status::Current;
            }
            Err(error) => {
                // 失败只记日志与上屏，不弹任何东西：一次检查失败不是读者的错。
                tracing::warn!("检查更新失败：{error}");
                self.status = Status::Failed(error);
            }
        }
        cx.notify();
    }
}

/// 进程共用的那一份出网传输（`ureq` + rustls）。
///
/// 与书架那条 URL 导入**共用同一个池**：一个进程两份连接池不只是多占内存，还会各算一次
/// DNS 与 TLS 握手，读者从书架下载一本书的同时点一次「检查更新」就要重连一次。
pub fn transport() -> Arc<dyn Transport> {
    static SHARED: OnceLock<Arc<dyn Transport>> = OnceLock::new();
    SHARED
        .get_or_init(folio_core::net::remote::ureq::shared)
        .clone()
}

pub struct UpdateHolder(pub Entity<UpdateState>);

impl Global for UpdateHolder {}

/// 句柄；没建过时 `None`。
pub fn handle(cx: &App) -> Option<Entity<UpdateState>> {
    cx.try_global::<UpdateHolder>()
        .map(|holder| holder.0.clone())
}

/// 取句柄，没有就先建一个（与 `storage::entity` 同一条理由：按钮回调手里就是 `&mut App`）。
pub fn entity(cx: &mut App) -> Entity<UpdateState> {
    if let Some(entity) = handle(cx) {
        return entity;
    }
    let entity = cx.new(|_cx| UpdateState::default());
    cx.set_global(UpdateHolder(entity.clone()));
    entity
}

/// 那颗「检查更新」按钮唯一的入口。
pub fn check(cx: &mut App) {
    let entity = entity(cx);
    entity.update(cx, |state, cx| state.check_now(cx));
}

/// 卡片能给的两种东西：**正文**，或者**一句为什么没有**。
///
/// 分两档而不是 `Option<String>` 带个错误字符串：离线时说「这一版没有留下更新日志」是一句
/// 假话，读者会去翻作者的发布页找一个根本不存在的条目，而真正该做的是看看自己的网络。
#[derive(Debug, Clone)]
pub enum Notes {
    Ready(String),
    Failed(SharedString),
}

/// 拿到**手上这一版**的更新日志正文，然后交给 `show` 去开那一屏。
///
/// 为什么把开窗这件事做成回调而不是服务层直接开：卡片长什么样属于界面，
/// 而「异步回来时手里已经没有 `&mut Window` 了」属于运行时形状。服务层只负责在拿到东西之后
/// 找回活动窗口（`active_window` + `update_window`），怎么画由调用处给。
///
/// 三条分支各有各的理由：
/// - **有缓存**：当场就开。离线时这是唯一还能看见正文的路，所以缓存不清。
/// - **正在取**：这一颗按钮什么都不做。连点两次会烧掉两次配额，而读者看不出区别。
/// - **没取过**：起一次按 tag 的取数，回来再开 —— 不先开一张空卡片赌它会不会自己重画，
///   `open_dialog` 的文档写的是「builder 可能在层重建时再跑」，不是「一定会再跑」。
pub fn with_notes(
    window: &mut Window,
    cx: &mut App,
    show: impl FnOnce(Notes, &mut Window, &mut App) + 'static,
) {
    let entity = entity(cx);
    if let Some(text) = entity.read(cx).notes.clone() {
        show(Notes::Ready(text), window, cx);
        return;
    }
    if entity.read(cx).notes_loading {
        return;
    }
    let target = window.window_handle();
    entity.update(cx, |state, cx| {
        state.notes_loading = true;
        let transport = transport();
        let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
        let work =
            cx.background_spawn(async move { update::release_by_tag(transport.as_ref(), &tag) });
        let this = cx.entity();
        cx.spawn(async move |_, cx| {
            // 结果先留着，**翻句子要等到拿到 `&App` 的那一刻**（下面 `cx.update` 里面）：
            // 文案表按当前界面语言查，而那件事只有 `App` 能做，`AsyncApp` 这一层做不了。
            let outcome = work.await;
            cx.update(|cx| {
                let notes = match outcome {
                    Ok(release) => {
                        let text = release.notes.clone();
                        this.update(cx, |state, app| {
                            state.notes = Some(text.clone());
                            state.notes_loading = false;
                            app.notify();
                        });
                        Notes::Ready(text)
                    }
                    Err(error) => {
                        this.update(cx, |state, app| {
                            state.notes_loading = false;
                            app.notify();
                        });
                        tracing::warn!("取本版本的更新日志失败：{error}");
                        Notes::Failed(status_label(&Status::Failed(error), cx))
                    }
                };
                // 用的是**按下那一刻那扇窗的句柄**，不是事后问 `active_window()`。
                // 后者在 Android 上实测返回 `None`（`platform.active_window()` 那条在
                // gpui-mobile 上没实现到桌面那个样子），症状是「查看」按下去什么也不发生。
                let _ = cx.update_window(target, |_, window, cx| show(notes, window, cx));
            });
        })
        .detach();
    });
}

/// 把新版本下到应用数据目录、校验、然后交给系统安装器。
///
/// 只在 Android 上有意义（`HAS_SELF_INSTALL` 那条判据在 `ui/platform/mod.rs`，界面据此决定
/// 摆不摆这颗按钮）。四步，每步失败都停在原地并把原因交给界面，**不重试也不静默跳过**：
/// 校验不过意味着拿到的包不是发出去的那个，那件事必须让读者看见。
///
/// 1. 下到 `<数据根>/apk/<资产名>`（`download_to_file` 自己清残留、写 `.part`、改名）。
/// 2. **算 SHA-256 与资产上那份摘要比**。摘要缺失时不放行 —— 没有可比的东西就是没验过。
/// 3. 问 `canRequestInstalls()`；没授权就把读者送到系统那一页，**不自动续跑**
///    （权限什么时候开由读者决定，自动续跑会撞上「还没生效」那一档，看起来像坏了）。
/// 4. 交给系统安装器。真正的确认在系统那一屏，装完进程被杀掉是预期。
///
/// 进度为什么绕两道通道：下载跑在后台线程池上，而 `Entity` 只能在主线程改。
/// `mpsc`（阻塞侧）→ `async_channel`（异步侧）→ 主线程 `update` 这条形状抄
/// `features/shelf/view/mod.rs` 的导入进度，不另发明一套。
pub fn download_and_install(cx: &mut App) {
    if !platform::HAS_SELF_INSTALL {
        return;
    }
    let holder = entity(cx);
    let Some((url, name, total, expected)) = holder.read(cx).asset().map(|asset| {
        (
            asset.url.clone(),
            asset.name.clone(),
            asset.size,
            asset.sha256_hex().map(str::to_owned),
        )
    }) else {
        holder.update(cx, |s, app| {
            s.download = Download::Failed(DownloadFailure::NoAsset);
            app.notify();
        });
        return;
    };
    if matches!(holder.read(cx).download(), Download::Running { .. }) {
        return; // 一次只跑一个下载
    }
    let dest = folio_core::infra::paths::data_dir().join("apk").join(&name);
    holder.update(cx, |s, app| {
        s.download = Download::Running { done: 0, total };
        app.notify();
    });

    let transport = transport();
    let (tx, rx) = std::sync::mpsc::channel::<u64>();
    let work = cx.background_spawn(async move {
        let cancel = folio_core::net::remote::Cancel::new();
        let result =
            update::download_to_file(transport.as_ref(), &url, &dest, &cancel, move |done| {
                // 发送失败只意味着收的那一头已经没了，不该让下载线程为此 panic。
                let _ = tx.send(done);
            });
        (result, dest, expected)
    });
    let (utx, urx) = async_channel::unbounded::<u64>();
    cx.background_spawn(async move {
        // `rx.recv()` 是阻塞的，跑在后台线程池上正是它的去处；转成异步侧的通道交给主线程。
        while let Ok(done) = rx.recv() {
            if utx.try_send(done).is_err() {
                break;
            }
        }
    })
    .detach();
    holder.update(cx, move |_state, app| {
        app.spawn(async move |this: WeakEntity<UpdateState>, cx| {
            while let Ok(done) = urx.recv().await {
                let _ = this.update(cx, |state, app| {
                    state.download = Download::Running { done, total };
                    app.notify();
                });
            }
            let (result, dest, expected) = work.await;
            cx.update(|cx| {
                let Some(this) = this.upgrade() else { return };
                match result {
                    Err(error) => {
                        tracing::warn!("下载未完成：{error}");
                        this.update(cx, |state, app| {
                            state.download = Download::Failed(DownloadFailure::Transport);
                            app.notify();
                        });
                    }
                    Ok(()) => {
                        // 校验走的是**已经改名的最终文件**：`.part` 那一步在 `finish` 里结清了。
                        if !update::matches_sha256(&dest, expected.as_deref()) {
                            tracing::error!("下载的包校验不过，删掉不发：{}", dest.display());
                            let _ = std::fs::remove_file(&dest);
                            this.update(cx, |state, app| {
                                state.download = Download::Failed(DownloadFailure::Checksum);
                                app.notify();
                            });
                            return;
                        }
                        // 校验过了**先停下**：交给安装器是读者的第二下，不是下载完顺手做的事。
                        // 用户要的形状就是「检验 → 确认 → 安装」那三步。
                        this.update(cx, |state, app| {
                            state.download = Download::Ready;
                            state.apk = Some(dest);
                            app.notify();
                        });
                    }
                }
            });
        })
        .detach();
    });
}

/// 把**已经校验过**的那个包交给系统安装器。第二下，与 [`download_and_install`] 分开的理由
/// 写在上面那段：下载完就自动弹安装器，等于把「确认」这一步替读者做了。
pub fn install_verified(cx: &mut App) {
    let holder = entity(cx);
    let Some(path) = holder.read(cx).apk.clone() else {
        return;
    };
    if !platform::can_request_installs() {
        // 送他去系统那一页，状态停在 NeedsPermission；回来之后**这一屏再点一次**。
        // 不自动续跑：权限什么时候生效由读者决定，续跑会撞上「还没生效」那一档，看起来像坏了。
        platform::open_install_permission_settings();
        holder.update(cx, |state, app| {
            state.download = Download::NeedsPermission;
            app.notify();
        });
        return;
    }
    platform::install_apk(&path);
    holder.update(cx, |state, app| {
        state.download = Download::Installing;
        app.notify();
    });
}

/// 下载状态那一句。与 `status_label` 同一条理由住在服务层：两屏都要念同一句。
pub fn download_label(download: &Download, cx: &App) -> SharedString {
    match download {
        Download::Idle | Download::Running { .. } => tr(cx, "update.downloading"),
        Download::Ready => tr(cx, "update.ready"),
        Download::Installing => tr(cx, "update.installing"),
        Download::NeedsPermission => tr(cx, "update.need_permission"),
        Download::Failed(DownloadFailure::NoAsset) => tr(cx, "update.fail_no_asset"),
        Download::Failed(DownloadFailure::Transport) => tr(cx, "update.fail_transport"),
        Download::Failed(DownloadFailure::Checksum) => tr(cx, "update.fail_checksum"),
        Download::Failed(DownloadFailure::Io) => tr(cx, "update.fail_io"),
    }
}

/// 状态 → 那一格要念的一句（键名见 `locales/*.toml` 的 `[update]` 段）。
///
/// 文案住在这里而不是视图里，是因为**两屏**（桌面与紧凑）都要念同一句；两处各写一遍
/// 的结局是某一将来只改了一处，手机上说「已是最新」而桌面上说「检查失败」。
///
/// 每一句都是 `tr` 直接吃**字面量键名**，不先取键名再传进去：`services/l10n.rs` 里那条扫
/// 字面量的门（`every_literal_key_resolves_in_every_language`）只认写在调用处的键名，
/// 绕过去之后漏一条键不会报错，屏幕上念的就是键名本身。
/// （这条门连**注释里**举的例子都扫，所以这里不能写出一个假的调用形状。）
pub fn status_label(status: &Status, cx: &App) -> SharedString {
    match status {
        Status::Idle => tr(cx, "update.idle"),
        Status::Checking => tr(cx, "update.checking"),
        Status::Current => tr(cx, "update.current"),
        Status::Newer => tr(cx, "update.newer"),
        Status::Failed(update::Error::Unreachable(_)) => tr(cx, "update.offline"),
        Status::Failed(update::Error::RateLimited) => tr(cx, "update.rate_limited"),
        Status::Failed(update::Error::NoRelease) => tr(cx, "update.no_release"),
        Status::Failed(update::Error::Unexpected { .. }) => tr(cx, "update.server"),
        Status::Failed(update::Error::Malformed(_)) => tr(cx, "update.malformed"),
    }
}

#[cfg(test)]
mod tests {
    //! 这一层能被测到的正是它**唯一自己决定**的两件事：状态机的转换，和「失败五档各自
    //! 念不同的一句」。网络那一头在 `folio_core::net::update` 里已经用假传输测透了。

    use folio_core::Config;
    use gpui_kit::TestAppContext;

    use super::*;
    use crate::ui::services::config::ConfigHolder;

    /// 取文案要先有语言来源：`l10n::locale` 读的是 `ConfigHolder`，没装过会当场炸。
    fn prepared(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| ConfigHolder::install(Config::default(), cx));
    }

    /// 断言写在 `read` 的闭包**里面**而不是外面：`status()` 给的是借用，
    /// 借出去就出不了那个闭包的生命周期（编译期就拦住了，不是风格问题）。
    #[gpui_kit::test]
    fn a_finished_check_lands_on_current(cx: &mut TestAppContext) {
        prepared(cx);
        let entity = cx.update(|cx| cx.new(|_cx| UpdateState::default()));
        assert!(cx.read(|cx| entity.read(cx).status() == &Status::Idle));
        assert!(!cx.read(|cx| entity.read(cx).busy()));
        let release = Release {
            version: update::Version {
                major: 0,
                minor: 1,
                patch: 1,
                pre: None,
            },
            notes: "改了什么".into(),
            assets: Vec::new(),
        };
        entity.update(cx, |state, app| {
            state.settle(Ok(Outcome::Newer(release)), app)
        });
        assert!(cx.read(|cx| entity.read(cx).status() == &Status::Newer));
        assert!(cx.read(|cx| entity.read(cx).notes() == Some("改了什么")));
        assert!(
            cx.read(|cx| entity.read(cx).found().is_some()),
            "Newer 时 found() 要给出那一版"
        );
    }

    #[gpui_kit::test]
    fn current_still_keeps_the_release_it_was_told(cx: &mut TestAppContext) {
        // 「已最新」时正文也要留住：更新日志那一格靠它显示当前版的改动，
        // 而不是非要先查出一个新版本才有东西可看。
        prepared(cx);
        let entity = cx.update(|cx| cx.new(|_cx| UpdateState::default()));
        entity.update(cx, |state, app| state.settle(Ok(Outcome::Current), app));
        assert!(cx.read(|cx| entity.read(cx).status() == &Status::Current));
        assert!(cx.read(|cx| entity.read(cx).notes().is_none()));
        assert!(
            cx.read(|cx| entity.read(cx).found().is_none()),
            "Current 时 found() 必须是空的 —— 否则界面会念出「发现新版本 0.0.0」"
        );
    }

    #[gpui_kit::test]
    fn checking_refuses_a_second_click(cx: &mut TestAppContext) {
        // 连点两下会发两个请求，而配额按次数算 —— 所以第二次必须什么都不做。
        prepared(cx);
        let entity = cx.update(|cx| cx.new(|_cx| UpdateState::default()));
        entity.update(cx, |state, _app| state.status = Status::Checking);
        assert!(cx.read(|cx| entity.read(cx).busy()));
        entity.update(cx, |state, app| state.check_now(app));
        assert!(cx.read(|cx| entity.read(cx).status() == &Status::Checking));
    }

    #[gpui_kit::test]
    fn each_failure_kind_gets_its_own_sentence(cx: &mut TestAppContext) {
        prepared(cx);
        let cases = [
            update::Error::Unreachable("dns".into()),
            update::Error::RateLimited,
            update::Error::NoRelease,
            update::Error::Unexpected { status: 502 },
            update::Error::Malformed("x".into()),
        ];
        let mut labels: Vec<String> = cases
            .iter()
            .map(|error| {
                cx.read(|cx| status_label(&Status::Failed(error.clone()), cx))
                    .to_string()
            })
            .collect();
        for label in &labels {
            assert!(
                !label.starts_with("update."),
                "文案没解出来（屏幕上会念键名本身）：{label}"
            );
        }
        labels.dedup();
        assert_eq!(
            labels.len(),
            cases.len(),
            "五档失败要念五句不同的话，那才是「三种成因三句话」那条规矩的落地：{labels:?}"
        );
    }
}
