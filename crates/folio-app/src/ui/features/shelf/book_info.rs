//! 「信息」那一页：卡片长按/右键菜单点开后**压上导航栈**的一整屏。
//!
//! 它是一页，不是架面上的一块面板 —— 所以两件事是白送的：系统返回键把它弹掉（外壳那条
//! `depth() > 1 就 pop`），以及栈上压着页时外壳那条导航栏自己收起（见
//! [`crate::ui::shell`]）。架面因此在这一屏后面被整个盖住，头顶那条搜索/导入也看不见。
//!
//! 左栏是那张封面，右边上下两格是**书名**与**作者** —— 这两条是读者自己认的，就地改得动：
//! 写回 `books` 的那两列（[`Store::set_book_meta`](folio_core::Store::set_book_meta)），
//! 并往外报一条 [`BookInfoEvent::Saved`]，架面收到就重查一次，那张卡上的一行字当场变。
//! 底下那一组是这本书自带的书目（都柏林核心那一组：系列、贡献者、主题、简介、出版者、
//! 出版日期、语言、ISBN、类型、版权，末尾再加格式与大小两条文件的事实），只摆不改 ——
//! 读者改不动文件大小。**有值才摆那一行**：一本书通常只说其中几项，把空的那几项摆成一串
//! 空栏位，读到的信息是「这本书什么都没写」，而不是「这一项没有」。
//! 这里也不摆文件路径：这一屏要认的是「是哪一本、名字对不对」，而那一串内容是哈希的
//! 副本文件名既读不出什么，也不是在这儿能改的东西。
//!
//! 事实由 [`ShelfView::show_info`] 在点开这一刻查齐再交进来，这一页不再回头查库 ——
//! 它手上只有那一份 [`BookMeta`]、一张解码好的封面，以及写回用的句柄。
//! 「改过没有」由**框里的现值**与**进来（或最近一次保存）那一刻的那份**比出来，
//! 不另存一个 dirty 旗标：旗标与框里的字是同一件事的两份记录，迟早漂开。
//! Enter 就是那颗「保存」（primary 的契约：这一屏默认的提交）。

use std::sync::Arc;

use folio_core::store::{BookMeta, Store};
use gpui_kit::assets::IconName;
use gpui_kit::base::{NavMotion, NavStackState};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::form::{field, v_form};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, Image, InteractiveElement,
    IntoElement, ObjectFit, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, StyledImage, Subscription, TestSupportExt as _, WeakEntity, Window, div, img, px,
};

use crate::ui::shared::error::report;
use crate::ui::shared::images::cover_image;
use crate::ui::shared::l10n::{tf, tr};
use crate::ui::widgets::{TopBar, hairline};

use super::book_card::AUTHOR_SEP;
use super::scan::fmt_size;
use super::view::{GUTTER, ShelfView};

/// 左栏那张封面卡的宽（逻辑像素）。
///
/// 比架上一格的下限（`super::view::MIN_CARD` 那一档，手机上 112）窄：这一屏不需要把
/// 封面看到仔细，要的是**认得出是哪一本**，剩下的宽度全归右边那两格 —— 书名与作者才是
/// 要点改的地方。高与架上那一格同一比例（2:3，见 `book_card`）。
const COVER_WIDTH: f32 = 96.0;

/// 封面格的宽高比（宽 ÷ 高），与架上那一格同一档。
const COVER_RATIO: f32 = 2.0 / 3.0;

/// 整块内容的最大宽（逻辑像素）。
///
/// 与桌面工具栏那条搜索框同一个理由：一行书名有多长，这一屏就该有多宽。1600 宽的窗口
/// 把两格输入拉到整片内容宽，它们就从「改书名」变成两块空白。
const PANEL_WIDTH: f32 = 520.0;

/// 这一页往外报的一件事。
pub enum BookInfoEvent {
    /// 书名或作者写回库里了。外壳需要这条：那一行字就是架上那张卡，得重查一次。
    Saved,
}

pub struct BookInfoView {
    /// 自己这一页挂在栈上，收页 = pop 自己（与阅读页同一条路）。
    stack: WeakEntity<NavStackState>,
    /// 写回时按哪一行。
    book_id: i64,
    /// 写回用的书库句柄（读的那一份由外壳查好交进来）。
    store: Arc<Store>,
    /// 左栏那张封面，解码好的。`None` 画占位而不是空白。
    cover: Option<Arc<Image>>,
    /// 书名那一格。进来时填库里现有的值，占位用同一个字段名（值为空时看得出来要填什么）。
    title: Entity<InputState>,
    /// 作者那一格：多位作者串成一行摆，写回时按 [`AUTHOR_SEP`] 拆回人名。
    authors: Entity<InputState>,
    /// 进来那一刻（以及每次保存成功之后）库里的那两份 —— 改没改就是这么比出来的。
    saved_title: SharedString,
    saved_authors: SharedString,
    /// 点开这一刻查齐的那一份书目。除书名与作者（那两格是改得动的）以外，这里每一项
    /// 都只摆不改；为空的那几项在 [`fact_rows`] 里筛掉，不占一行。
    meta: BookMeta,
    /// 两格的订阅：值变了要重画一次（那颗「保存」按不按得动是比出来的），Enter 要提交。
    /// `Subscription` 一 drop 就退订，所以跟着这一页一起活、一起没。
    _watch: (Subscription, Subscription),
}

impl ShelfView {
    /// 卡片菜单上那颗「信息」：查一次库，把这一本摆成**一整页**压上导航栈。
    ///
    /// 压上栈而不是在架面上画一块面板，换来的是两件不用各自维护的事：栈上压了页 →
    /// 外壳那条导航栏自己收起（`AppShell::page_pushed`），系统返回键自己把这一页弹掉。
    ///
    /// 查的是 [`Store::book_meta`] 而不是 [`Store::book`]：这一屏要摆的都柏林那几项
    /// 只在前者里，而它按 id 单查一条，多带的简介与主题词摊不到架面那几百行上。
    ///
    /// 只查不拦 —— 书查不到（已删、库坏了）就不开这一页，留一条日志：菜单是瞬间的事，
    /// 这里没有要保的旧状态。封面读不出来不拦 —— 那一格画占位，右边那两格才是正事。
    pub(super) fn show_info(&mut self, book_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            tracing::error!("ShelfView: 没有书库，查不了第 {book_id} 本的信息");
            return;
        };
        let meta = match store.book_meta(book_id) {
            Ok(Some(meta)) => meta,
            Ok(None) => {
                tracing::warn!("ShelfView: 第 {book_id} 本已经不在库里，不开信息页");
                return;
            }
            Err(err) => {
                report(
                    "ShelfView",
                    &anyhow::Error::new(err).context(format!("查第 {book_id} 本的信息")),
                );
                return;
            }
        };
        let cover = match store.book_cover(book_id) {
            Ok(Some(bytes)) => cover_image(&bytes),
            Ok(None) => None,
            Err(err) => {
                tracing::warn!("ShelfView: 第 {book_id} 本的封面读不出来：{err}");
                None
            }
        };
        let page =
            cx.new(|cx| BookInfoView::new(self.stack.clone(), store, meta, cover, window, cx));
        // 那一页写回书名作者之后报一声：架上那张卡上的一行字要跟着变。
        self._info_watch = Some(cx.subscribe(&page, |this, _, event, cx| {
            match event {
                // 这一页只报一件事：写回了。架面那张卡上的一行字要跟着变。
                BookInfoEvent::Saved => this.force_reload(cx),
            }
        }));
        let _ = self
            .stack
            .update(cx, |state, cx| state.push(page, NavMotion::Animated, cx));
    }

    /// 喂一条「点开信息」的命令。只在跑回归门的那一端编译。
    ///
    /// 真实入口是卡片长按/右键菜单里那一条，而测试窗口没有「点到这一块元素」的定位法 ——
    /// 与 [`Self::feed_import_event`] 同一取舍：从命令已经派到架面这一刻进。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn show_info_for_test(
        &mut self,
        book_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_info(book_id, window, cx);
    }
}

impl BookInfoView {
    pub fn new(
        stack: WeakEntity<NavStackState>,
        store: Arc<Store>,
        meta: BookMeta,
        cover: Option<Arc<Image>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title_line = SharedString::from(meta.title.clone());
        let authors_line = SharedString::from(meta.authors.join(AUTHOR_SEP));
        let title_label = tr(cx, "shelf.info_field_title");
        let authors_label = tr(cx, "shelf.info_field_author");
        let title_entry = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(title_label)
                .default_value(title_line.clone())
        });
        let authors_entry = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(authors_label)
                .default_value(authors_line.clone())
        });
        let _watch = (
            cx.subscribe(&title_entry, |this, _, event, cx| this.on_input(event, cx)),
            cx.subscribe(&authors_entry, |this, _, event, cx| {
                this.on_input(event, cx)
            }),
        );
        let book_id = meta.id;
        Self {
            stack,
            book_id,
            store,
            cover,
            title: title_entry,
            authors: authors_entry,
            saved_title: title_line,
            saved_authors: authors_line,
            meta,
            _watch,
        }
    }

    fn pop(&mut self, cx: &mut Context<Self>) {
        let _ = self
            .stack
            .update(cx, |state, cx| state.pop(NavMotion::Animated, cx));
    }

    /// 回归门用：把两格改成给的样子，然后按「保存」。
    ///
    /// 走的是真的 [`InputState`] 与生产那一手 [`Self::save`] —— 门要钉的是「框里的字怎么
    /// 变成库里的行」，自己拼一个字符串直接塞进 `Store` 就只是在测 `Store`。
    /// 真实入口是那一屏的输入框 + 那颗按钮，而测试窗口里没有「点到这一块元素」的定位法。
    #[cfg(all(test, not(mobile)))]
    pub(crate) fn rename_for_test(
        &mut self,
        title: &str,
        authors: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.title
            .update(cx, |field, cx| field.set_value(title, window, cx));
        self.authors
            .update(cx, |field, cx| field.set_value(authors, window, cx));
        self.save(cx);
    }

    /// 两格里的一条事件 → 这一页的一次重画，或一次提交。
    fn on_input(&mut self, event: &InputEvent, cx: &mut Context<Self>) {
        match event {
            // 值变了要重画一次：那颗「保存」按不按得动，是拿现值与库里那份比出来的。
            InputEvent::Change => cx.notify(),
            // Enter 走的是同一条提交 —— primary 按钮画的就是这一手。
            InputEvent::PressEnter { .. } => self.save(cx),
            InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    /// 把两格的现值写回 `books` 那两列。
    ///
    /// 空书名不进：那一列空了，架上那张卡就只剩作者一行，而没封面时格子里画的也是书名。
    /// 界面上那颗按钮此刻已经按不动，这里是 Enter 那条路 —— 两条都得拦在写之前。
    ///
    /// 写失败只在这一页报出去，两份基准值不动：于是「保存」还亮着，读者原地再试一次，
    /// 界面不猜「是不是其实已经写进去了」。
    fn save(&mut self, cx: &mut Context<Self>) {
        let title = self.title.read(cx).value().trim().to_string();
        let authors = split_authors(&self.authors.read(cx).value());
        if title.is_empty() {
            return;
        }
        if let Err(err) = self.store.set_book_meta(self.book_id, &title, &authors) {
            report(
                "BookInfoView",
                &anyhow::Error::new(err).context(format!("改第 {} 本的书名作者", self.book_id)),
            );
            return;
        }
        // 基准跟着写进去的那份走（去过首尾空白、作者拆过又并起来），不是框里的原始串 ——
        // 否则保存完「改过没有」还是真，那颗按钮永远亮着。
        self.saved_title = title.clone().into();
        self.saved_authors = authors.join(AUTHOR_SEP).into();
        tracing::info!("BookInfoView: 第 {} 本改成《{title}》", self.book_id);
        // 架面那一行字跟着变（外壳把它转给架面重查）。
        cx.emit(BookInfoEvent::Saved);
        cx.notify();
    }
}

impl EventEmitter<BookInfoEvent> for BookInfoView {}

impl Render for BookInfoView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted_bg = cx.theme().muted;
        let muted_fg = cx.theme().muted_foreground;
        let title_label = tr(cx, "shelf.info_field_title");
        let authors_label = tr(cx, "shelf.info_field_author");

        // 那颗「保存」按得动的条件：改过，而且书名不为空。禁用而不是弹一条错误 ——
        // 按不动这件事自己就是说明，空格子旁边再写一句「不能为空」才是要补的那半句。
        let title_now = self.title.read(cx).value();
        let authors_now = self.authors.read(cx).value();
        let title_missing = title_now.trim().is_empty();
        let changed = title_now.trim() != self.saved_title.as_str()
            || authors_now.trim() != self.saved_authors.as_str();

        let back_label = tr(cx, "shelf.back");
        let back = cx.entity().clone();
        let save = cx.entity().clone();
        let has_cover = self.cover.is_some();
        v_flex()
            .id("folio-book-info")
            // 外壳那扇回归门按 id 认这一屏画没画出来。非 `test-support` 构建里这一层
            // 是透明的（`Observed` 原样转交内部元素），真机不多花一个节点。
            .test_support()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // 顶栏：返回 + 这一屏叫什么。压上来的那一页要自己带回去的路 —— 它把底栏
            // 盖掉了，而系统返回键不是所有人都知道按。那条的样子在 [`TopBar`] 里，
            // 与阅读页、紧凑设置子页共用一份（内缩跟着这一屏正文的 `GUTTER` 对齐）。
            .child(
                TopBar::new("book-info-bar")
                    .title(tr(cx, "shelf.info_title"))
                    .strong()
                    .back_tooltip(back_label)
                    .show_back_label()
                    .pad_x(GUTTER)
                    .on_back(move |_, cx| {
                        back.update(cx, |this, cx| this.pop(cx));
                    }),
            )
            .child(
                div()
                    // 这个 id 不只是给 `overflow_y_scroll` 用的（它挂在 `Stateful<Div>` 上）：
                    // gpui 的滚动位置按元素身份记，没有 id 的话每次重画都跳回顶部。
                    .id("book-info-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(GUTTER))
                    .pb_4()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(px(PANEL_WIDTH))
                            .gap_3()
                            // 封面与那两格同一档顶线：两格比封面矮得多，居中会让它看上去
                            // 浮在封面旁边。
                            .child(
                                h_flex()
                                    .w_full()
                                    .items_start()
                                    .gap_3()
                                    .child(
                                        div()
                                            .w(px(COVER_WIDTH))
                                            .flex_shrink_0()
                                            .aspect_ratio(COVER_RATIO)
                                            .rounded_md()
                                            .overflow_hidden()
                                            .bg(muted_bg)
                                            .when_some(self.cover.clone(), |this, image| {
                                                this.child(
                                                    img(image)
                                                        .size_full()
                                                        .object_fit(ObjectFit::Cover),
                                                )
                                            })
                                            // 没封面时这一格画一个书形，**不**画书名：书名就在
                                            // 右边那一格里，同一屏把同一句话摆两遍会读成两条
                                            // 信息。（架上那一格画的正是书名 —— 那里没有别处
                                            // 可摆，见 `book_card`。）
                                            .when(!has_cover, |this| {
                                                this.child(
                                                    div()
                                                        .size_full()
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .child(
                                                            Icon::new(IconName::BookOpen)
                                                                .large()
                                                                .text_color(muted_fg),
                                                        ),
                                                )
                                            }),
                                    )
                                    .child(
                                        v_flex().flex_1().min_w_0().child(
                                            v_form()
                                                .child({
                                                    let mut field = field()
                                                        .label(title_label.clone())
                                                        .w_full()
                                                        .child(
                                                            Input::new(&self.title)
                                                                .aria_label(title_label.clone())
                                                                .cleanable(true),
                                                        );
                                                    // 空书名不写回，也不让「保存」亮着 ——
                                                    // 但读者得知道为什么按不动。
                                                    if title_missing {
                                                        field = field.description(tr(
                                                            cx,
                                                            "shelf.info_title_required",
                                                        ));
                                                    }
                                                    field
                                                })
                                                .child(
                                                    field()
                                                        .label(authors_label.clone())
                                                        .w_full()
                                                        .child(
                                                            Input::new(&self.authors)
                                                                .aria_label(authors_label),
                                                        )
                                                        // 多位作者的写法不写在占位里：框里
                                                        // 已经填着现值，占位一辈子见不着。
                                                        .description(tr(
                                                            cx,
                                                            "shelf.info_author_hint",
                                                        )),
                                                ),
                                        ),
                                    ),
                            )
                            .child(hairline(cx))
                            // 这一本自带的书目，加格式与大小两行。摆在这里是为了「这本是不是
                            // 那一套的第 3 本 / 是不是那本 6.9 MB 的」这类问题当场有答案。
                            .child(
                                v_flex()
                                    .w_full()
                                    .gap_2()
                                    .children(fact_rows(&self.meta, cx)),
                            ),
                    ),
            )
            // 那颗提交钉在底下，不跟着内容滚走：改完手就在下面那一格上。
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .px(px(GUTTER))
                    .py_2()
                    .child(
                        Button::new("book-info-save")
                            .primary()
                            .label(tr(cx, "shelf.info_save").to_string())
                            .accessibility_label(tr(cx, "shelf.info_save"))
                            .disabled(!(changed && !title_missing))
                            .on_click(move |_, _, cx| save.update(cx, |this, cx| this.save(cx))),
                    ),
            )
    }
}

/// 一行作者拆回人名：按 ` · ` 切、去每段的空白、丢掉空的那几段。
///
/// 分隔符与架上那张卡并作者用的是同一条（[`AUTHOR_SEP`]），所以读者在架上看见的
/// 「甲 · 乙」在这里改完仍是两个人；只写一个人名时也得一个人。
/// 库里存的仍是换行拼接 —— 那套编码与 [`Store::add_book`](folio_core::Store::add_book)
/// 写进去的是同一个约定。
fn split_authors(line: &str) -> Vec<String> {
    line.split(AUTHOR_SEP)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(String::from)
        .collect()
}

/// 一行书目：标签占一道固定栏，值在它右边、放不下就折行。
///
/// 标签栏是固定的一道（`w_20` = 5 倍界面字号，跟着缩放走）而不是跟着字长走：这一屏一次摆
/// 十来行，错落的标签读不成一条竖线，扫一行要横向跳两次。那道栏取的是三种语言里最长的
/// 标签 —— 中文「出版日期」、英文 "Contributors"、日文「内容紹介」；再窄就要折行，再宽就
/// 把值那一栏挤瘦了。值用前景色、标签用次要色：读者要读的是值。
///
/// `field` 同时是这一行的测试 id（`book-info-<field>`）：「有值才摆那一行」这一条口径在
/// 编译期看不出来，摆错与摆漏都要靠回归门按 id 认。
fn fact(field: &'static str, label: SharedString, value: SharedString, cx: &App) -> AnyElement {
    h_flex()
        .id(format!("book-info-{field}"))
        .test_support()
        .w_full()
        .gap_2()
        .items_start()
        .child(
            div()
                .w_20()
                .flex_shrink_0()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(div().flex_1().min_w_0().child(value))
        .into_any_element()
}

/// 底下那一组书目，按读者要问的顺序排，**有值才摆那一行**。
///
/// 顺序不是都柏林核心的表序而是这里的读法：先「属于哪一套、还有谁写了它、讲什么题材、
/// 内容简介」，再「谁出的、哪一年、什么语言、哪个书号、版权」，最后才是文件自己那两行。
///
/// 空的那几项不摆，有两个理由：一本电子书通常只说其中几项，全摆就成了空栏位的清单；
/// 而一排「（空）」读出来的意思是「这本书什么都没写」，那不是这条信息。
///
/// `dc:type` 库里存着但不摆：电子书几乎全是 `text`，那一行答不出任何问题，
/// 而界面里多一个词就要多读一个词。
fn fact_rows(meta: &BookMeta, cx: &App) -> Vec<AnyElement> {
    // 多值那几项并成一行的分隔符由文案给：中文的顿号与英文的「逗号 + 空格」不是同一种
    // 约定，也不是能从别处的文案里推出来的。
    let sep = tr(cx, "shelf.info_sep");
    let list = |items: &[String]| SharedString::from(items.join(sep.as_str()));
    let mut rows: Vec<AnyElement> = Vec::new();

    if let Some(series) = &meta.series {
        // 序号有没有是这本书说的，两种写法各是一条完整文案：把「第 N 本」拼在系列名后面
        // 是拿另一语言的词序拼这一语言的句子。
        let position = series.position.trim();
        let value = if position.is_empty() {
            SharedString::from(series.name.clone())
        } else {
            SharedString::from(tf(
                cx,
                "shelf.info_series_ordinal",
                &[("name", &series.name), ("position", &position)],
            ))
        };
        rows.push(fact("series", tr(cx, "shelf.info_field_series"), value, cx));
    }
    if !meta.contributors.is_empty() {
        rows.push(fact(
            "contributors",
            tr(cx, "shelf.info_field_contributors"),
            list(&meta.contributors),
            cx,
        ));
    }
    if !meta.subjects.is_empty() {
        rows.push(fact(
            "subjects",
            tr(cx, "shelf.info_field_subjects"),
            list(&meta.subjects),
            cx,
        ));
    }
    if !meta.description.is_empty() {
        rows.push(fact(
            "description",
            tr(cx, "shelf.info_field_description"),
            meta.description.clone().into(),
            cx,
        ));
    }
    if !meta.publisher.is_empty() {
        rows.push(fact(
            "publisher",
            tr(cx, "shelf.info_field_publisher"),
            meta.publisher.clone().into(),
            cx,
        ));
    }
    if !meta.published.is_empty() {
        rows.push(fact(
            "published",
            tr(cx, "shelf.info_field_published"),
            meta.published.clone().into(),
            cx,
        ));
    }
    if !meta.language.is_empty() {
        rows.push(fact(
            "language",
            tr(cx, "shelf.info_field_language"),
            meta.language.clone().into(),
            cx,
        ));
    }
    if !meta.isbn.is_empty() {
        rows.push(fact(
            "isbn",
            tr(cx, "shelf.info_field_isbn"),
            meta.isbn.clone().into(),
            cx,
        ));
    }
    if !meta.rights.is_empty() {
        rows.push(fact(
            "rights",
            tr(cx, "shelf.info_field_rights"),
            meta.rights.clone().into(),
            cx,
        ));
    }
    rows.push(fact(
        "format",
        tr(cx, "shelf.info_format"),
        meta.format.clone().into(),
        cx,
    ));
    rows.push(fact(
        "size",
        tr(cx, "shelf.info_size"),
        fmt_size(meta.size).into(),
        cx,
    ));
    rows
}

#[cfg(test)]
mod tests {
    //! 作者那一行是这一屏唯一一处「摆出来的与写回去的不是同一个字符串」的地方，
    //! 所以由纯函数钉住：界面摆一行、库里存一列换行拼接的人名，两侧要能互相还原。

    use super::{AUTHOR_SEP, split_authors};

    /// 「甲 · 乙」拆回两个人，一个人拆回一个人，而空白与多余的分隔不造出空人名。
    ///
    /// 写歪的后果不是报错，是数据悄悄变形：把「甲 · 乙」存成一个人，架上那一行与
    /// 按作者的搜索（`Store::shelf` 那条 LIKE）会一起认错书。
    #[test]
    fn the_author_line_splits_back_into_the_people_it_was_joined_from() {
        assert_eq!(split_authors("安德烈·纪德"), ["安德烈·纪德"]);
        assert_eq!(split_authors("甲 · 乙 · 丙"), ["甲", "乙", "丙"]);
        // 人名里本身带 `·`（前后没空格）是常事：分隔是「空格 · 空格」，不该把它切开。
        assert_eq!(split_authors("弗朗索瓦·莫里亚克"), ["弗朗索瓦·莫里亚克"]);

        assert!(split_authors("").is_empty(), "空的一格就是没有作者");
        assert!(split_authors("   ").is_empty());
        assert!(split_authors(" · ").is_empty());
        assert_eq!(
            split_authors("甲 ·  · 乙"),
            ["甲", "乙"],
            "空的那一段不该算一个人"
        );
        assert_eq!(
            split_authors("  甲  · 乙  "),
            ["甲", "乙"],
            "每段各自去首尾空白"
        );

        // 往返：架上那一行怎么并起来的，这里就该怎么拆回去。
        let names = ["甲", "乙"];
        assert_eq!(split_authors(&names.join(AUTHOR_SEP)), names);
    }
}
