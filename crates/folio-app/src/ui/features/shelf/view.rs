//! 书架：顶部一行命令（搜索 + 导入），下面一片按封面排的网格。
//!
//! 两端用的是同一份架面，只有顶部那一行和一行放几本按窗口档位换：手机上是一条
//! 满幅的搜索框，桌面与平板上是右上角一组定宽命令。见 `toolbar` 与 [`columns_for`]。
//!
//! 搜索框查的是**书名与作者**（`Store::shelf`，一条 `LIKE`），不是正文。
//! 正文的全文检索是另一件事（`Store::search`，trigram FTS，短于 3 个字命中不了）：
//! 书架上这颗框在读者眼里是「找我那本书」，输入两个字就该立刻把书名对得上的那几本
//! 列出来 —— 把两件事混在一个入口会让短查询静默返回空。
//!
//! 封面走 gpui 的 `img` 元素，纹理上传由它负责（正文里的 `<img>` 用的是同一条路径，
//! 见 `reader_view`）。没有封面的书**必须**照样出现在架上并占住一格，
//! 否则「导入了一本但看不见」和「根本没导进来」在真机上没法分开。

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use folio_core::Store;
use folio_core::store::ShelfItem;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FontWeight, Image,
    InteractiveElement, IntoElement, ObjectFit, ParentElement, PathPromptOptions, Render,
    SharedString, StatefulInteractiveElement, Styled, StyledImage, Subscription, Task, WeakEntity,
    Window, div, img, px,
};

use crate::ui::shared::config_model::ConfigHolder;
use crate::ui::shared::images::cover_image;
use crate::ui::shared::l10n::{tf, tr};
use crate::ui::shared::library::{delete_book, stage, suggested_dir, target_dir};
use crate::ui::shell::navigation::{RAIL_WIDTH, SIDEBAR_WIDTH, SizeClass};
use crate::ui::widgets::{empty_state, hairline, muted_label};

/// 手机上一行放几本。
///
/// 3 是在 1080×2400 / scale 2.8 那块屏上量的：可用宽约 386 逻辑像素，三列扣掉格间距
/// 后每格约 112，封面 2:3 就是 168 高。两列会让书名只看得下三个字，四列低于拇指
/// 能稳定命中的最小格宽（56）。
///
/// 桌面与平板不按这个数，见 [`columns_for`]。
const COLUMNS: u16 = 3;

/// 网格间距，与 `grid` 的 `gap_2` 同一档（默认 rem 下 8 逻辑像素）。
const GAP: f32 = 8.0;

/// 桌面上一格封面的最小宽度（逻辑像素）。
///
/// 比手机的 112 宽：鼠标桌面上书名要一眼读完，而不是靠点进去看，而一格多宽，
/// 标题那一行就能塞下几个字。再往下加一列会开始把长书名截成看不懂的半句。
const MIN_CARD: f32 = 168.0;

/// 封面格的宽高比（宽 ÷ 高），出版物的通行比例。
///
/// 行高由它定而不是由图片自己定：图片的真实比例五花八门（方图、长条都有），
/// 让每格按图算高会得到一片参差的边。统一裁成这一档，多出来的部分由
/// `ObjectFit::Cover` 裁掉。
const COVER_RATIO: f32 = 2.0 / 3.0;

/// 网格与搜索行两侧的留白（逻辑像素）。和顶部那条章导航的 `px_3` 不同一档，
/// 因为这里是整片卡片的边，视觉上要比一条文字栏松。
const GUTTER: f32 = 12.0;

/// 一行放几本：手机固定 [`COLUMNS`]，桌面与平板按**内容区实际宽度**排到每格
/// 不低于 [`MIN_CARD`]。
///
/// 3 列写死在桌面上是不成立的：1600 宽的窗口会给出三格 440 像素的封面，
/// 一屏只看得见两本书 —— 而「我的书都在哪儿」正是架面要回答的问题。
///
/// 导航栏的宽度从 `shell::navigation` 取：那两个常数是阅读页预留版心用的同一份，
/// 在这里再写一遍数字的话，改一侧忘另一侧只会表现为「书架的格子宽得莫名其妙」。
///
/// `pub` 只为测试能钉住这三档的列数 —— 列数算错不会报错，只会让架面在某个窗口宽上
/// 突然变成一格一本。
pub fn columns_for(width: f32, size: SizeClass) -> u16 {
    if size.is_compact() {
        return COLUMNS;
    }
    let nav = match size {
        SizeClass::Compact => 0.0,
        SizeClass::Medium => RAIL_WIDTH,
        SizeClass::Expanded => SIDEBAR_WIDTH,
    };
    // 第 n 格要占 n 个格宽加 n−1 条间距，所以分子补回一条 GAP。
    let available = (width - nav - GUTTER * 2.0).max(0.0);
    let fitted = ((available + GAP) / (MIN_CARD + GAP)).floor() as u16;
    fitted.max(1)
}

/// 书架向外层报的两件事，都带书库主键而不是路径 —— 选择器给的那份路径是临时的，
/// 书的内容早就在库里了。
pub enum ShelfEvent {
    /// 点了一张卡片。
    Open(i64),
    /// 删掉了一本。外壳需要这条：正文可能正开着这一本，书没了，那一层就该退回架面。
    Deleted(i64),
}

/// 一张卡片：库行摊平到界面直接用的样子 —— 封面已解码，作者已并成一行。
struct Card {
    id: i64,
    title: SharedString,
    authors: SharedString,
    cover: Option<Arc<Image>>,
}

pub struct ShelfView {
    /// 书库句柄，来自 [`crate::ui::shared::app_store`]。放 `None` 时架面是空的，
    /// 但界面照常起得来。
    store: Option<Rc<Store>>,
    items: Vec<Card>,
    /// 上一次查库用的串。
    query: SharedString,
    search: Entity<InputState>,
    /// 架面上是不是站着那块「书库位置」确认面板。
    ///
    /// 导入会把文件**拷一份**进硬盘，所以位置要先说定。没定过的时候点导入不开选择器，
    /// 而是就地换成这块面板 —— 用普通按钮而不是浮层：浮层（长按菜单那条路）在
    /// Android 上现在点不住，见 `docs/known-limitations.md`。
    confirming: bool,
    /// 封面解码结果，按书的主键存着。
    ///
    /// 键用 `id` 而不是每次现解：改一个搜索词会把同一本书再查出来一遍，
    /// 没缓存的话每敲一个字都要把整个架面的 JPEG 重解一遍。
    /// `None` 也要存 —— 不记住这次失败，每查一次都会再试一遍同样的失败。
    covers: HashMap<i64, Option<Arc<Image>>>,
    _search_watch: Subscription,
    /// 导入那条异步链。Task 一 drop 任务就取消，于是「选了文件但界面已经忘了」。
    _import: Option<Task<()>>,
}

impl ShelfView {
    pub fn boot(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = crate::ui::shared::app_store::handle(cx);
        if store.is_none() {
            log::error!("ShelfView: 书库没装，架面会是空的");
        }
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr(cx, "shelf.search_placeholder").to_string())
        });
        let _search_watch = cx.subscribe(&search, |this, state, event, cx| {
            if matches!(event, InputEvent::Change) {
                let query = state.read(cx).value();
                this.reload(&query, cx);
            }
        });
        let mut this = Self {
            store,
            items: Vec::new(),
            query: SharedString::default(),
            search,
            confirming: false,
            covers: HashMap::new(),
            _search_watch,
            _import: None,
        };
        this.reload(&SharedString::default(), cx);
        this
    }

    /// 查一遍库换成卡片。查不动时保留上一次的架面而不是清空：
    /// 一条语句报错不该让读者的书看起来消失了。
    fn reload(&mut self, query: &str, cx: &mut Context<Self>) {
        if self.query == query && !self.items.is_empty() {
            return;
        }
        let queried = self.store.as_ref().map(|store| store.shelf(query));
        match queried {
            Some(Ok(items)) => {
                self.query = query.into();
                self.items = items.into_iter().map(|item| self.card(item)).collect();
            }
            Some(Err(err)) => log::error!("ShelfView: 查书架失败 {err}"),
            None => {
                self.query = query.into();
                self.items.clear();
            }
        }
        cx.notify();
    }

    /// 一条库行 → 一张卡。封面没解过才解，解不出的那本记住 `None`。
    fn card(&mut self, item: ShelfItem) -> Card {
        let cover = match self.covers.get(&item.id) {
            Some(cached) => cached.clone(),
            None => {
                let decoded = item.cover.as_deref().and_then(cover_image);
                self.covers.insert(item.id, decoded.clone());
                decoded
            }
        };
        Card {
            id: item.id,
            title: item.title.into(),
            authors: item.authors.join(" · ").into(),
            cover,
        }
    }

    /// 点「导入」。书库位置没确认过就不开选择器，先把那块面板摆出来。
    fn import(&mut self, cx: &mut Context<Self>) {
        if target_dir(&ConfigHolder::library(cx)).is_some() {
            self.open_picker(cx);
            return;
        }
        self.confirming = true;
        cx.notify();
    }

    /// 确认「书就拷到这里」。写进配置并落盘（外壳订阅了配置实体，落盘由它做），
    /// 然后接着把选择器打开 —— 读者点导入要的是导书，不是填表格。
    fn confirm_library(&mut self, cx: &mut Context<Self>) {
        let dir = suggested_dir();
        ConfigHolder::edit_library(cx, |library| {
            library.dir = dir.to_string_lossy().into_owned()
        });
        self.confirming = false;
        self.open_picker(cx);
    }

    fn cancel_confirm(&mut self, cx: &mut Context<Self>) {
        self.confirming = false;
        cx.notify();
    }

    /// 打开系统文件选择器选书。选中的每个文件先复制进书库再入库。
    ///
    /// **先复制**不是多余的一步：选择器给的是它自己临时目录里的路径，系统随时会清，
    /// 而 `books.path` 那一列是书的身份（同路径重复导入算更新，不算第二本书）。
    fn open_picker(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        self._import = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let paths = match receiver.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) => return,
                Ok(Err(err)) => {
                    log::error!("ShelfView: 文件选择器失败 {err}");
                    return;
                }
                Err(_) => return,
            };
            let _ = this.update(cx, |this, cx| {
                let Some(dir) = target_dir(&ConfigHolder::library(cx)) else {
                    log::error!("ShelfView: 书库位置没确认，选中的文件没有导入");
                    return;
                };
                for path in &paths {
                    this.import_one(path, &dir);
                }
                let query = this.query.clone();
                // 新入库的书要立刻上架，但搜索词得留着：读者可能正搜到一半。
                // `reload` 的去重按「词没变且架面非空」判，所以先把它清空逼它重查。
                this.items.clear();
                this.reload(&query, cx);
            });
        }));
    }

    /// 一个选中的文件 → 架上的一本。失败只写日志：一次选多本时，坏掉的那本
    /// 不该把其余的也挡在架外。
    fn import_one(&mut self, source: &Path, dir: &Path) {
        let Some(store) = &self.store else {
            log::error!("ShelfView: 没有书库，跳过 {source:?}");
            return;
        };
        let dest = match stage(source, dir) {
            Ok(dest) => dest,
            Err(err) => {
                log::error!("ShelfView: 复制 {source:?} 到 {dir:?} 失败 {err}");
                return;
            }
        };
        let (meta, flow) = match crate::ui::shared::document::open(&dest) {
            Ok(doc) => doc,
            Err(err) => {
                log::error!("ShelfView: 打开 {dest:?} 失败 {err}");
                return;
            }
        };
        let book_id = match store.add_book(&dest, &meta) {
            Ok(id) => id,
            Err(err) => {
                log::error!("ShelfView: 入库失败 {err}");
                return;
            }
        };
        if let Err(err) = store.save_flow(book_id, &flow) {
            log::error!("ShelfView: 章节入库失败 {err}");
            return;
        }
        // 封面可能跟着这次导入换了；缓存里那条旧的要清掉。
        self.covers.remove(&book_id);
        log::info!("ShelfView: 导入 {dest:?}");
    }

    /// 删掉一本：库里的行、属于我们自己的那份副本，然后重查一次架面。
    ///
    /// 「哪些文件算我们自己的」这条判据在 [`delete_book`] 里，不在这里：桌面端记的是
    /// 读者手里的原文件，和移动端的副本在库里长得一模一样，只有位置能分开它们，
    /// 而删错的那一头是不可逆的。
    fn remove(&mut self, book_id: i64, cx: &mut Context<Self>) {
        let Some(store) = &self.store else {
            log::error!("ShelfView: 没有书库，删不掉第 {book_id} 本");
            return;
        };
        if let Err(err) = delete_book(
            store,
            book_id,
            target_dir(&ConfigHolder::library(cx)).as_deref(),
        ) {
            log::error!("ShelfView: 删除第 {book_id} 本失败 {err}");
            return;
        }
        self.covers.remove(&book_id);
        cx.emit(ShelfEvent::Deleted(book_id));
        // 清空是为了绕过 `reload` 的去重：搜索词没变，但架面少了一本，必须重查一次。
        self.items.clear();
        let query = self.query.clone();
        self.reload(&query, cx);
    }

    /// 搜索框那一行：框占满剩下的宽度，导入按钮钉在右边。
    fn search_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        let import_label = tr(cx, "shelf.import");
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px(px(GUTTER))
            .py_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.search).prefix(Icon::new(IconName::Search).small())),
            )
            .child(
                Button::new("shelf-import")
                    .ghost()
                    .small()
                    .icon(IconName::Plus)
                    .label(import_label.clone().to_string())
                    .accessibility_label(import_label)
                    .on_click(move |_, _, cx| entity.update(cx, |this, cx| this.import(cx))),
            )
    }

    /// 桌面/平板的工具栏：钉在架面最上方，搜索框与导入靠右。
    ///
    /// 不复用手机上那一行，因为两者要回答的问题不同：手机上宽度只够一条满幅的框，
    /// 框被拉满就是「先搜」；桌面上把同一颗框拉到 1300 像素宽，它就从一条命令
    /// 变成一块空白。所以这里给它一个和「书名或作者」这句话相称的定宽，
    /// 与导入一起收成右上角的一组命令。
    ///
    /// 左右留白与网格共用 [`GUTTER`]：两行不共用一条对齐脊线的话，
    /// 搜索框的左边会莫名其妙地比第一格封面缩进一截。
    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        let import_label = tr(cx, "shelf.import");
        h_flex()
            .w_full()
            .flex_shrink_0()
            .items_center()
            .justify_end()
            .gap_2()
            .px(px(GUTTER))
            .py_2()
            .child(
                // 工具栏按 compact 档排：框和按钮同一档，否则一行里会长出两种高度。
                div().w_64().child(
                    Input::new(&self.search)
                        .small()
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).small()),
                ),
            )
            .child(
                // 工具栏里的一条普通命令用 default 档，不用 primary：
                // primary 表达的是「这一组决定里回车要走的那一条」，而这里没有决定。
                Button::new("shelf-import")
                    .small()
                    .icon(IconName::Plus)
                    .label(import_label.clone().to_string())
                    .accessibility_label(import_label)
                    .on_click(move |_, _, cx| entity.update(cx, |this, cx| this.import(cx))),
            )
    }

    /// 「书要拷到哪儿」那块面板。
    ///
    /// 它是架面的一部分而不是浮层：浮层在 Android 上现在点不住（长按菜单那条路的缺陷），
    /// 而这一步是导入的前提，点不动就等于没有导入。
    fn confirm_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let confirm_entity = cx.entity().clone();
        let cancel_entity = confirm_entity.clone();
        let confirm_label = tr(cx, "library.confirm");
        let cancel_label = tr(cx, "library.cancel");
        v_flex()
            .id("library-confirm-panel")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_3()
            .px(px(GUTTER))
            .py_4()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr(cx, "library.title")),
            )
            .child(muted_label(tr(cx, "library.hint"), cx))
            .child(
                div()
                    .w_full()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(cx.theme().muted)
                    .text_color(cx.theme().muted_foreground)
                    .child(suggested_dir().display().to_string()),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Button::new("library-confirm")
                            .primary()
                            .label(confirm_label.clone().to_string())
                            .accessibility_label(confirm_label)
                            .on_click(move |_, _, cx| {
                                confirm_entity.update(cx, |this, cx| this.confirm_library(cx))
                            }),
                    )
                    .child(
                        Button::new("library-cancel")
                            .ghost()
                            .label(cancel_label.clone().to_string())
                            .accessibility_label(cancel_label)
                            .on_click(move |_, _, cx| {
                                cancel_entity.update(cx, |this, cx| this.cancel_confirm(cx))
                            }),
                    ),
            )
            .into_any_element()
    }

    /// 网格本体，或者两种「这里没有东西」—— 它们必须说得不一样。
    fn body(&self, cx: &mut Context<Self>, columns: u16) -> AnyElement {
        if self.confirming {
            return self.confirm_panel(cx);
        }
        if !self.items.is_empty() {
            return self.grid(cx, columns).into_any_element();
        }
        // 空架要说「导一本书进来」，有书但都不匹配要说「换个词」：
        // 写成一句会让读者以为自己把书弄丢了。
        let (title, note) = if self.query.is_empty() {
            ("shelf.empty", "shelf.empty_hint")
        } else {
            ("shelf.no_results", "shelf.no_results_hint")
        };
        empty_state(tr(cx, title), tr(cx, note), cx).into_any_element()
    }

    fn grid(&self, cx: &mut Context<Self>, columns: u16) -> impl IntoElement {
        let entity = cx.entity().clone();
        let placeholder_bg = cx.theme().muted;
        let muted_fg = cx.theme().muted_foreground;
        let cards: Vec<AnyElement> = self
            .items
            .iter()
            .map(|card| {
                let book_id = card.id;
                let title = card.title.clone();
                let authors = card.authors.clone();
                let cover = card.cover.clone();
                let has_cover = cover.is_some();
                let tapped = entity.clone();
                let menu_entity = entity.clone();
                // 菜单上写的是书名而不是光一个「删除」：长按之后手指已经离开卡片，
                // 只有标题能让读者确认自己按的是哪一本。
                let delete_label: SharedString =
                    tf(cx, "library.delete", &[("title", &card.title)]).into();
                div()
                    // 元素 id 用书库主键而不是标题：重新查库它不变，而标题会随导入更新，
                    // 拿标题当键会让一张卡片每次改名都丢掉自己的按下态。
                    .id(format!("shelf-card-{book_id}"))
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .on_click(move |_, _, cx| {
                        tapped.update(cx, |_, cx| cx.emit(ShelfEvent::Open(book_id)))
                    })
                    .child(
                        div()
                            .w_full()
                            .aspect_ratio(COVER_RATIO)
                            .rounded_md()
                            .overflow_hidden()
                            .bg(placeholder_bg)
                            .when_some(cover, |this, image| {
                                this.child(img(image).size_full().object_fit(ObjectFit::Cover))
                            })
                            // 没封面的那格把书名当封面画：一整格空白看起来像这本坏了，
                            // 而它只是没有图。
                            .when(!has_cover, |this| {
                                this.child(
                                    div()
                                        .size_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .px_2()
                                        .text_center()
                                        .text_color(muted_fg)
                                        .child(title.clone()),
                                )
                            }),
                    )
                    .child(
                        div()
                            .w_full()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(title),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_xs()
                            .text_color(muted_fg)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(authors),
                    )
                    // 长按开菜单。框架里 `ContextMenuExt` 就是这么设计的：手指没有右键，
                    // 所以长按走的是右键那条路；而且长按一旦被菜单认领，这一次触摸就
                    // 不会再派发成 tap —— 不会出现「长按想删书，顺手把书打开了」。
                    .context_menu(move |menu, _, _| {
                        let entity = menu_entity.clone();
                        menu.item(
                            PopupMenuItem::new(delete_label.clone())
                                .icon(IconName::Trash)
                                .on_click(move |_, _, cx| {
                                    entity.update(cx, |this, cx| this.remove(book_id, cx))
                                }),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        div()
            .id("shelf-grid")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .grid()
                    .grid_cols(columns)
                    .gap_2()
                    .px(px(GUTTER))
                    .pt_2()
                    .pb(px(GUTTER))
                    .w_full()
                    .children(cards),
            )
    }
}

impl EventEmitter<ShelfEvent> for ShelfView {}

impl Render for ShelfView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 顶部那一行按窗口档位换：手机上满幅的搜索框是「先搜再翻」，桌面上同一颗框
        // 拉到整片内容宽就成了一块空白，所以那边是定宽搜索框 + 导入的工具栏。
        let size = SizeClass::of(window);
        let top = if size.is_compact() {
            self.search_bar(cx).into_any_element()
        } else {
            self.toolbar(cx).into_any_element()
        };
        let columns = columns_for(f32::from(window.viewport_size().width), size);
        v_flex()
            .id("folio-shelf")
            .size_full()
            .child(top)
            .child(hairline(cx))
            .child(self.body(cx, columns))
    }
}

#[cfg(test)]
mod tests {
    //! 列数这件事是架面最容易在改宽度时悄悄坏掉的一处，所以纯函数直接钉。

    use super::columns_for;
    use crate::ui::shell::navigation::SizeClass;

    /// 架面一行放几本：手机钉在 3，桌面与平板随窗口宽往上加，任何宽度都不许塌成 0 列。
    ///
    /// 列数写死在 3 上是桌面书架最先坏掉的一处：1600 宽的窗口会给出三格 440 像素的封面，
    /// 一屏只看得见两本书，而「我的书都在哪儿」正是架面要回答的问题。反过来在最小窗口
    /// （520）上按同一公式硬塞，长出来的会是零宽的一格。两种都只在渲染出来之后看得见。
    #[test]
    fn the_shelf_grid_grows_with_the_window() {
        // 手机：386 逻辑像素可用宽上量出来的 3 列，不随窗口继续变。
        assert_eq!(columns_for(390.0, SizeClass::Compact), 3);
        assert_eq!(columns_for(430.0, SizeClass::Compact), 3);

        // 桌面最小窗口（图标栏档）：两格，宁可小也不许没有。
        assert_eq!(columns_for(520.0, SizeClass::Medium), 2);
        // 每格都不窄于 168：再加一列就要挤破这个下限时，宁可少一列。
        assert_eq!(columns_for(776.0, SizeClass::Medium), 4);
        assert_eq!(columns_for(1600.0, SizeClass::Expanded), 7);

        // 同一档内加宽不会反而少一列。
        for (width, size) in [
            (600.0, SizeClass::Medium),
            (700.0, SizeClass::Medium),
            (900.0, SizeClass::Expanded),
            (1200.0, SizeClass::Expanded),
            (2000.0, SizeClass::Expanded),
        ] {
            let at = columns_for(width, size);
            assert!(at >= 1, "{width} 宽不该一行放 0 本");
            assert!(
                columns_for(width + 1.0, size) >= at,
                "{width} 处加宽反而少了一列：{at} → {}",
                columns_for(width + 1.0, size)
            );
        }
    }
}
