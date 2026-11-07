//! 网盘取书：整屏浏览器（账号 → 目录 → 勾选 → 下载），从架面「⋮」菜单压上导航栈。
//!
//! 一页只管一件事：把 core 读出来的目录清单摆出来 —— 目录是一行点进去，文件是一行
//! 勾上。下载走现有导入管线（[`CloudSource`] 挂着当时的账号现造一台
//! [`Drive`](folio_core::net::cloud::Drive)），所以判重、哈希、报表与书架刷新都复用
//! 同一套。
//!
//! 内层不用逐层压栈：整个浏览器是导航栈上的**一页**，顶栏的返回先回上一级目录，
//! 到了根再退账号页，最后才由系统返回键弹掉整页 —— 与 OPDS 浏览器同一条规矩，
//! 只是目录树没有 feed 那种「一页内容很重」的问题，一层字符串路径就够了。

use std::collections::HashSet;

use folio_core::net::cloud::baidu::BaiduDrive;
use folio_core::net::cloud::baidu_auth;
use folio_core::net::cloud::dav::DavDrive;
use folio_core::net::cloud::{Drive, Entry};
use folio_core::net::remote::Error as RemoteError;
use folio_core::{
    CloudItem, CloudSource, ImportOutcome, ImportReport, TaskEvent, WebDavDrive, submit_import,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::{NavMotion, NavStackState};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    WeakEntity, Window, div, px,
};

use crate::ui::components::{TopBar, muted_label};
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::{tplural, tr};
use crate::ui::services::resident;
use crate::ui::services::{app_store, library};

const FORM_WIDTH: f32 = 560.0;

/// 架面那边唯一需要接的一件事：有书进了库，架面要重查。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudEvent {
    Imported,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Drives,
    WebDavForm,
    BaiduForm,
    Browse,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Failure {
    Auth,
    Network,
    Remote,
}

/// 网盘有哪几台。要标「这是谁交的」时由那台 [`Drive::label`] 自报，
/// 这里只负责分流账号表单与客户端构造。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    WebDav,
    Baidu,
}

struct WebDavForm {
    url: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    dir: Entity<InputState>,
    error: Option<SharedString>,
}

struct BaiduForm {
    token: Entity<InputState>,
    /// OAuth 授权码：读者从授权页粘回来的那一串。非空时保存走「换 token」那一路，
    /// token 框空着也行 —— 两条来路在保存那一格分岔。
    code: Entity<InputState>,
    /// 换 token 的请求在路上：保存与「打开授权页」都按住，不叠第二发。
    linking: bool,
    error: Option<SharedString>,
}

struct ImportState {
    message: SharedString,
    done: bool,
}

pub struct CloudView {
    stack: WeakEntity<NavStackState>,
    screen: Screen,
    webdav: WebDavDrive,
    baidu: folio_core::BaiduDrive,
    form: Option<WebDavForm>,
    baidu_form: Option<BaiduForm>,
    kind: Kind,
    /// 当前目录（Drive 的相对路径语义，空 = 根）。
    path: String,
    entries: Vec<Entry>,
    selected: HashSet<String>,
    loading: bool,
    error: Option<Failure>,
    importing: Option<ImportState>,
    /// 这一轮浏览里 token 已经静默续过一次：刷新再失败就把错误亮出来，不无限转。
    refreshed: bool,
}

impl EventEmitter<CloudEvent> for CloudView {}

impl CloudView {
    pub fn new(stack: WeakEntity<NavStackState>, cx: &mut Context<Self>) -> Self {
        let cloud = ConfigHolder::cloud(cx);
        Self {
            stack,
            screen: Screen::Drives,
            webdav: cloud.webdav,
            baidu: cloud.baidu,
            form: None,
            baidu_form: None,
            kind: Kind::WebDav,
            path: String::new(),
            entries: Vec::new(),
            selected: HashSet::new(),
            loading: false,
            error: None,
            importing: None,
            refreshed: false,
        }
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        match self.screen {
            Screen::WebDavForm | Screen::BaiduForm => {
                self.form = None;
                self.baidu_form = None;
                self.screen = Screen::Drives;
                cx.notify();
            }
            Screen::Browse => {
                if let Some((parent, _)) = self.path.rsplit_once('/') {
                    self.path = parent.to_string();
                    self.selected.clear();
                    self.load_dir(cx);
                } else if !self.path.is_empty() {
                    self.path = String::new();
                    self.selected.clear();
                    self.load_dir(cx);
                } else {
                    self.screen = Screen::Drives;
                    cx.notify();
                }
            }
            Screen::Drives => {
                let _ = self
                    .stack
                    .update(cx, |stack, cx| stack.pop(NavMotion::Animated, cx));
            }
        }
    }

    fn open_webdav_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = cx.new(|cx| InputState::new(window, cx).default_value(self.webdav.url.clone()));
        let username =
            cx.new(|cx| InputState::new(window, cx).default_value(self.webdav.username.clone()));
        let password =
            cx.new(|cx| InputState::new(window, cx).default_value(self.webdav.password.clone()));
        let dir = cx.new(|cx| InputState::new(window, cx).default_value(self.webdav.dir.clone()));
        self.form = Some(WebDavForm {
            url,
            username,
            password,
            dir,
            error: None,
        });
        self.screen = Screen::WebDavForm;
        cx.notify();
    }

    fn save_webdav(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.form.as_ref() else {
            return;
        };
        let url = form.url.read(cx).value().trim().to_string();
        let username = form.username.read(cx).value().trim().to_string();
        let password = form.password.read(cx).value().to_string();
        let dir = form
            .dir
            .read(cx)
            .value()
            .trim()
            .trim_matches('/')
            .to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) || username.is_empty() {
            if let Some(form) = self.form.as_mut() {
                form.error = Some(tr(cx, "cloud.form_invalid"));
            }
            cx.notify();
            return;
        }
        ConfigHolder::edit_cloud(cx, |cloud| {
            cloud.webdav = WebDavDrive {
                url: url.clone(),
                username: username.clone(),
                password: password.clone(),
                dir: dir.clone(),
            };
        });
        self.webdav = WebDavDrive {
            url,
            username,
            password,
            dir,
        };
        self.form = None;
        self.screen = Screen::Drives;
        cx.notify();
    }

    fn open_baidu_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let token =
            cx.new(|cx| InputState::new(window, cx).default_value(self.baidu.access_token.clone()));
        let code = cx.new(|cx| InputState::new(window, cx));
        self.baidu_form = Some(BaiduForm {
            token,
            code,
            linking: false,
            error: None,
        });
        self.screen = Screen::BaiduForm;
        cx.notify();
    }

    fn save_baidu(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.baidu_form.as_ref() else {
            return;
        };
        if form.linking {
            return;
        }
        let code = form.code.read(cx).value().trim().to_string();
        let token = form.token.read(cx).value().trim().to_string();
        if !code.is_empty() {
            // 授权码那一路：换 token 在网络上，去后台走（见 [`Self::link_baidu`]）。
            self.link_baidu(code, cx);
            return;
        }
        if token.is_empty() {
            if let Some(form) = self.baidu_form.as_mut() {
                form.error = Some(tr(cx, "cloud.token_missing"));
            }
            cx.notify();
            return;
        }
        // 手贴 token 的老路：没有刷新凭据，过期了界面请他重贴。
        let baidu = folio_core::BaiduDrive {
            access_token: token,
            ..folio_core::BaiduDrive::default()
        };
        ConfigHolder::edit_cloud(cx, |cloud| cloud.baidu = baidu.clone());
        self.baidu = baidu;
        self.baidu_form = None;
        self.screen = Screen::Drives;
        cx.notify();
    }

    /// 授权码那一路：换 token（网络）→ 问账号名 → 落盘 → 回账号页。
    ///
    /// 换出来的 `refresh_token` 是今后唯一有效的那支（百度会轮换），必须落盘；
    /// 账号名问不到不挡路 —— 显示名空着，token 照常能用。
    fn link_baidu(&mut self, code: String, cx: &mut Context<Self>) {
        if let Some(form) = self.baidu_form.as_mut() {
            form.linking = true;
            form.error = None;
        }
        cx.notify();
        let work = cx.background_spawn(async move {
            baidu_auth::exchange_code(&code, crate::ui::services::update::transport()).map(
                |tokens| {
                    let account = baidu_auth::account_name(
                        &tokens.access_token,
                        crate::ui::services::update::transport(),
                    )
                    .unwrap_or_default();
                    (tokens, account)
                },
            )
        });
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_this: WeakEntity<CloudView>, cx| {
            let result = work.await;
            let _ = weak.update(cx, |this, cx| this.settle_link(result, cx));
        })
        .detach();
    }

    fn settle_link(
        &mut self,
        result: Result<(baidu_auth::BaiduTokens, String), RemoteError>,
        cx: &mut Context<Self>,
    ) {
        let (tokens, account) = match result {
            Ok(pair) => pair,
            Err(error) => {
                tracing::warn!("cloud: 百度授权换 token 失败：{error}");
                if let Some(form) = self.baidu_form.as_mut() {
                    form.linking = false;
                    form.error = Some(failure_text(failure_of(&error), cx));
                }
                cx.notify();
                return;
            }
        };
        let baidu = linked_drive(&tokens, account, self.baidu.account.clone());
        ConfigHolder::edit_cloud(cx, |cloud| cloud.baidu = baidu.clone());
        self.baidu = baidu;
        self.baidu_form = None;
        self.screen = Screen::Drives;
        cx.notify();
    }

    /// token 过期（errno 111 那一档）而有刷新凭据：静默续一次再重列。
    /// 读者不该为「30 天到了」再跑一趟授权。
    fn refresh_baidu(&mut self, cx: &mut Context<Self>) {
        let refresh_token = self.baidu.refresh_token.clone();
        self.loading = true;
        cx.notify();
        let work = cx.background_spawn(async move {
            baidu_auth::refresh_tokens(&refresh_token, crate::ui::services::update::transport())
        });
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_this: WeakEntity<CloudView>, cx| {
            let result = work.await;
            let _ = weak.update(cx, |this, cx| this.settle_refresh(result, cx));
        })
        .detach();
    }

    fn settle_refresh(
        &mut self,
        result: Result<baidu_auth::BaiduTokens, RemoteError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(tokens) => {
                let baidu = linked_drive(&tokens, String::new(), self.baidu.account.clone());
                ConfigHolder::edit_cloud(cx, |cloud| cloud.baidu = baidu.clone());
                self.baidu = baidu;
                self.load_dir(cx);
            }
            Err(error) => {
                tracing::warn!("cloud: 百度 token 续期失败：{error}");
                self.loading = false;
                self.error = Some(failure_of(&error));
                cx.notify();
            }
        }
    }

    /// 进一台网盘的目录树。没配账号就先带去表单 —— 这是能力不存在时的统一降级，
    /// 不是一句报错。
    fn browse(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        let configured = match kind {
            Kind::WebDav => self.webdav.configured(),
            Kind::Baidu => self.baidu.configured(),
        };
        if !configured {
            match kind {
                Kind::WebDav => self.open_webdav_form(window, cx),
                Kind::Baidu => self.open_baidu_form(window, cx),
            }
            return;
        }
        self.kind = kind;
        self.refreshed = false;
        // WebDAV 的默认目录是配置里那格；百度没有这格，从根开始。
        self.path = match kind {
            Kind::WebDav => self.webdav.dir.clone(),
            Kind::Baidu => String::new(),
        };
        self.selected.clear();
        self.entries.clear();
        self.screen = Screen::Browse;
        self.load_dir(cx);
    }

    fn load_dir(&mut self, cx: &mut Context<Self>) {
        let Some(drive) = build_drive(self.kind, cx) else {
            // 账号半路被清掉（理论上 browse 已挡）：回到账号页，不给一屏空列表。
            self.screen = Screen::Drives;
            cx.notify();
            return;
        };
        let path = self.path.clone();
        self.loading = true;
        self.error = None;
        cx.notify();

        let work = cx.background_spawn(async move { drive.list(&path) });
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_this: WeakEntity<CloudView>, cx| {
            let result = work.await;
            let _ = weak.update(cx, |this, cx| this.settle_dir(result, cx));
        })
        .detach();
    }

    fn settle_dir(&mut self, result: Result<Vec<Entry>, RemoteError>, cx: &mut Context<Self>) {
        self.loading = false;
        match result {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
            }
            Err(error) => {
                tracing::warn!("cloud: 列 {:?} 失败：{error}", self.path);
                if failure_of(&error) == Failure::Auth
                    && self.kind == Kind::Baidu
                    && self.baidu.refreshable()
                    && !self.refreshed
                {
                    self.refreshed = true;
                    self.refresh_baidu(cx);
                    return;
                }
                self.error = Some(failure_of(&error));
            }
        }
        cx.notify();
    }

    /// 进一层目录：换路径、清勾选、重列。
    fn open_entry(&mut self, path: String, cx: &mut Context<Self>) {
        self.path = path;
        self.selected.clear();
        self.entries.clear();
        self.load_dir(cx);
    }

    fn toggle(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.selected.remove(&path) {
            self.selected.insert(path);
        }
        cx.notify();
    }

    fn start_download(&mut self, cx: &mut Context<Self>) {
        if self.importing.as_ref().is_some_and(|state| !state.done) {
            return;
        }
        if self.selected.is_empty() {
            return;
        }
        let Some(dir) = library::target_dir(&ConfigHolder::library(cx)) else {
            self.importing = Some(ImportState {
                message: tr(cx, "cloud.library_missing"),
                done: true,
            });
            cx.notify();
            return;
        };
        let Some(store) = app_store::handle(cx) else {
            self.importing = Some(ImportState {
                message: tr(cx, "cloud.library_missing"),
                done: true,
            });
            cx.notify();
            return;
        };
        let Some(drive) = build_drive(self.kind, cx) else {
            return;
        };
        let items: Vec<CloudItem> = self
            .entries
            .iter()
            .filter(|entry| !entry.is_dir && self.selected.contains(&entry.path))
            .map(|entry| CloudItem {
                path: entry.path.clone(),
                name: entry.name.clone(),
                size: entry.size,
            })
            .collect();
        let count = items.len();
        if items.is_empty() {
            return;
        }
        let source = CloudSource::new(drive, items);
        let handle = submit_import(&resident::pool(), Box::new(source), dir, store);
        self.importing = Some(ImportState {
            message: tplural(cx, "cloud.importing", count).into(),
            done: false,
        });
        cx.notify();

        let (forward, events) = async_channel::unbounded::<TaskEvent<Vec<ImportOutcome>>>();
        cx.background_spawn(async move {
            for event in handle.events.iter() {
                if forward.send_blocking(event).is_err() {
                    break;
                }
            }
        })
        .detach();
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_this: WeakEntity<CloudView>, cx| {
            while let Ok(event) = events.recv().await {
                let done = weak
                    .update(cx, |this, cx| this.import_event(event, cx))
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    fn import_event(
        &mut self,
        event: TaskEvent<Vec<ImportOutcome>>,
        cx: &mut Context<Self>,
    ) -> bool {
        match event {
            TaskEvent::Finished(result) => {
                let (message, added) = match result {
                    Ok(outcomes) => {
                        let report = ImportReport::tally(&outcomes);
                        tracing::info!("cloud: 一批取书结清 —— {}", report.summary());
                        if report.added > 0 {
                            (tr(cx, "cloud.imported"), true)
                        } else if report.duplicate > 0 {
                            (tr(cx, "cloud.import_duplicate"), false)
                        } else {
                            (tr(cx, "cloud.import_failed"), false)
                        }
                    }
                    Err(error) => {
                        tracing::warn!("cloud: 取书失败：{error}");
                        (tr(cx, "cloud.import_failed"), false)
                    }
                };
                if let Some(importing) = self.importing.as_mut() {
                    importing.message = message;
                    importing.done = true;
                }
                if added {
                    cx.emit(CloudEvent::Imported);
                }
                cx.notify();
                true
            }
            TaskEvent::Cancelled => {
                if let Some(importing) = self.importing.as_mut() {
                    importing.message = tr(cx, "cloud.import_cancelled");
                    importing.done = true;
                }
                cx.notify();
                true
            }
            TaskEvent::Started | TaskEvent::Progress(_) | TaskEvent::Bytes(_) => false,
        }
    }
}

/// 按当时的配置现造一台网盘客户端。凭据只在这一批活里活着 —— 改了账号、清了
/// token，旧的那台随对象一起消失。
fn build_drive(kind: Kind, cx: &App) -> Option<Box<dyn Drive>> {
    let cloud = ConfigHolder::cloud(cx);
    let transport = crate::ui::services::update::transport();
    Some(match kind {
        Kind::WebDav if cloud.webdav.configured() => {
            let webdav = cloud.webdav;
            Box::new(DavDrive::open(
                webdav.url,
                webdav.username,
                webdav.password,
                transport,
            ))
        }
        Kind::Baidu if cloud.baidu.configured() => {
            Box::new(BaiduDrive::new(cloud.baidu.access_token, transport))
        }
        _ => return None,
    })
}

/// 换到 / 刷新到的一对 token → 落盘形状：到期时刻在此刻折算（Unix 秒）。
///
/// 账号名问到了就用新的；没问到（uinfo 失败不挡路）沿用已存的那一条 —— 账号列表
/// 上「这是谁」不该因为一次续期就变成空白。
fn linked_drive(
    tokens: &baidu_auth::BaiduTokens,
    account: String,
    fallback: String,
) -> folio_core::BaiduDrive {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    folio_core::BaiduDrive {
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone(),
        expires_at: now + tokens.expires_in.max(0),
        account: if account.is_empty() {
            fallback
        } else {
            account
        },
    }
}

fn failure_of(error: &RemoteError) -> Failure {
    match error {
        RemoteError::Transport(_) => Failure::Network,
        RemoteError::Unauthorized { .. }
        | RemoteError::Forbidden { .. }
        | RemoteError::DigestRequired => Failure::Auth,
        _ => Failure::Remote,
    }
}

fn failure_text(failure: Failure, cx: &App) -> SharedString {
    tr(
        cx,
        match failure {
            Failure::Auth => "cloud.fail_auth",
            Failure::Network => "cloud.fail_network",
            Failure::Remote => "cloud.fail_remote",
        },
    )
}

/// 字节数的一行说明：给人看大小，不参与任何逻辑。统一一位小数 ——
/// 「640.0 KB」比「0 KB」（实际 500 字节）诚实。
fn size_label(size: Option<u64>) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    match size {
        None | Some(0) => String::new(),
        Some(bytes) => {
            let bytes = bytes as f64;
            if bytes >= GB {
                format!("{:.1} GB", bytes / GB)
            } else if bytes >= MB {
                format!("{:.1} MB", bytes / MB)
            } else {
                format!("{:.1} KB", bytes / KB)
            }
        }
    }
}

impl Render for CloudView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match self.screen {
            Screen::Drives => tr(cx, "cloud.title"),
            Screen::WebDavForm => tr(cx, "cloud.webdav"),
            Screen::BaiduForm => tr(cx, "cloud.baidu"),
            Screen::Browse => {
                let name = self.path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
                if name.is_empty() {
                    tr(cx, "cloud.root")
                } else {
                    SharedString::from(name.to_string())
                }
            }
        };
        let view = cx.entity().clone();
        let top = TopBar::new("cloud-top-bar")
            .title(title)
            .back_tooltip(tr(cx, "shelf.back"))
            .on_back(move |_, cx| {
                view.update(cx, |this, cx| this.back(cx));
            });

        let body = match self.screen {
            Screen::Drives => self.drives_body(window, cx),
            Screen::WebDavForm => self.webdav_form_body(window, cx),
            Screen::BaiduForm => self.baidu_form_body(window, cx),
            Screen::Browse => self.browse_body(cx),
        };
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(top)
            .child(body)
    }
}

impl CloudView {
    fn drives_body(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // 门禁：网盘取书是会员功能（判据只有 activation::premium 一条）。非会员这一屏
        // 整段换成一句引导 —— 账号表单与目录树对还没买的人是噪音。
        if !crate::ui::services::activation::premium(cx) {
            return v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .items_center()
                .justify_center()
                .px_6()
                .child(
                    div()
                        .max_w(px(420.0))
                        .w_full()
                        .child(crate::ui::components::gate_banner(
                            tr(cx, "membership.gate_cloud"),
                            cx,
                        )),
                )
                .into_any_element();
        }
        let view_browse_webdav = cx.entity().clone();
        let view_edit_webdav = cx.entity().clone();
        let view_browse_baidu = cx.entity().clone();
        let view_edit_baidu = cx.entity().clone();
        let webdav_subtitle = if self.webdav.configured() {
            if self.webdav.dir.is_empty() {
                SharedString::from(self.webdav.url.clone())
            } else {
                format!("{} · {}", self.webdav.url, self.webdav.dir).into()
            }
        } else {
            tr(cx, "cloud.not_configured")
        };
        let baidu_subtitle = if self.baidu.configured() {
            match self.baidu.account.as_str() {
                "" => tr(cx, "cloud.token_saved"),
                account => account.to_string().into(),
            }
        } else {
            tr(cx, "cloud.not_configured")
        };

        v_flex()
            .id("cloud-drives")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .px_4()
            .py_3()
            .gap_2()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                Button::new("cloud-drive-webdav")
                                    .ghost()
                                    .w_full()
                                    .justify_start()
                                    .icon(IconName::HardDrive)
                                    .label(tr(cx, "cloud.webdav").to_string())
                                    .on_click(move |_, window, cx| {
                                        view_browse_webdav.update(cx, |this, cx| {
                                            this.browse(Kind::WebDav, window, cx)
                                        });
                                    }),
                            )
                            .child(muted_label(webdav_subtitle, cx)),
                    )
                    .child(
                        Button::new("cloud-webdav-edit")
                            .ghost()
                            .small()
                            .icon(IconName::NotebookPen)
                            .tooltip(tr(cx, "cloud.edit").to_string())
                            .accessibility_label(tr(cx, "cloud.edit"))
                            .on_click(move |_, window, cx| {
                                view_edit_webdav
                                    .update(cx, |this, cx| this.open_webdav_form(window, cx));
                            }),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                Button::new("cloud-drive-baidu")
                                    .ghost()
                                    .w_full()
                                    .justify_start()
                                    .icon(IconName::Cloud)
                                    .label(tr(cx, "cloud.baidu").to_string())
                                    .on_click(move |_, window, cx| {
                                        view_browse_baidu.update(cx, |this, cx| {
                                            this.browse(Kind::Baidu, window, cx)
                                        });
                                    }),
                            )
                            .child(muted_label(baidu_subtitle, cx)),
                    )
                    .child(
                        Button::new("cloud-baidu-edit")
                            .ghost()
                            .small()
                            .icon(IconName::NotebookPen)
                            .tooltip(tr(cx, "cloud.edit").to_string())
                            .accessibility_label(tr(cx, "cloud.edit"))
                            .on_click(move |_, window, cx| {
                                view_edit_baidu
                                    .update(cx, |this, cx| this.open_baidu_form(window, cx));
                            }),
                    ),
            )
            .child(muted_label(tr(cx, "cloud.drives_hint"), cx))
            .into_any_element()
    }

    fn webdav_form_body(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(form) = self.form.as_ref() else {
            return div().into_any_element();
        };
        let view_save = cx.entity().clone();
        let view_cancel = cx.entity().clone();
        let mut body = v_flex()
            .id("cloud-webdav-form")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .px_4()
            .py_4()
            .gap_3()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(FORM_WIDTH))
                    .gap_1()
                    .child(div().text_sm().child(tr(cx, "cloud.url")))
                    .child(Input::new(&form.url).w_full()),
            )
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(FORM_WIDTH))
                    .gap_1()
                    .child(div().text_sm().child(tr(cx, "cloud.username")))
                    .child(Input::new(&form.username).w_full()),
            )
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(FORM_WIDTH))
                    .gap_1()
                    .child(div().text_sm().child(tr(cx, "cloud.password")))
                    .child(Input::new(&form.password).w_full()),
            )
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(FORM_WIDTH))
                    .gap_1()
                    .child(div().text_sm().child(tr(cx, "cloud.dir")))
                    .child(muted_label(tr(cx, "cloud.dir_hint"), cx))
                    .child(Input::new(&form.dir).w_full()),
            );
        if let Some(error) = &form.error {
            body = body.child(muted_label(error.clone(), cx));
        }
        body.child(
            h_flex()
                .w_full()
                .max_w(px(FORM_WIDTH))
                .justify_end()
                .gap_2()
                .child(
                    Button::new("cloud-webdav-cancel")
                        .ghost()
                        .small()
                        .label(tr(cx, "cloud.cancel").to_string())
                        .on_click(move |_, _, cx| {
                            view_cancel.update(cx, |this, cx| this.back(cx));
                        }),
                )
                .child(
                    Button::new("cloud-webdav-save")
                        .small()
                        .label(tr(cx, "cloud.save").to_string())
                        .on_click(move |_, _, cx| {
                            view_save.update(cx, |this, cx| this.save_webdav(cx));
                        }),
                ),
        )
        .into_any_element()
    }

    fn baidu_form_body(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(form) = self.baidu_form.as_ref() else {
            return div().into_any_element();
        };
        let view_save = cx.entity().clone();
        let view_cancel = cx.entity().clone();
        // 授权那一路：应用凭据内置了才有得点 —— 没配就说一声，token 手贴那条路仍在。
        let auth_block = match baidu_auth::credentials() {
            Some((client_id, _)) => v_flex()
                .w_full()
                .max_w(px(FORM_WIDTH))
                .gap_1()
                .child(div().text_sm().child(tr(cx, "cloud.baidu_code")))
                .child(muted_label(tr(cx, "cloud.baidu_code_hint"), cx))
                .child(Input::new(&form.code).w_full())
                .child(
                    Button::new("cloud-baidu-open-page")
                        .outline()
                        .small()
                        .label(tr(cx, "cloud.baidu_open_page").to_string())
                        .disabled(form.linking)
                        .on_click(move |_, _, _cx| {
                            crate::ui::platform::open_url(&baidu_auth::authorize_url(client_id));
                        }),
                ),
            None => v_flex()
                .w_full()
                .max_w(px(FORM_WIDTH))
                .gap_1()
                .child(muted_label(tr(cx, "cloud.baidu_no_credentials"), cx)),
        };
        let mut body = v_flex()
            .id("cloud-baidu-form")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .px_4()
            .py_4()
            .gap_3()
            .child(auth_block)
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(FORM_WIDTH))
                    .gap_1()
                    .child(div().text_sm().child(tr(cx, "cloud.token")))
                    .child(muted_label(tr(cx, "cloud.token_hint"), cx))
                    .child(muted_label(tr(cx, "cloud.token_expiry"), cx))
                    .child(Input::new(&form.token).w_full()),
            );
        if let Some(error) = &form.error {
            body = body.child(muted_label(error.clone(), cx));
        }
        body.child(
            h_flex()
                .w_full()
                .max_w(px(FORM_WIDTH))
                .justify_end()
                .gap_2()
                .child(
                    Button::new("cloud-baidu-cancel")
                        .ghost()
                        .small()
                        .label(tr(cx, "cloud.cancel").to_string())
                        .on_click(move |_, _, cx| {
                            view_cancel.update(cx, |this, cx| this.back(cx));
                        }),
                )
                .child(
                    Button::new("cloud-baidu-save")
                        .small()
                        .label(if form.linking {
                            tr(cx, "cloud.baidu_linking")
                        } else {
                            tr(cx, "cloud.save")
                        })
                        .disabled(form.linking)
                        .on_click(move |_, _, cx| {
                            view_save.update(cx, |this, cx| this.save_baidu(cx));
                        }),
                ),
        )
        .into_any_element()
    }

    fn browse_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().clone();
        let view_download = cx.entity().clone();

        let mut body = v_flex().flex_1().min_h_0().w_full();
        if let Some(importing) = &self.importing {
            body = body.child(
                h_flex()
                    .w_full()
                    .px_4()
                    .py_2()
                    .child(muted_label(importing.message.clone(), cx)),
            );
        }

        if self.loading && self.entries.is_empty() {
            return body
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .child(muted_label(tr(cx, "cloud.loading"), cx)),
                )
                .into_any_element();
        }
        if let Some(error) = self.error {
            return body
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .child(div().text_base().child(failure_text(error, cx)))
                        .child(
                            Button::new("cloud-retry")
                                .outline()
                                .small()
                                .label(tr(cx, "cloud.retry").to_string())
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| this.load_dir(cx));
                                }),
                        ),
                )
                .into_any_element();
        }

        body = body.child(self.breadcrumb(cx));

        if self.entries.is_empty() {
            return body
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .child(div().text_base().child(tr(cx, "cloud.empty_dir"))),
                )
                .into_any_element();
        }

        let mut list = v_flex()
            .id("cloud-entries")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .px_4()
            .py_3()
            .gap_1();
        for entry in &self.entries {
            if entry.is_dir {
                list = list.child(self.directory_row(entry, cx));
            } else {
                list = list.child(self.file_row(entry, cx));
            }
        }
        let count = self.selected.len();
        let busy = self.importing.as_ref().is_some_and(|state| !state.done);
        body.child(list)
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_2()
                    .child(
                        Button::new("cloud-download")
                            .small()
                            .icon(IconName::Download)
                            .label(tplural(cx, "cloud.download", count))
                            .disabled(count == 0 || busy)
                            .on_click(move |_, _, cx| {
                                view_download.update(cx, |this, cx| this.start_download(cx));
                            }),
                    ),
            )
            .into_any_element()
    }

    /// 面包屑：根 + 逐级目录，点哪级回哪级。当前那级按不动。
    fn breadcrumb(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().clone();
        let mut row = h_flex()
            .w_full()
            .items_center()
            .flex_wrap()
            .px_4()
            .py_1()
            .gap_1()
            .child(
                Button::new("cloud-crumb-root")
                    .ghost()
                    .xsmall()
                    .label(tr(cx, "cloud.root").to_string())
                    .disabled(self.path.is_empty())
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.path = String::new();
                            this.selected.clear();
                            this.load_dir(cx);
                        });
                    }),
            );
        let walked = self.path.split('/').filter(|s| !s.is_empty());
        let mut prefix = String::new();
        for (index, segment) in walked.enumerate() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(segment);
            let here = prefix == self.path;
            let view = cx.entity().clone();
            let jump = prefix.clone();
            row = row
                .child(
                    Icon::new(IconName::ChevronRight)
                        .small()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(
                    Button::new(format!("cloud-crumb-{index}"))
                        .ghost()
                        .xsmall()
                        .label(segment.to_string())
                        .disabled(here)
                        .on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.path = jump.clone();
                                this.selected.clear();
                                this.load_dir(cx);
                            });
                        }),
                );
        }
        row.into_any_element()
    }

    fn directory_row(&self, entry: &Entry, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().clone();
        let next = entry.path.clone();
        Button::new(format!("cloud-dir-{}", entry.path))
            .ghost()
            .w_full()
            .justify_start()
            .icon(IconName::FolderOpen)
            .label(entry.name.clone())
            .on_click(move |_, _, cx| {
                view.update(cx, |this, cx| this.open_entry(next.clone(), cx));
            })
            .into_any_element()
    }

    fn file_row(&self, entry: &Entry, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().clone();
        let path = entry.path.clone();
        let checked = self.selected.contains(&path);
        let size = size_label(entry.size);
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Checkbox::new(format!("cloud-check-{}", entry.path))
                    .checked(checked)
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| this.toggle(path.clone(), cx));
                    }),
            )
            .child(
                Icon::new(IconName::File)
                    .small()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_ellipsis()
                    .child(entry.name.clone()),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(size),
            )
            .into_any_element()
    }
}
