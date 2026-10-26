//! OPDS 书源：整屏浏览器（书源列表 → feed → 子目录），从架面「⋮」菜单压上导航栈。
//!
//! 一页只管一件事：把 core 读出来的 [`Feed`] 摆出来 —— 导航条目是一行，出版物是
//! 「封面 + 书名/作者/简介 + 下载」。下载走现有导入管线（`UrlSource` 带上书源的
//! Basic 头），所以落库、判重、哈希与书架刷新都复用同一套。
//!
//! 内层用 `Screen` 而不是给每个子目录再压一页：整个浏览器是导航栈上的**一页**，
//! 顶栏的返回优先退内层（子目录 → 上层 feed → 书源列表 → 架面），系统返回键仍由
//! 外壳弹掉整页。封面按 URL 在后台取一次、解码好存内存，失败的不再重试。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use folio_core::net::opds::{self, Acquisition, Entry, Feed};
use folio_core::net::remote::{Method, Request, Transport};
use folio_core::{ImportOutcome, ImportReport, OpdsCatalog, TaskEvent, UrlSource, submit_import};
use gpui_kit::assets::IconName;
use gpui_kit::base::{NavMotion, NavStackState};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, Image, InteractiveElement,
    IntoElement, ObjectFit, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, StyledImage, WeakEntity, Window, div, img, px,
};

use crate::ui::components::{TopBar, muted_label};
use crate::ui::foundation::images::cover_image;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::{tf, tr};
use crate::ui::services::resident;
use crate::ui::services::{app_store, library};

const COVER_WIDTH: f32 = 56.0;
const COVER_HEIGHT: f32 = 84.0;
const FORM_WIDTH: f32 = 560.0;

/// 架面那边唯一需要接的一件事：有书进了库，架面要重查。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpdsEvent {
    Imported,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Catalogs,
    Form,
    Feed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Failure {
    Auth,
    Network,
    Remote,
    Parse,
}

struct CatalogForm {
    name: Entity<InputState>,
    url: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    editing: Option<usize>,
    error: Option<SharedString>,
}

struct FeedPage {
    title: String,
    url: String,
    catalog: usize,
    feed: Feed,
    loading: bool,
    loading_more: bool,
    error: Option<Failure>,
    more_error: Option<Failure>,
}

struct ImportState {
    message: SharedString,
    done: bool,
}

pub struct OpdsView {
    stack: WeakEntity<NavStackState>,
    screen: Screen,
    catalogs: Vec<OpdsCatalog>,
    form: Option<CatalogForm>,
    pages: Vec<FeedPage>,
    pending_delete: Option<usize>,
    thumbs: HashMap<String, Arc<Image>>,
    requested_thumbs: HashSet<String>,
    importing: Option<ImportState>,
}

impl EventEmitter<OpdsEvent> for OpdsView {}

impl OpdsView {
    pub fn new(stack: WeakEntity<NavStackState>, cx: &mut Context<Self>) -> Self {
        Self {
            stack,
            screen: Screen::Catalogs,
            catalogs: ConfigHolder::opds(cx).catalogs,
            form: None,
            pages: Vec::new(),
            pending_delete: None,
            thumbs: HashMap::new(),
            requested_thumbs: HashSet::new(),
            importing: None,
        }
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        match self.screen {
            Screen::Form => {
                self.form = None;
                self.screen = Screen::Catalogs;
                cx.notify();
            }
            Screen::Feed => {
                if self.pages.len() > 1 {
                    self.pages.pop();
                } else {
                    self.pages.clear();
                    self.screen = Screen::Catalogs;
                }
                cx.notify();
            }
            Screen::Catalogs => {
                let _ = self
                    .stack
                    .update(cx, |stack, cx| stack.pop(NavMotion::Animated, cx));
            }
        }
    }

    fn open_form(&mut self, window: &mut Window, editing: Option<usize>, cx: &mut Context<Self>) {
        let current = editing
            .and_then(|index| self.catalogs.get(index))
            .cloned()
            .unwrap_or_default();
        let name = cx.new(|cx| InputState::new(window, cx).default_value(current.name));
        let url = cx.new(|cx| InputState::new(window, cx).default_value(current.url));
        let username = cx.new(|cx| InputState::new(window, cx).default_value(current.username));
        let password = cx.new(|cx| InputState::new(window, cx).default_value(current.password));
        self.form = Some(CatalogForm {
            name,
            url,
            username,
            password,
            editing,
            error: None,
        });
        self.screen = Screen::Form;
        cx.notify();
    }

    fn save_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.form.as_ref() else {
            return;
        };
        let name = form.name.read(cx).value().trim().to_string();
        let url = form.url.read(cx).value().trim().to_string();
        let username = form.username.read(cx).value().trim().to_string();
        let password = form.password.read(cx).value().to_string();
        let editing = form.editing;
        if name.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
            if let Some(form) = self.form.as_mut() {
                form.error = Some(tr(cx, "opds.form_invalid"));
            }
            cx.notify();
            return;
        }

        let catalog = OpdsCatalog {
            name,
            url,
            username,
            password,
        };
        let mut catalogs = self.catalogs.clone();
        match editing {
            Some(index) if index < catalogs.len() => catalogs[index] = catalog,
            _ => catalogs.push(catalog),
        }
        ConfigHolder::edit_opds(cx, |opds| opds.catalogs = catalogs.clone());
        self.catalogs = catalogs;
        self.form = None;
        self.screen = Screen::Catalogs;
        cx.notify();
    }

    fn remove_catalog(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.catalogs.len() {
            return;
        }
        let mut catalogs = self.catalogs.clone();
        catalogs.remove(index);
        ConfigHolder::edit_opds(cx, |opds| opds.catalogs = catalogs.clone());
        self.catalogs = catalogs;
        self.pending_delete = None;
        self.pages.clear();
        self.screen = Screen::Catalogs;
        cx.notify();
    }

    fn open_catalog(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(catalog) = self.catalogs.get(index) else {
            return;
        };
        let title = catalog.name.clone();
        let url = catalog.url.clone();
        self.pages.clear();
        self.pages.push(FeedPage {
            title,
            url,
            catalog: index,
            feed: Feed::default(),
            loading: true,
            loading_more: false,
            error: None,
            more_error: None,
        });
        self.screen = Screen::Feed;
        self.load_page(0, cx);
    }

    fn open_navigation(&mut self, href: String, title: String, cx: &mut Context<Self>) {
        let Some(catalog) = self.pages.last().map(|page| page.catalog) else {
            return;
        };
        self.pages.push(FeedPage {
            title,
            url: href,
            catalog,
            feed: Feed::default(),
            loading: true,
            loading_more: false,
            error: None,
            more_error: None,
        });
        let index = self.pages.len() - 1;
        self.screen = Screen::Feed;
        self.load_page(index, cx);
    }

    fn load_page(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(page) = self.pages.get(index) else {
            return;
        };
        let url = page.url.clone();
        let Some(catalog) = self.catalogs.get(page.catalog).cloned() else {
            return;
        };
        if let Some(page) = self.pages.get_mut(index) {
            page.loading = true;
            page.error = None;
        }
        cx.notify();

        let transport = transport();
        let work = cx.background_spawn(async move {
            opds::fetch(
                transport.as_ref(),
                &url,
                &catalog.username,
                &catalog.password,
            )
        });
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_this: WeakEntity<OpdsView>, cx| {
            let result = work.await;
            let _ = weak.update(cx, |this, cx| this.settle_page(index, result, cx));
        })
        .detach();
    }

    fn settle_page(
        &mut self,
        index: usize,
        result: Result<Feed, opds::Error>,
        cx: &mut Context<Self>,
    ) {
        let Some(page) = self.pages.get_mut(index) else {
            return;
        };
        page.loading = false;
        match result {
            Ok(feed) => {
                page.feed = feed;
                page.error = None;
                page.more_error = None;
                let urls = thumb_urls(&page.feed);
                let catalog = page.catalog;
                self.request_thumbs(urls, catalog, cx);
            }
            Err(error) => {
                tracing::warn!("OPDS: 读 {} 失败：{error}", page.url);
                page.error = Some(failure_of(&error));
            }
        }
        cx.notify();
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.pages.len().checked_sub(1) else {
            return;
        };
        let Some(page) = self.pages.get(index) else {
            return;
        };
        let Some(url) = page.feed.next.clone() else {
            return;
        };
        if page.loading_more {
            return;
        }
        let Some(catalog) = self.catalogs.get(page.catalog).cloned() else {
            return;
        };
        if let Some(page) = self.pages.get_mut(index) {
            page.loading_more = true;
            page.more_error = None;
        }
        cx.notify();

        let transport = transport();
        let work = cx.background_spawn(async move {
            opds::fetch(
                transport.as_ref(),
                &url,
                &catalog.username,
                &catalog.password,
            )
        });
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_this: WeakEntity<OpdsView>, cx| {
            let result = work.await;
            let _ = weak.update(cx, |this, cx| this.settle_more(index, result, cx));
        })
        .detach();
    }

    fn settle_more(
        &mut self,
        index: usize,
        result: Result<Feed, opds::Error>,
        cx: &mut Context<Self>,
    ) {
        let Some(page) = self.pages.get_mut(index) else {
            return;
        };
        page.loading_more = false;
        match result {
            Ok(feed) => {
                page.feed.entries.extend(feed.entries);
                page.feed.next = feed.next;
                page.more_error = None;
                let urls = thumb_urls(&page.feed);
                let catalog = page.catalog;
                self.request_thumbs(urls, catalog, cx);
            }
            Err(error) => {
                tracing::warn!("OPDS: 翻下一页失败：{error}");
                page.more_error = Some(failure_of(&error));
            }
        }
        cx.notify();
    }

    fn request_thumbs(&mut self, urls: Vec<String>, catalog: usize, cx: &mut Context<Self>) {
        let headers = self
            .catalogs
            .get(catalog)
            .and_then(|catalog| opds::auth_header(&catalog.username, &catalog.password))
            .into_iter()
            .collect::<Vec<_>>();
        for url in urls {
            if self.requested_thumbs.contains(&url) || self.thumbs.contains_key(&url) {
                continue;
            }
            self.requested_thumbs.insert(url.clone());
            let headers = headers.clone();
            let transport = transport();
            let work = cx.background_spawn(async move {
                let mut request = Request::new(Method::Get, url.clone());
                for (name, value) in headers {
                    request = request.header(name, value);
                }
                let response = transport.request(request).ok()?;
                if !(200..300).contains(&response.status) {
                    return None;
                }
                let image = cover_image(&response.body)?;
                Some((url, image))
            });
            let weak = cx.entity().downgrade();
            cx.spawn(async move |_this: WeakEntity<OpdsView>, cx| {
                let got = work.await;
                if let Some((url, image)) = got {
                    let _ = weak.update(cx, |this, cx| {
                        this.thumbs.insert(url, image);
                        cx.notify();
                    });
                }
            })
            .detach();
        }
    }

    fn start_download(
        &mut self,
        title: String,
        acquisition: Acquisition,
        catalog: usize,
        cx: &mut Context<Self>,
    ) {
        if self.importing.as_ref().is_some_and(|state| !state.done) {
            return;
        }
        let Some(dir) = library::target_dir(&ConfigHolder::library(cx)) else {
            self.importing = Some(ImportState {
                message: tr(cx, "opds.library_missing"),
                done: true,
            });
            cx.notify();
            return;
        };
        let Some(store) = app_store::handle(cx) else {
            self.importing = Some(ImportState {
                message: tr(cx, "opds.library_missing"),
                done: true,
            });
            cx.notify();
            return;
        };
        let headers = self
            .catalogs
            .get(catalog)
            .and_then(|catalog| opds::auth_header(&catalog.username, &catalog.password))
            .into_iter()
            .collect::<Vec<_>>();
        let source = UrlSource::new(acquisition.href, transport()).with_headers(headers);
        let handle = submit_import(&resident::pool(), Box::new(source), dir, store);
        self.importing = Some(ImportState {
            message: tf(cx, "opds.importing", &[("title", &title)]).into(),
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
        cx.spawn(async move |_this: WeakEntity<OpdsView>, cx| {
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
                        tracing::info!("OPDS: 一批导入结清 —— {}", report.summary());
                        if report.added > 0 {
                            (tr(cx, "opds.imported"), true)
                        } else if report.duplicate > 0 {
                            (tr(cx, "opds.import_duplicate"), false)
                        } else {
                            (tr(cx, "opds.import_failed"), false)
                        }
                    }
                    Err(error) => {
                        tracing::warn!("OPDS: 导入失败：{error}");
                        (tr(cx, "opds.import_failed"), false)
                    }
                };
                if let Some(importing) = self.importing.as_mut() {
                    importing.message = message;
                    importing.done = true;
                }
                if added {
                    cx.emit(OpdsEvent::Imported);
                }
                cx.notify();
                true
            }
            TaskEvent::Cancelled => {
                if let Some(importing) = self.importing.as_mut() {
                    importing.message = tr(cx, "opds.import_cancelled");
                    importing.done = true;
                }
                cx.notify();
                true
            }
            TaskEvent::Started | TaskEvent::Progress(_) | TaskEvent::Bytes(_) => false,
        }
    }
}

impl Render for OpdsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match self.screen {
            Screen::Catalogs => tr(cx, "opds.catalogs"),
            Screen::Form => {
                if self
                    .form
                    .as_ref()
                    .is_some_and(|form| form.editing.is_some())
                {
                    tr(cx, "opds.edit")
                } else {
                    tr(cx, "opds.add")
                }
            }
            Screen::Feed => self
                .pages
                .last()
                .map(|page| SharedString::from(page.title.clone()))
                .unwrap_or_else(|| tr(cx, "opds.catalogs")),
        };
        let view = cx.entity().clone();
        let top = TopBar::new("opds-top-bar")
            .title(title)
            .back_tooltip(tr(cx, "shelf.back"))
            .on_back(move |_, cx| {
                view.update(cx, |this, cx| this.back(cx));
            });

        let body = match self.screen {
            Screen::Catalogs => self.catalogs_body(cx),
            Screen::Form => self.form_body(window, cx),
            Screen::Feed => self.feed_body(cx),
        };
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(top)
            .child(body)
    }
}

impl OpdsView {
    fn catalogs_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().clone();
        let add = Button::new("opds-add")
            .outline()
            .small()
            .icon(IconName::Plus)
            .label(tr(cx, "opds.add").to_string())
            .on_click(move |_, window, cx| {
                view.update(cx, |this, cx| this.open_form(window, None, cx));
            });

        if self.catalogs.is_empty() {
            return v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(div().text_base().child(tr(cx, "opds.empty")))
                .child(muted_label(tr(cx, "opds.empty_hint"), cx))
                .child(add)
                .into_any_element();
        }

        let mut list = v_flex()
            .id("opds-catalogs")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .px_4()
            .py_3()
            .gap_2();
        for (index, catalog) in self.catalogs.iter().enumerate() {
            if self.pending_delete == Some(index) {
                let view_cancel = cx.entity().clone();
                let view_remove = cx.entity().clone();
                list = list.child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .rounded(cx.theme().radius_lg)
                        .bg(cx.theme().muted)
                        .px_3()
                        .py_2()
                        .child(div().child(tr(cx, "opds.remove_confirm")))
                        .child(
                            h_flex()
                                .w_full()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("opds-remove-cancel")
                                        .ghost()
                                        .small()
                                        .label(tr(cx, "opds.cancel").to_string())
                                        .on_click(move |_, _, cx| {
                                            view_cancel.update(cx, |this, cx| {
                                                this.pending_delete = None;
                                                cx.notify();
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("opds-remove-yes")
                                        .danger()
                                        .small()
                                        .label(tr(cx, "opds.remove").to_string())
                                        .on_click(move |_, _, cx| {
                                            view_remove.update(cx, |this, cx| {
                                                this.remove_catalog(index, cx)
                                            });
                                        }),
                                ),
                        ),
                );
                continue;
            }

            let view_open = cx.entity().clone();
            let view_remove = cx.entity().clone();
            let view_edit = cx.entity().clone();
            list = list.child(
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
                                Button::new(format!("opds-catalog-{index}"))
                                    .ghost()
                                    .w_full()
                                    .justify_start()
                                    .icon(IconName::BookOpen)
                                    .label(catalog.name.clone())
                                    .on_click(move |_, _, cx| {
                                        view_open
                                            .update(cx, |this, cx| this.open_catalog(index, cx));
                                    }),
                            )
                            .child(muted_label(catalog.url.clone(), cx)),
                    )
                    .child(
                        Button::new(format!("opds-edit-{index}"))
                            .ghost()
                            .small()
                            .icon(IconName::NotebookPen)
                            .tooltip(tr(cx, "opds.edit").to_string())
                            .accessibility_label(tr(cx, "opds.edit"))
                            .on_click(move |_, window, cx| {
                                view_edit
                                    .update(cx, |this, cx| this.open_form(window, Some(index), cx));
                            }),
                    )
                    .child(
                        Button::new(format!("opds-trash-{index}"))
                            .ghost()
                            .small()
                            .icon(IconName::Trash)
                            .tooltip(tr(cx, "opds.remove").to_string())
                            .accessibility_label(tr(cx, "opds.remove"))
                            .on_click(move |_, _, cx| {
                                view_remove.update(cx, |this, cx| {
                                    this.pending_delete = Some(index);
                                    cx.notify();
                                });
                            }),
                    ),
            );
        }

        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(h_flex().w_full().justify_end().px_4().py_2().child(add))
            .child(list)
            .into_any_element()
    }

    fn form_body(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(form) = self.form.as_ref() else {
            return div().into_any_element();
        };
        let view_save = cx.entity().clone();
        let view_cancel = cx.entity().clone();
        let mut body = v_flex()
            .id("opds-form")
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
                    .child(div().text_sm().child(tr(cx, "opds.name")))
                    .child(Input::new(&form.name).w_full()),
            )
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(FORM_WIDTH))
                    .gap_1()
                    .child(div().text_sm().child(tr(cx, "opds.url")))
                    .child(Input::new(&form.url).w_full()),
            )
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(FORM_WIDTH))
                    .gap_1()
                    .child(div().text_sm().child(tr(cx, "opds.username")))
                    .child(Input::new(&form.username).w_full()),
            )
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(FORM_WIDTH))
                    .gap_1()
                    .child(div().text_sm().child(tr(cx, "opds.password")))
                    .child(Input::new(&form.password).w_full()),
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
                    Button::new("opds-form-cancel")
                        .ghost()
                        .small()
                        .label(tr(cx, "opds.cancel").to_string())
                        .on_click(move |_, _, cx| {
                            view_cancel.update(cx, |this, cx| this.back(cx));
                        }),
                )
                .child(
                    Button::new("opds-form-save")
                        .small()
                        .label(tr(cx, "opds.save").to_string())
                        .on_click(move |_, _, cx| {
                            view_save.update(cx, |this, cx| this.save_form(cx));
                        }),
                ),
        )
        .into_any_element()
    }

    fn feed_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(index) = self.pages.len().checked_sub(1) else {
            return div().into_any_element();
        };
        let page = &self.pages[index];
        let view_retry = cx.entity().clone();
        let view_more = cx.entity().clone();

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

        if page.loading && page.feed.entries.is_empty() {
            return body
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .child(muted_label(tr(cx, "opds.loading"), cx)),
                )
                .into_any_element();
        }
        if let Some(error) = page.error {
            return body
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .child(div().text_base().child(failure_text(error, cx)))
                        .child(
                            Button::new("opds-retry")
                                .outline()
                                .small()
                                .label(tr(cx, "opds.retry").to_string())
                                .on_click(move |_, _, cx| {
                                    view_retry.update(cx, |this, cx| this.load_page(index, cx));
                                }),
                        ),
                )
                .into_any_element();
        }

        if page.feed.entries.is_empty() {
            return body
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .child(div().text_base().child(tr(cx, "opds.empty_feed"))),
                )
                .into_any_element();
        }

        let mut list = v_flex()
            .id(format!("opds-feed-{}", self.pages.len()))
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .px_4()
            .py_3()
            .gap_2();
        for (entry_index, entry) in page.feed.entries.iter().enumerate() {
            if entry.navigation.is_some() {
                list = list.child(self.navigation_row(entry, entry_index, cx));
            } else {
                list = list.child(self.publication_card(entry, index, entry_index, cx));
            }
        }
        if let Some(error) = page.more_error {
            list = list.child(muted_label(failure_text(error, cx), cx));
        }
        if page.feed.next.is_some() {
            list = list.child(
                h_flex().w_full().justify_center().py_2().child(
                    Button::new("opds-next")
                        .outline()
                        .small()
                        .label(
                            tr(
                                cx,
                                if page.loading_more {
                                    "opds.loading_more"
                                } else {
                                    "opds.next"
                                },
                            )
                            .to_string(),
                        )
                        .disabled(page.loading_more)
                        .on_click(move |_, _, cx| {
                            view_more.update(cx, |this, cx| this.load_more(cx));
                        }),
                ),
            );
        }
        body.child(list).into_any_element()
    }

    fn navigation_row(
        &self,
        entry: &Entry,
        entry_index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(href) = entry.navigation.clone() else {
            return div().into_any_element();
        };
        let title = entry.title.clone();
        let view = cx.entity().clone();
        let mut row = v_flex().w_full().gap_0p5().child(
            Button::new(format!("opds-nav-{entry_index}"))
                .ghost()
                .w_full()
                .justify_start()
                .icon(IconName::FolderOpen)
                .label(entry.title.clone())
                .on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| {
                        this.open_navigation(href.clone(), title.clone(), cx)
                    });
                }),
        );
        if let Some(summary) = &entry.summary {
            row = row.child(muted_label(summary.clone(), cx));
        }
        row.into_any_element()
    }

    fn publication_card(
        &self,
        entry: &Entry,
        page_index: usize,
        entry_index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cover = cover_element(entry, &self.thumbs, cx);
        let mut text = v_flex()
            .flex_1()
            .min_w_0()
            .gap_1()
            .child(div().text_base().child(entry.title.clone()));
        if !entry.authors.is_empty() {
            text = text.child(muted_label(entry.authors.clone(), cx));
        }
        if let Some(summary) = &entry.summary {
            text = text.child(
                div()
                    .text_sm()
                    .line_clamp(2)
                    .text_color(cx.theme().muted_foreground)
                    .child(summary.clone()),
            );
        }
        if let Some(acquisition) = preferred_acquisition(entry) {
            let label = acquisition_label(acquisition);
            let label = if label == "BOOK" {
                tr(cx, "opds.format_book").to_string()
            } else {
                label.to_string()
            };
            let label = tf(cx, "opds.download", &[("format", &label)]);
            let title = entry.title.clone();
            let acquisition = acquisition.clone();
            let page = self
                .pages
                .get(page_index)
                .map(|page| page.catalog)
                .unwrap_or_default();
            let busy = self.importing.as_ref().is_some_and(|state| !state.done);
            let view = cx.entity().clone();
            text = text.child(
                h_flex().pt_1().child(
                    Button::new(format!("opds-download-{entry_index}"))
                        .outline()
                        .small()
                        .label(label)
                        .disabled(busy)
                        .on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.start_download(title.clone(), acquisition.clone(), page, cx)
                            });
                        }),
                ),
            );
        }
        h_flex()
            .w_full()
            .items_start()
            .gap_3()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(cover)
            .child(text)
            .into_any_element()
    }
}

fn cover_element(entry: &Entry, thumbs: &HashMap<String, Arc<Image>>, cx: &App) -> AnyElement {
    let url = entry.thumbnail.as_ref().or(entry.cover.as_ref());
    if let Some(image) = url.and_then(|url| thumbs.get(url)) {
        return img(image.clone())
            .w(px(COVER_WIDTH))
            .h(px(COVER_HEIGHT))
            .object_fit(ObjectFit::Cover)
            .into_any_element();
    }
    div()
        .w(px(COVER_WIDTH))
        .h(px(COVER_HEIGHT))
        .flex_shrink_0()
        .rounded(cx.theme().radius_lg)
        .bg(cx.theme().muted)
        .into_any_element()
}

fn thumb_urls(feed: &Feed) -> Vec<String> {
    let mut urls = Vec::new();
    for entry in &feed.entries {
        if let Some(url) = entry.thumbnail.as_ref().or(entry.cover.as_ref())
            && !urls.iter().any(|known| known == url)
        {
            urls.push(url.clone());
        }
    }
    urls
}

fn preferred_acquisition(entry: &Entry) -> Option<&Acquisition> {
    const PREFERRED: [&str; 9] = [
        "application/epub+zip",
        "application/pdf",
        "application/x-mobipocket-ebook",
        "application/vnd.amazon.ebook",
        "application/x-fictionbook+xml",
        "application/vnd.comicbook+zip",
        "application/vnd.comicbook-rar",
        "application/zip",
        "text/plain",
    ];
    for media_type in PREFERRED {
        if let Some(acquisition) = entry
            .acquisitions
            .iter()
            .find(|acquisition| acquisition.media_type.eq_ignore_ascii_case(media_type))
        {
            return Some(acquisition);
        }
    }
    entry.acquisitions.first()
}

fn acquisition_label(acquisition: &Acquisition) -> &'static str {
    let media_type = acquisition.media_type.to_ascii_lowercase();
    if media_type.contains("epub") {
        return "EPUB";
    }
    if media_type.contains("pdf") {
        return "PDF";
    }
    if media_type.contains("mobipocket") || media_type.contains("mobi") {
        return "MOBI";
    }
    if media_type.contains("amazon.ebook") || media_type.contains("azw") {
        return "AZW3";
    }
    if media_type.contains("fictionbook") || media_type.contains("fb2") {
        return "FB2";
    }
    if media_type.contains("comicbook+zip") {
        return "CBZ";
    }
    if media_type.contains("comicbook-rar") {
        return "CBR";
    }
    if media_type.contains("text/plain") {
        return "TXT";
    }
    if let Some(ext) = acquisition.href.rsplit('.').next() {
        match ext.to_ascii_lowercase().as_str() {
            "epub" => return "EPUB",
            "pdf" => return "PDF",
            "mobi" => return "MOBI",
            "azw3" => return "AZW3",
            "fb2" => return "FB2",
            "cbz" => return "CBZ",
            "cbr" => return "CBR",
            "txt" => return "TXT",
            _ => {}
        }
    }
    "BOOK"
}

fn failure_of(error: &opds::Error) -> Failure {
    use folio_core::net::remote::Error as Remote;
    match error {
        opds::Error::Parse(_) => Failure::Parse,
        opds::Error::Remote(Remote::Transport(_)) => Failure::Network,
        opds::Error::Remote(
            Remote::Unauthorized { .. } | Remote::Forbidden { .. } | Remote::DigestRequired,
        ) => Failure::Auth,
        opds::Error::Remote(_) => Failure::Remote,
    }
}

fn failure_text(failure: Failure, cx: &App) -> SharedString {
    tr(
        cx,
        match failure {
            Failure::Auth => "opds.fail_auth",
            Failure::Network => "opds.fail_network",
            Failure::Remote => "opds.fail_remote",
            Failure::Parse => "opds.fail_parse",
        },
    )
}

fn transport() -> Arc<dyn Transport> {
    crate::ui::services::update::transport()
}
