//! 网盘取书：一页里的分段浏览器 —— 顶上一排网盘胶囊，选中那台的配置表单或目录卡
//! 直接长在下面，从架面「⋮」菜单压上导航栈。
//!
//! 一页只管一件事：把 core 读出来的目录清单摆出来 —— 目录是一行点进去，文件是一行
//! 勾上。下载走现有导入管线（[`CloudSource`] 挂着当时的账号现造一台
//! [`Drive`](folio_core::net::cloud::Drive)），所以判重、哈希、报表与书架刷新都复用
//! 同一套。
//!
//! 换一台网盘只换下面那一块，不再压第二屏：配置好的直接是目录卡，没配好的是配置表单；
//! 保存落定后目录卡自动出现。翻子目录也只换卡里的内容，顶栏的返回在子目录里回上一级、
//! 到了根再弹掉整页 —— 与 OPDS 浏览器同一条规矩，一层字符串路径就够了。

use std::collections::HashSet;

use folio_core::net::cloud::aliyun_auth;
use folio_core::net::cloud::baidu_auth;
use folio_core::net::cloud::{CloudProvider, Drive, Entry};
use folio_core::net::remote::Error as RemoteError;
use folio_core::{
    CloudItem, CloudSource, ImportOutcome, ImportReport, TaskEvent, WebDavDrive, submit_import,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::{NavMotion, NavStackState};
use gpui_kit::component::breadcrumb::{Breadcrumb, BreadcrumbItem};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::empty::{Empty, EmptyContent, EmptyHeader, EmptyMedia, EmptyTitle};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    TestSupportExt as _, WeakEntity, Window, div, px,
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
enum Failure {
    Auth,
    Network,
    Remote,
}

/// 网盘有哪几台。要标「这是谁交的」时由那台 [`Drive::label`] 自报，
/// 这里只负责分流账号表单与客户端构造。
///
/// **行为**全部问 [`CloudProvider`]（见 [`Kind::provider`]）：配没配好、浏览起点、
/// 怎么造客户端，都在 `services::cloud_providers` 的那张表上 —— 这个枚举剩下的
/// 职责只有**界面路由**（没配好带去哪张表单，那是逐家不同的屏）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    WebDav,
    Baidu,
    Aliyun,
}

impl Kind {
    /// 这一台的 [`CloudProvider`]（`services::cloud_providers` 表里的静态条目）。
    fn provider(self) -> &'static dyn CloudProvider {
        match self {
            Kind::WebDav => &crate::ui::services::cloud_providers::WEBDAV,
            Kind::Baidu => &crate::ui::services::cloud_providers::BAIDU,
            Kind::Aliyun => &crate::ui::services::cloud_providers::ALIYUN,
        }
    }

    /// 元素 id 用的那截短名（`cloud-{slug}-edit` / `cloud-{slug}-enter`）。
    /// 三台的按钮 id 原本就是这些串，改了名字测试与无障碍得跟着找一遍。
    fn slug(self) -> &'static str {
        match self {
            Kind::WebDav => "webdav",
            Kind::Baidu => "baidu",
            Kind::Aliyun => "aliyun",
        }
    }
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

/// 阿里云盘的账号表单。
///
/// 与百度那张相比**没有 `drive_id` 那一格**：读者不该知道那是什么。手贴 token 的
/// 老路在保存时顺手问一次 `getDriveInfo` 把 `drive_id` 补上（一次网络往返，走后台），
/// 授权那条路本来就会带回来。
struct AliyunForm {
    token: Entity<InputState>,
    code: Entity<InputState>,
    linking: bool,
    error: Option<SharedString>,
}

struct ImportState {
    message: SharedString,
    done: bool,
}

pub struct CloudView {
    stack: WeakEntity<NavStackState>,
    webdav: WebDavDrive,
    baidu: folio_core::BaiduDrive,
    aliyun: folio_core::AliyunDrive,
    form: Option<WebDavForm>,
    baidu_form: Option<BaiduForm>,
    aliyun_form: Option<AliyunForm>,
    /// 胶囊选中的那台。开屏就停在 WebDAV —— 读者进这一页要办的第一件事，
    /// 多数是配一台能连上的盘，而不是先挑一家。
    kind: Kind,
    /// 正在改一台**已经配好**的网盘。未配好的那台永远是表单，不看这一格；
    /// 配好的平时是目录卡，按卡头那颗笔才翻成表单。
    editing: bool,
    /// 第一帧补一次「选中那台」：配好就列目录，没配好就开表单。渲染时才做，
    /// 因为开表单要 `&mut Window`，而 `new` 手里没有。
    started: bool,
    /// 当前目录（Drive 的相对路径语义，空 = 根）。
    path: String,
    entries: Vec<Entry>,
    selected: HashSet<String>,
    loading: bool,
    error: Option<Failure>,
    importing: Option<ImportState>,
    /// 列目录的序号：切盘 / 连点目录时，回来晚的那一发不许覆盖新目录。
    load_token: u64,
    /// 深链回跳带回来、还没填进表单的授权码（见 [`crate::ui::services::cloud_link`]）。
    ///
    /// 不直接写输入框：写它要窗口，而回跳发生在 JNI 线程上。这里只排队，等下一次
    /// 有窗口的渲染帧取用（[`Self::take_pending_aliyun_code`]）。
    pending_aliyun_code: Option<String>,
}

impl EventEmitter<CloudEvent> for CloudView {}

impl CloudView {
    pub fn new(stack: WeakEntity<NavStackState>, cx: &mut Context<Self>) -> Self {
        let cloud = ConfigHolder::cloud(cx);
        Self {
            stack,
            webdav: cloud.webdav,
            baidu: cloud.baidu,
            aliyun: cloud.aliyun,
            form: None,
            baidu_form: None,
            aliyun_form: None,
            kind: Kind::WebDav,
            editing: false,
            started: false,
            path: String::new(),
            entries: Vec::new(),
            selected: HashSet::new(),
            loading: false,
            error: None,
            importing: None,
            load_token: 0,
            pending_aliyun_code: None,
        }
    }

    /// 胶囊点了另一台：页面不动，只把下面那一块换掉 —— 配好的直接列目录，
    /// 没配好的就是它的配置表单。选与看是一步，中间不再压第二屏。
    fn select(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        if kind == self.kind {
            return;
        }
        self.kind = kind;
        self.refresh(window, cx);
    }

    /// 把「选中那台」的下面那一块摆正确：配置好的是目录，没配好的是表单。
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.entries.clear();
        self.selected.clear();
        self.error = None;
        self.importing = None;
        let cloud = ConfigHolder::cloud(cx);
        if self.kind.provider().configured(&cloud) {
            self.clear_forms();
            self.editing = false;
            self.path = self.kind.provider().default_dir(&cloud);
            self.load_dir(cx);
        } else {
            self.editing = true;
            self.path.clear();
            self.open_drive_form(self.kind, window, cx);
        }
    }

    fn clear_forms(&mut self) {
        self.form = None;
        self.baidu_form = None;
        self.aliyun_form = None;
    }

    /// 下面那一块是表单还是目录：没配好的一定是表单；配好的只在按了卡头那支笔时是表单。
    fn showing_form(&self, cx: &App) -> bool {
        !self.kind.provider().configured(&ConfigHolder::cloud(cx)) || self.editing
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        let configured = self.kind.provider().configured(&ConfigHolder::cloud(cx));
        // 看的是目录、又按了笔：返回先收起表单，别把整页弹掉。
        if self.editing && configured {
            self.editing = false;
            self.clear_forms();
            cx.notify();
            return;
        }
        if !self.editing {
            if let Some((parent, _)) = self.path.rsplit_once('/') {
                self.path = parent.to_string();
                self.selected.clear();
                self.load_dir(cx);
                return;
            }
            if !self.path.is_empty() {
                self.path = String::new();
                self.selected.clear();
                self.load_dir(cx);
                return;
            }
        }
        // 没配好的那台只剩表单，返回就是离开这一页。
        let _ = self
            .stack
            .update(cx, |stack, cx| stack.pop(NavMotion::Animated, cx));
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
        self.after_save(cx);
    }

    /// 一次保存落定：收起表单，按新配置从默认目录开始列 —— 读者不用再点一下
    /// 「浏览」，目录卡就在下面长出来。
    fn after_save(&mut self, cx: &mut Context<Self>) {
        self.editing = false;
        self.clear_forms();
        self.entries.clear();
        self.selected.clear();
        self.error = None;
        let cloud = ConfigHolder::cloud(cx);
        self.path = self.kind.provider().default_dir(&cloud);
        self.load_dir(cx);
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
        self.after_save(cx);
    }

    fn open_aliyun_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let token = cx
            .new(|cx| InputState::new(window, cx).default_value(self.aliyun.access_token.clone()));
        // 回跳可能已经先到了（读者先点了授权、再回到这一屏）：开表单时就把它填上。
        let pending = self.pending_aliyun_code.take();
        let code = cx.new(|cx| InputState::new(window, cx));
        if let Some(code_value) = pending {
            code.update(cx, |state, cx| state.set_value(code_value, window, cx));
        }
        self.aliyun_form = Some(AliyunForm {
            token,
            code,
            linking: false,
            error: None,
        });
        cx.notify();
    }

    /// 把深链回跳带回来的授权码取出来，填进已经开着的阿里云盘表单。
    ///
    /// 在 `render` 里调 —— 那一帧同时握着窗口与 `&mut self`，而
    /// `InputState::set_value` 要窗口。回跳本身发生在 JNI 线程上，那一刻既没有窗口
    /// 也没有 `App`，所以那里只排队（见 [`crate::ui::services::cloud_link`]），
    /// 到这一帧才落地。
    ///
    /// 表单没开着就先留着：读者点「编辑」开表单时由 [`Self::open_aliyun_form`] 填。
    fn take_pending_aliyun_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for url in crate::ui::services::cloud_link::drain() {
            let Some(code) = crate::ui::services::cloud_link::parse_code(&url) else {
                tracing::debug!(
                    "cloud: 收到一条不是网盘授权的深链，忽略：{}",
                    folio_core::net::remote::redact_url(&url)
                );
                continue;
            };
            tracing::info!("cloud: 阿里云盘授权的回跳到了");
            if self.kind != Kind::Aliyun || !self.showing_form(cx) || self.aliyun_form.is_none() {
                // 表单没开着：先存着，等它开的时候填进去。
                self.pending_aliyun_code = Some(code);
                continue;
            }
            if let Some(form) = self.aliyun_form.as_ref() {
                form.code
                    .update(cx, |state, cx| state.set_value(code, window, cx));
            }
            cx.notify();
        }
    }

    fn save_aliyun(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.aliyun_form.as_ref() else {
            return;
        };
        if form.linking {
            return;
        }
        let code = form.code.read(cx).value().trim().to_string();
        let token = form.token.read(cx).value().trim().to_string();
        if !code.is_empty() {
            self.link_aliyun(code, cx);
            return;
        }
        if token.is_empty() {
            if let Some(form) = self.aliyun_form.as_mut() {
                form.error = Some(tr(cx, "cloud.token_missing"));
            }
            cx.notify();
            return;
        }
        // 手贴 token：`drive_id` 缺着请求必被拒，所以顺手问一次 getDriveInfo 补上。
        // 这一趟要出网，走后台；问不到就报错 —— 存一份「有 token 但用不了」的配置
        // 比让读者现在知道不行更糟。
        self.probe_aliyun(token, cx);
    }

    /// 手贴 token 那一路：问一次 `getDriveInfo` 换 `drive_id` 与账号名。
    fn probe_aliyun(&mut self, token: String, cx: &mut Context<Self>) {
        if let Some(form) = self.aliyun_form.as_mut() {
            form.linking = true;
            form.error = None;
        }
        cx.notify();
        let ask = token.clone();
        let work = cx.background_spawn(async move {
            aliyun_auth::drive_info(&ask, crate::ui::services::update::transport())
        });
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_this: WeakEntity<CloudView>, cx| {
            let result = work.await;
            let _ = weak.update(cx, |this, cx| {
                this.settle_probe_aliyun(token, result, cx);
            });
        })
        .detach();
    }

    fn settle_probe_aliyun(
        &mut self,
        token: String,
        result: Result<aliyun_auth::DriveInfo, RemoteError>,
        cx: &mut Context<Self>,
    ) {
        let info = match result {
            Ok(info) => info,
            Err(error) => {
                tracing::warn!("cloud: 阿里云盘 getDriveInfo 失败：{error}");
                if let Some(form) = self.aliyun_form.as_mut() {
                    form.linking = false;
                    form.error = Some(failure_text(failure_of(&error), cx));
                }
                cx.notify();
                return;
            }
        };
        let account = aliyun_account_of(&info);
        let aliyun = folio_core::AliyunDrive {
            access_token: token,
            drive_id: info.drive_id,
            account,
            ..folio_core::AliyunDrive::default()
        };
        ConfigHolder::edit_cloud(cx, |cloud| cloud.aliyun = aliyun.clone());
        self.aliyun = aliyun;
        // 等 token 的这段时间读者可能已经切去另一台：配置照存，但别把当前那一块
        // 换成阿里云的目录。
        if self.kind == Kind::Aliyun {
            self.after_save(cx);
        } else {
            cx.notify();
        }
    }

    /// 授权码那一路：换 token（带回 `drive_id`）→ 问账号名 → 落盘 → 列目录。
    fn link_aliyun(&mut self, code: String, cx: &mut Context<Self>) {
        if let Some(form) = self.aliyun_form.as_mut() {
            form.linking = true;
            form.error = None;
        }
        cx.notify();
        let work = cx.background_spawn(async move {
            aliyun_auth::exchange_code(&code, crate::ui::services::update::transport()).map(
                |tokens| {
                    let info = aliyun_auth::drive_info(
                        &tokens.access_token,
                        crate::ui::services::update::transport(),
                    );
                    (tokens, info.ok())
                },
            )
        });
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_this: WeakEntity<CloudView>, cx| {
            let result = work.await;
            let _ = weak.update(cx, |this, cx| this.settle_link_aliyun(result, cx));
        })
        .detach();
    }

    fn settle_link_aliyun(
        &mut self,
        result: Result<(aliyun_auth::AliyunTokens, Option<aliyun_auth::DriveInfo>), RemoteError>,
        cx: &mut Context<Self>,
    ) {
        let (tokens, info) = match result {
            Ok(pair) => pair,
            Err(error) => {
                tracing::warn!("cloud: 阿里云盘授权换 token 失败：{error}");
                if let Some(form) = self.aliyun_form.as_mut() {
                    form.linking = false;
                    form.error = Some(failure_text(failure_of(&error), cx));
                }
                cx.notify();
                return;
            }
        };
        // `drive_id` 优先用 getDriveInfo 那一份（刷新应答里可能没有），
        // 再退 token 那一步带回来的。两份都空就是真拿不到，存下去也用不了 ——
        // 但此时 token 是真的换到了，存着让读者下一次重开还能补问一遍。
        let drive_id = info
            .as_ref()
            .map(|info| info.drive_id.clone())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| tokens.drive_id.clone());
        let account = info
            .as_ref()
            .map(aliyun_account_of)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.aliyun.account.clone());
        let aliyun = linked_aliyun(&tokens, drive_id, account);
        ConfigHolder::edit_cloud(cx, |cloud| cloud.aliyun = aliyun.clone());
        self.aliyun = aliyun;
        if self.kind == Kind::Aliyun {
            self.after_save(cx);
        } else {
            cx.notify();
        }
    }

    /// 授权码那一路：换 token（网络）→ 问账号名 → 落盘 → 列目录。
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
        if self.kind == Kind::Baidu {
            self.after_save(cx);
        } else {
            cx.notify();
        }
    }

    fn load_dir(&mut self, cx: &mut Context<Self>) {
        let Some(drive) = build_drive(self.kind, cx) else {
            // 账号半路被清掉（例如在别处清了 token）：下一帧重新分流，落回表单，
            // 不给一屏空列表。
            self.entries.clear();
            self.selected.clear();
            self.loading = false;
            self.error = None;
            self.started = false;
            cx.notify();
            return;
        };
        let path = self.path.clone();
        self.loading = true;
        self.error = None;
        self.load_token = self.load_token.wrapping_add(1);
        let token = self.load_token;
        cx.notify();

        let work = cx.background_spawn(async move { drive.list(&path) });
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_this: WeakEntity<CloudView>, cx| {
            let result = work.await;
            let _ = weak.update(cx, |this, cx| this.settle_dir(token, result, cx));
        })
        .detach();
    }

    fn settle_dir(
        &mut self,
        token: u64,
        result: Result<Vec<Entry>, RemoteError>,
        cx: &mut Context<Self>,
    ) {
        // 切了盘或连点目录：这一发回来得比新的晚，丢掉。否则旧盘的清单会盖在新盘上。
        if token != self.load_token {
            return;
        }
        self.loading = false;
        match result {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
            }
            Err(error) => {
                tracing::warn!("cloud: 列 {:?} 失败：{error}", self.path);
                // 认证被拒在这里就是终态：`AuthedStore` 已经在请求层续过一次、重放过
                // 一次（见 `services::cloud_auth`），走到这一步说明 refresh_token 也
                // 不行了 —— 界面上只剩「请重新授权」这一个动作。
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
/// 按当时的配置现造一台网盘客户端。凭据只在这一批活里活着 —— 改了账号、清了
/// token，旧的那台随对象一起消失。
///
/// 「怎么造」全在 [`CloudProvider`] 的实现里（`services::cloud_providers`）：两家网盘
/// 套着凭据续期（`AuthedStore`），过期先续、401 强刷重放一次；手贴 token（没有
/// refresh_token）的那一路同样从那里过 —— 续不了就把原来的 401 交回界面，读者重贴。
/// 这一层不再按家写 `match`。
fn build_drive(kind: Kind, cx: &App) -> Option<Box<dyn Drive>> {
    let cloud = ConfigHolder::cloud(cx);
    kind.provider()
        .build_drive(&cloud, crate::ui::services::update::transport())
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

/// 换到 / 刷新到的一对 token → 阿里云盘的落盘形状。
///
/// `drive_id` 由调用方挑好（授权时来自 `getDriveInfo`，刷新时沿用已存的那一份）——
/// 这一格不是 token 的附赠品，它是每个文件请求都要带的一个参数。
fn linked_aliyun(
    tokens: &aliyun_auth::AliyunTokens,
    drive_id: String,
    account: String,
) -> folio_core::AliyunDrive {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    folio_core::AliyunDrive {
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone(),
        expires_at: now + tokens.expires_in.max(0),
        drive_id,
        account,
    }
}

/// `getDriveInfo` 的昵称：昵称 > 用户名 > 手机号，与 [`aliyun_auth::account_name`]
/// 同一套优先级 —— 两处各写一份就会一边空一边不空。
fn aliyun_account_of(info: &aliyun_auth::DriveInfo) -> String {
    if !info.nick_name.trim().is_empty() {
        info.nick_name.clone()
    } else if !info.user_name.trim().is_empty() {
        info.user_name.clone()
    } else {
        info.phone.clone()
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
        // 深链回跳的授权码：只有这一帧同时握着窗口与 `&mut self`，所以在这一格里
        // 把它填进表单（见 `take_pending_aliyun_code`）。一次性 —— 队列取完就空。
        self.take_pending_aliyun_code(window, cx);
        let view = cx.entity().clone();
        let top = TopBar::new("cloud-top-bar")
            .title(tr(cx, "cloud.title"))
            .back_tooltip(tr(cx, "shelf.back"))
            .on_back(move |_, cx| {
                view.update(cx, |this, cx| this.back(cx));
            });

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(top)
            .child(self.drives_body(window, cx))
    }
}

impl CloudView {
    fn drives_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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
        // 第一帧补一次：开屏就停在 WebDAV —— 配好的是目录，没配好的是它的表单。
        // 放在渲染里是因为开表单要窗口，而 `new` 手里没有。
        if !self.started {
            self.started = true;
            self.refresh(window, cx);
        }
        // 顶部那颗胶囊**照「同步」页的分段胶囊**（`settings/pages/sync.rs` 的
        // `provider_switch`）：`muted` 底的全圆角槽里三段等宽，选中那段用主色填实。
        // 点哪段只换下面那一块，不压第二屏 —— 配置表与目录卡都长在同一个位置。
        let pill = {
            let mut row = h_flex()
                .w_full()
                .items_center()
                .gap_1()
                .p_0p5()
                .rounded_full()
                .bg(cx.theme().muted);
            for kind in [Kind::WebDav, Kind::Baidu, Kind::Aliyun] {
                let on = self.kind == kind;
                let label = match kind {
                    Kind::WebDav => tr(cx, "cloud.webdav"),
                    Kind::Baidu => tr(cx, "cloud.baidu"),
                    Kind::Aliyun => tr(cx, "cloud.aliyun"),
                };
                let entity = cx.entity().clone();
                let mut segment = div()
                    .id(format!("cloud-drive-{}", kind.slug()))
                    .test_support()
                    .flex_1()
                    .px_3()
                    .py_2()
                    .rounded_full()
                    .text_sm()
                    .text_center()
                    .aria_selected(on)
                    .aria_label(label.clone())
                    .child(label);
                segment = if on {
                    segment
                        .bg(cx.theme().primary)
                        .text_color(cx.theme().primary_foreground)
                } else {
                    segment.text_color(cx.theme().muted_foreground)
                };
                row = row.child(segment.on_click(move |_, window, cx| {
                    entity.update(cx, |this, cx| this.select(kind, window, cx));
                }));
            }
            row
        };

        let content = if self.showing_form(cx) {
            match self.kind {
                Kind::WebDav => self.webdav_form_body(window, cx),
                Kind::Baidu => self.baidu_form_body(window, cx),
                Kind::Aliyun => self.aliyun_form_body(window, cx),
            }
        } else {
            self.directory_card(cx)
        };

        v_flex()
            .id("cloud-drives")
            .flex_1()
            .min_h_0()
            .w_full()
            .px_4()
            .py_3()
            .gap_3()
            .child(pill)
            .when(self.showing_form(cx), |this| {
                this.child(muted_label(tr(cx, "cloud.drives_hint"), cx))
            })
            .child(content)
            .into_any_element()
    }

    /// 这一台的一句现状：地址·目录、账号名，或「未配置」。目录卡卡头那句副题
    /// 念的就是它：正在谁的盘里翻。
    fn drive_subtitle(&self, kind: Kind, cx: &App) -> SharedString {
        match kind {
            Kind::WebDav => {
                if self.webdav.configured() {
                    if self.webdav.dir.is_empty() {
                        SharedString::from(self.webdav.url.clone())
                    } else {
                        format!("{} · {}", self.webdav.url, self.webdav.dir).into()
                    }
                } else {
                    tr(cx, "cloud.not_configured")
                }
            }
            Kind::Baidu => {
                if self.baidu.configured() {
                    match self.baidu.account.as_str() {
                        "" => tr(cx, "cloud.token_saved"),
                        account => account.to_string().into(),
                    }
                } else {
                    tr(cx, "cloud.not_configured")
                }
            }
            Kind::Aliyun => {
                if self.aliyun.configured() {
                    match self.aliyun.account.as_str() {
                        "" => tr(cx, "cloud.token_saved"),
                        account => account.to_string().into(),
                    }
                } else {
                    tr(cx, "cloud.not_configured")
                }
            }
        }
    }

    /// 带去某一台的账号表单。原先是三颗编辑按钮各写一个 `match` 臂，收成一格 ——
    /// 判据（哪台）与去处（哪张表单）在这里只会同时用到一次。
    fn open_drive_form(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        match kind {
            Kind::WebDav => self.open_webdav_form(window, cx),
            Kind::Baidu => self.open_baidu_form(window, cx),
            Kind::Aliyun => self.open_aliyun_form(window, cx),
        }
    }

    fn webdav_form_body(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(form) = self.form.as_ref() else {
            return div().into_any_element();
        };
        let view_save = cx.entity().clone();
        let view_cancel = cx.entity().clone();
        // 已经配好的那台是从目录卡按笔进来的：给一颗「取消」退回目录；
        // 第一次配的那台没有可退的地方，只留「保存」。
        let configured = self.kind.provider().configured(&ConfigHolder::cloud(cx));
        let mut body = v_flex()
            .id("cloud-webdav-form")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().tokens.group_box)
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
        let mut actions = h_flex()
            .w_full()
            .max_w(px(FORM_WIDTH))
            .justify_end()
            .gap_2();
        if configured {
            actions = actions.child(
                Button::new("cloud-webdav-cancel")
                    .ghost()
                    .small()
                    .label(tr(cx, "cloud.cancel").to_string())
                    .on_click(move |_, _, cx| {
                        view_cancel.update(cx, |this, cx| this.back(cx));
                    }),
            );
        }
        body.child(
            actions.child(
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
        let configured = self.kind.provider().configured(&ConfigHolder::cloud(cx));
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
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().tokens.group_box)
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
        let mut actions = h_flex()
            .w_full()
            .max_w(px(FORM_WIDTH))
            .justify_end()
            .gap_2();
        if configured {
            actions = actions.child(
                Button::new("cloud-baidu-cancel")
                    .ghost()
                    .small()
                    .label(tr(cx, "cloud.cancel").to_string())
                    .on_click(move |_, _, cx| {
                        view_cancel.update(cx, |this, cx| this.back(cx));
                    }),
            );
        }
        body.child(
            actions.child(
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

    /// 阿里云盘的账号表单。
    ///
    /// 与百度那张同形，差别只在一处提示：阿里云盘的授权**必须**回跳到应用登记的
    /// 那个地址，授权页不会像百度那样直接把 code 摆在页面上。所以这一格除了
    /// 「打开授权页」，还写清「授权完回到应用，码会自动填进来 / 也可以手贴」。
    fn aliyun_form_body(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(form) = self.aliyun_form.as_ref() else {
            return div().into_any_element();
        };
        let view_save = cx.entity().clone();
        let view_cancel = cx.entity().clone();
        let configured = self.kind.provider().configured(&ConfigHolder::cloud(cx));
        let auth_block = match aliyun_auth::credentials() {
            Some((client_id, _)) => v_flex()
                .w_full()
                .max_w(px(FORM_WIDTH))
                .gap_1()
                .child(div().text_sm().child(tr(cx, "cloud.aliyun_code")))
                .child(muted_label(tr(cx, "cloud.aliyun_code_hint"), cx))
                .child(Input::new(&form.code).w_full())
                .child(
                    Button::new("cloud-aliyun-open-page")
                        .outline()
                        .small()
                        .label(tr(cx, "cloud.aliyun_open_page").to_string())
                        .disabled(form.linking)
                        .on_click(move |_, _, _cx| {
                            crate::ui::platform::open_url(&aliyun_auth::authorize_url(client_id));
                        }),
                ),
            None => v_flex()
                .w_full()
                .max_w(px(FORM_WIDTH))
                .gap_1()
                .child(muted_label(tr(cx, "cloud.aliyun_no_credentials"), cx)),
        };
        let mut body = v_flex()
            .id("cloud-aliyun-form")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().tokens.group_box)
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
                    .child(muted_label(tr(cx, "cloud.aliyun_token_hint"), cx))
                    .child(Input::new(&form.token).w_full()),
            );
        if let Some(error) = &form.error {
            body = body.child(muted_label(error.clone(), cx));
        }
        let mut actions = h_flex()
            .w_full()
            .max_w(px(FORM_WIDTH))
            .justify_end()
            .gap_2();
        if configured {
            actions = actions.child(
                Button::new("cloud-aliyun-cancel")
                    .ghost()
                    .small()
                    .label(tr(cx, "cloud.cancel").to_string())
                    .on_click(move |_, _, cx| {
                        view_cancel.update(cx, |this, cx| this.back(cx));
                    }),
            );
        }
        body.child(
            actions.child(
                Button::new("cloud-aliyun-save")
                    .small()
                    .label(if form.linking {
                        tr(cx, "cloud.aliyun_linking")
                    } else {
                        tr(cx, "cloud.save")
                    })
                    .disabled(form.linking)
                    .on_click(move |_, _, cx| {
                        view_save.update(cx, |this, cx| this.save_aliyun(cx));
                    }),
            ),
        )
        .into_any_element()
    }

    /// 卡片头：一枚云图标、这一台的名字与账号/地址，右侧可选一支「改配置」的笔。
    /// 配置卡与目录卡共用同一块头 —— 换台时变的是下面的内容，不是这张卡的构造。
    fn panel_header(&self, with_edit: bool, cx: &mut Context<Self>) -> AnyElement {
        let label = match self.kind {
            Kind::WebDav => tr(cx, "cloud.webdav"),
            Kind::Baidu => tr(cx, "cloud.baidu"),
            Kind::Aliyun => tr(cx, "cloud.aliyun"),
        };
        let mut header = h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .px_4()
            .pt_4()
            .pb_3()
            .child(
                div()
                    .size_8()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted)
                    .child(
                        Icon::new(IconName::Cloud)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().text_sm().font_semibold().child(label))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .text_ellipsis()
                            .child(self.drive_subtitle(self.kind, cx)),
                    ),
            );
        if with_edit {
            let view = cx.entity().clone();
            header = header.child(
                Button::new(format!("cloud-edit-{}", self.kind.slug()))
                    .ghost()
                    .small()
                    .icon(IconName::NotebookPen)
                    .tooltip(tr(cx, "cloud.edit").to_string())
                    .accessibility_label(tr(cx, "cloud.edit"))
                    .on_click(move |_, window, cx| {
                        view.update(cx, |this, cx| this.begin_edit(window, cx));
                    }),
            );
        }
        header.into_any_element()
    }

    /// 选中那台的目录卡：卡头是这台盘与它的账号、一支改配置的笔；卡身是当前目录
    /// 的清单（面包屑 + 条目）；卡底是取书状态与下载钮。
    ///
    /// 不再压第二屏：目录就在胶囊下面长出来，翻子目录也只换这张卡里的内容。
    fn directory_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let view_retry = cx.entity().clone();
        let view_download = cx.entity().clone();
        let header = self.panel_header(true, cx);

        let mut inner = v_flex().flex_1().min_h_0().w_full();
        if self.loading && self.entries.is_empty() {
            inner = inner.child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .py_8()
                    .child(Spinner::new().large().color(cx.theme().muted_foreground))
                    .child(muted_label(tr(cx, "cloud.loading"), cx)),
            );
        } else if let Some(error) = self.error {
            inner = inner.child(
                Empty::new()
                    .border_0()
                    .p_4()
                    .header(
                        EmptyHeader::new()
                            .media(
                                EmptyMedia::new().child(
                                    Icon::new(IconName::CloudOff)
                                        .large()
                                        .text_color(cx.theme().muted_foreground),
                                ),
                            )
                            .title(EmptyTitle::new().child(failure_text(error, cx))),
                    )
                    .content(
                        EmptyContent::new().child(
                            Button::new("cloud-retry")
                                .outline()
                                .small()
                                .label(tr(cx, "cloud.retry").to_string())
                                .on_click(move |_, _, cx| {
                                    view_retry.update(cx, |this, cx| this.load_dir(cx));
                                }),
                        ),
                    ),
            );
        } else if self.entries.is_empty() {
            inner = inner.child(
                Empty::new().border_0().p_4().header(
                    EmptyHeader::new()
                        .media(
                            EmptyMedia::new().child(
                                Icon::new(IconName::FolderOpen)
                                    .large()
                                    .text_color(cx.theme().muted_foreground),
                            ),
                        )
                        .title(EmptyTitle::new().child(tr(cx, "cloud.empty_dir"))),
                ),
            );
        } else {
            let mut list = v_flex()
                .id("cloud-entries")
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scroll()
                .px_4()
                .py_2()
                .gap_1();
            for entry in &self.entries {
                if entry.is_dir {
                    list = list.child(self.directory_row(entry, cx));
                } else {
                    list = list.child(self.file_row(entry, cx));
                }
            }
            inner = inner.child(list);
        }

        let count = self.selected.len();
        let busy = self.importing.as_ref().is_some_and(|state| !state.done);
        let mut footer = h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(cx.theme().border);
        if let Some(importing) = &self.importing {
            footer = footer.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(muted_label(importing.message.clone(), cx)),
            );
        } else {
            footer = footer.child(div().flex_1());
        }
        footer = footer.child(
            Button::new("cloud-download")
                .primary()
                .small()
                .icon(IconName::CloudDownload)
                .label(tplural(cx, "cloud.download", count))
                .disabled(count == 0 || busy)
                .on_click(move |_, _, cx| {
                    view_download.update(cx, |this, cx| this.start_download(cx));
                }),
        );

        let mut card = v_flex()
            .id("cloud-directory-card")
            .flex_1()
            .min_h_0()
            .w_full()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().tokens.group_box)
            .child(header)
            .child(div().w_full().px_4().pb_2().child(self.breadcrumb(cx)))
            .child(inner);
        // 空目录 / 出错 / 还在读：底下没有可下的东西，别摆一颗永远按不动的
        // 「下载 0 本」。取书状态还挂着的时候仍留着 —— 那是上一批的收尾。
        if !self.entries.is_empty() || self.importing.is_some() {
            card = card.child(footer);
        }
        card.into_any_element()
    }

    /// 卡头那支笔：把下面换成这台盘的配置表单。改完保存直接回到目录，当前路径不丢。
    fn begin_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.kind.provider().configured(&ConfigHolder::cloud(cx)) {
            return;
        }
        self.editing = true;
        self.open_drive_form(self.kind, window, cx);
        cx.notify();
    }

    /// 面包屑：根 + 逐级目录，点哪级回哪级。当前那级按不动，颜色也比上面几级重
    /// —— 这是 `Breadcrumb` 组件自己的规则（最后一项用前景色），不用手摆一串按钮。
    fn breadcrumb(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut bar = Breadcrumb::new().w_full().flex_wrap();
        if self.path.is_empty() {
            bar = bar.child(BreadcrumbItem::new(tr(cx, "cloud.root")));
        } else {
            let view = cx.entity().clone();
            bar = bar.child(
                BreadcrumbItem::new(tr(cx, "cloud.root")).on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| {
                        this.path = String::new();
                        this.selected.clear();
                        this.load_dir(cx);
                    });
                }),
            );
        }
        let segments: Vec<&str> = self.path.split('/').filter(|s| !s.is_empty()).collect();
        let mut prefix = String::new();
        for (index, segment) in segments.iter().enumerate() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(segment);
            let last = index + 1 == segments.len();
            let item = BreadcrumbItem::new(SharedString::from(segment.to_string()));
            if last {
                bar = bar.child(item);
            } else {
                let view = cx.entity().clone();
                let jump = prefix.clone();
                bar = bar.child(item.on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| {
                        this.path = jump.clone();
                        this.selected.clear();
                        this.load_dir(cx);
                    });
                }));
            }
        }
        bar.into_any_element()
    }

    /// 一行文件夹：整行可点（`ListItem` 自带 hover 与圆角命中面），右侧一枚
    /// 「进去」的箭头。图标与名字共用一条 24 的脊柱，文件行也对齐这一条。
    fn directory_row(&self, entry: &Entry, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().clone();
        let next = entry.path.clone();
        let name = entry.name.clone();
        ListItem::new(format!("cloud-dir-{}", entry.path))
            .w_full()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .accessibility_label(name.clone())
            .on_click(move |_, _, cx| {
                view.update(cx, |this, cx| this.open_entry(next.clone(), cx));
            })
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    // 与文件行那颗勾选框同宽的槽：文件夹没有勾选框，但图标不能因此
                    // 往左挪半格 —— 两种行的图标与书名共用一条竖线。
                    .child(div().size_4().flex_shrink_0())
                    .child(
                        Icon::new(IconName::Folder)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(div().flex_1().min_w_0().text_ellipsis().child(name))
                    .child(
                        Icon::new(IconName::ChevronRight)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .into_any_element()
    }

    /// 一行文件：勾选框是唯一的命中面（整行可点会与勾选框撞成两次切换），
    /// 但选中之后整行垫一层浅色，读清单时不必逐个找那枚小小的勾。
    /// 大小钉在固定的右车道，长书名与短书名下面的数字才排得成一条线。
    fn file_row(&self, entry: &Entry, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().clone();
        let path = entry.path.clone();
        let name = entry.name.clone();
        let checked = self.selected.contains(&path);
        let size = size_label(entry.size);
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .when(checked, |this| this.bg(cx.theme().list_active))
            .child(
                Checkbox::new(format!("cloud-check-{}", entry.path))
                    .checked(checked)
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| this.toggle(path.clone(), cx));
                    }),
            )
            .child(
                Icon::new(IconName::FileText)
                    .small()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(div().flex_1().min_w_0().text_ellipsis().child(name))
            .child(
                div()
                    .w_16()
                    .text_sm()
                    .text_right()
                    .text_color(cx.theme().muted_foreground)
                    .child(size),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    //! 这一屏能离线钉住的是**版式规则**：开屏停在 WebDAV、下面直接是配置；换一台
    //! 只换下面那一块，不压第二屏。真实的列目录要网络，留在真机上验。

    use folio_core::Config;
    use folio_core::infra::config::WebDavDrive;
    use folio_core::net::cloud::Entry;
    use gpui_kit::base::NavStackState;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext, px, size};

    use super::{CloudView, Kind};
    use crate::ui::services::config::ConfigHolder;

    /// 开屏就该是 WebDAV，且下面直接摆它的配置表单 —— 不是先选一台、再点进去。
    #[gpui_kit::test]
    fn the_page_opens_on_webdav_with_its_form_under_the_pills(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut built = None;
        let handle: AnyWindowHandle = cx
            .open_window(size(px(390.), px(844.)), |window, cx| {
                ConfigHolder::install(Config::default(), cx);
                ConfigHolder::edit_membership(cx, |membership| membership.lifetime = true);
                let stack = cx.new(|_| NavStackState::new());
                let view = cx.new(|cx| CloudView::new(stack.downgrade(), cx));
                built = Some(view.clone());
                Root::new(view, window, cx)
            })
            .into();
        let _view = built.expect("这一屏该建起来");

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("cloud-drive-webdav").is_some(),
                "三台网盘的胶囊该在"
            );
            assert!(
                window.try_find("cloud-webdav-save").is_some(),
                "默认选中 WebDAV，下面该直接出现它的配置表单"
            );
            assert!(
                window.try_find("cloud-baidu-save").is_none(),
                "没选百度时不该摆百度的表单"
            );
            assert!(
                window.try_find("cloud-download").is_none(),
                "还没配好，目录卡不该出现"
            );
        })
        .expect("这一屏该画得出一帧");
    }

    /// 配置一落定就**当场**开始列目录，不需要读者再点一颗「浏览」。
    ///
    /// 不发请求：只钉状态机那一半 —— `after_save` 之后 `loading` 已经起来、
    /// 路径是这台盘的起始目录、表单已收起。真正的网络往返在真机上验。
    #[gpui_kit::test]
    fn saving_starts_the_directory_load_right_away(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            ConfigHolder::edit_membership(cx, |membership| membership.lifetime = true);
            ConfigHolder::edit_cloud(cx, |cloud| {
                cloud.webdav = WebDavDrive {
                    url: "https://dav.example.com/dav".into(),
                    username: "noke".into(),
                    password: "app-pass".into(),
                    dir: "我的书".into(),
                };
            });
        });
        let stack = cx.update(|cx| cx.new(|_| NavStackState::new()));
        let view = cx.update(|cx| cx.new(|cx| CloudView::new(stack.downgrade(), cx)));

        view.update(cx, |this, cx| {
            this.started = true; // 跳过首帧那一次分流，模拟「刚按了保存」。
            this.kind = Kind::WebDav;
            this.after_save(cx);
            assert!(!this.editing, "保存后表单该收起");
            assert!(this.form.is_none(), "WebDAV 的表单该被清掉");
            assert_eq!(this.path, "我的书", "目录该从配置里那一格开始");
            assert!(this.loading, "保存落定后该当场开始列目录");
        });
    }

    /// 点另一台只换下面那一块：表单随之换掉，页面与胶囊不动。
    #[gpui_kit::test]
    fn picking_another_drive_swaps_the_panel_in_place(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut built = None;
        let handle: AnyWindowHandle = cx
            .open_window(size(px(390.), px(844.)), |window, cx| {
                ConfigHolder::install(Config::default(), cx);
                ConfigHolder::edit_membership(cx, |membership| membership.lifetime = true);
                let stack = cx.new(|_| NavStackState::new());
                let view = cx.new(|cx| CloudView::new(stack.downgrade(), cx));
                built = Some(view.clone());
                Root::new(view, window, cx)
            })
            .into();
        let _view = built.expect("这一屏该建起来");

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("cloud-drive-baidu", cx);
            assert!(
                window.try_find("cloud-baidu-save").is_some(),
                "点了百度，下面该换上百度的配置"
            );
            assert!(
                window.try_find("cloud-webdav-save").is_none(),
                "上一台的表单该收掉，不能两张一起挂在下面"
            );
            assert!(
                window.try_find("cloud-drive-baidu").is_some()
                    && window.try_find("cloud-drive-webdav").is_some(),
                "胶囊本身不随换台消失"
            );
        })
        .expect("这一屏该画得出一帧");
    }

    /// 已读到的目录要画成卡片：文件夹行、带勾选框的文件行、卡底的下载钮与卡头的笔
    /// 四样都在。这里不连网 —— 直接摆一份 `entries`，`started` 已置真跳过首帧分流。
    #[gpui_kit::test]
    fn a_loaded_directory_draws_its_rows_and_actions(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut built = None;
        let handle: AnyWindowHandle = cx
            .open_window(size(px(390.), px(844.)), |window, cx| {
                ConfigHolder::install(Config::default(), cx);
                ConfigHolder::edit_membership(cx, |membership| membership.lifetime = true);
                ConfigHolder::edit_cloud(cx, |cloud| {
                    cloud.webdav = WebDavDrive {
                        url: "https://dav.example.com/dav".into(),
                        username: "noke".into(),
                        password: "app-pass".into(),
                        dir: String::new(),
                    };
                });
                let stack = cx.new(|_| NavStackState::new());
                let view = cx.new(|cx| CloudView::new(stack.downgrade(), cx));
                view.update(cx, |this, _| {
                    this.started = true; // 跳过首帧的列目录，别真去连网。
                    this.entries = vec![
                        Entry {
                            name: "小说".into(),
                            path: "小说".into(),
                            is_dir: true,
                            size: None,
                            modified: None,
                            etag: None,
                        },
                        Entry {
                            name: "三体.epub".into(),
                            path: "三体.epub".into(),
                            is_dir: false,
                            size: Some(2_400_000),
                            modified: None,
                            etag: None,
                        },
                    ];
                });
                built = Some(view.clone());
                Root::new(view, window, cx)
            })
            .into();
        let _view = built.expect("这一屏该建起来");

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("cloud-dir-小说").is_some(),
                "文件夹行该在卡片里"
            );
            assert!(
                window.try_find("cloud-check-三体.epub").is_some(),
                "文件行该带勾选框"
            );
            assert!(
                window.try_find("cloud-download").is_some(),
                "有条目时卡底要挂下载钮"
            );
            assert!(
                window.try_find("cloud-edit-webdav").is_some(),
                "卡头该有改配置的笔"
            );
        })
        .expect("目录卡该画得出一帧");
    }
}
