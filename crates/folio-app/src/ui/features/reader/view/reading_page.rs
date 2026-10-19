//! 阅读页：正文 + 干净态的两行小字 + 点正文中间浮出的顶栏与底栏。
//!
//! 形态照 Reeden 的阅读界面量出来（1080×2400 @3.0）：干净态只剩左上角一行章名与底部
//! 一行进度；点正文中间才浮出顶栏（返回 + 书名）与底栏（进度条 + 一排入口）。
//! 入口就地换面板，**不弹浮层** —— 浮层在 Android 上现在点不住（同 `shelf` 里
//! 「书要拷到哪儿」那块面板记的那条事实），而目录点不动等于没有目录。
//!
//! 这一屏两端**共用一份**：桌面上原来的工具条 + 状态栏那套常驻 chrome 换成了这个形态，
//! 因为「以手机那一版为准」不只是挑一套皮肤 —— 两套 chrome 意味着每个新功能要落两次地，
//! 而其中一端一定会漏。桌面多出来的那一格是「双页」，它按宽度自己退档（见
//! [`chrome::PageMode::effective`]），所以这一格在窄窗口里就是分页，不需要 `cfg`。
//!
//! 顶栏与底栏**压在正文之上**，不推动正文重排：读者点一下屏幕，字不该跳走。
//! 正文只让出干净态那两条带的高度，那两个数住在 [`chrome`] 里，布局与视口算法共用它。
//!
//! 点击分区、面板档位、模式退档都在 [`chrome`] 里判：那里是纯函数，宿主的 `cargo test`
//! 直接跑，这一屏只把结果落成动作。

use folio_core::infra::config::{Mode, PageTurn};
use gpui_kit::base::{NavMotion, NavStackState};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{ActiveTheme, ThemeMode, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, ClickEvent, Context, Entity, FocusHandle, Hsla,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, StatefulInteractiveElement,
    Styled, Subscription, WeakEntity, Window, div, px,
};

use crate::ui::features::reader::components::image_viewer::{self, ImageViewer};
use crate::ui::features::reader::components::toc_panel::{TocEvent, TocPanel};
use crate::ui::features::reader::model::chrome::{
    self, BRIGHTNESS_RANGE, Chrome, Effect, FONT_SIZE_RANGE, Panel, Reserve, zone,
};
use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::features::reader::view::bands;
use crate::ui::features::settings::CompactSettings;
use crate::ui::platform;
use crate::ui::services::config::ConfigHolder;
use crate::ui::shell::AppShell;
use crate::ui::shell::navigation::SizeClass;

/// 脚注小纸片的最宽那一条：内容的 `max_w` 与夹边算的是同一个数。
const FOOTNOTE_CARD_W: Pixels = px(300.);
/// 弹层自己那层内边距的**上估**。算少了卡片会压到屏外，算多了只是偏进来一点 —— 宁可偏进来。
const FOOTNOTE_PAD: Pixels = px(12.);
/// 卡片离屏边最少留这么多。
const FOOTNOTE_EDGE: Pixels = px(8.);
/// 标记下缘与卡片之间的缝。
const FOOTNOTE_GAP: Pixels = px(10.);

pub struct ReadingPage {
    pub(in crate::ui::features::reader) reader: Entity<ReaderSession>,
    /// 外壳报来的「不属于正文的那一段」。顶层盖满整窗，所以那一段只剩顶上那条系统状态栏 /
    /// 挖孔（底边见 [`ReadingPage::bottom_inset`]）—— `shell::reading_reserve`。
    pub(in crate::ui::features::reader) reserve: Reserve,
    /// 顶上那条系统状态栏 / 挖孔占掉多高（逻辑像素）。见 [`shell::reading_top_inset`]。
    ///
    /// 整层已经由外壳按这一个数往下推过了，这里只给顶栏用：那条的表面要**再往上长回屏幕顶边**，
    /// 不然它投影的上半截会露在条子上方，被条子自己的边切成一条横线（见 [`TopBar::overlay`]）。
    ///
    /// [`shell::reading_top_inset`]: crate::ui::shell::reading_top_inset
    pub(in crate::ui::features::reader) top_inset: f32,
    /// 底下那条系统手势条占掉多高（逻辑像素）。
    ///
    /// 它**不拿去推整层**，只给底栏卡片与干净态页脚用：那两处把内容抬高 `bottom_inset`、
    /// 底色铺到屏幕底，所以卡片与屏幕底之间不是一条空缝（与读者那句「位于系统底部栏之上、
    /// 颜色保持一致」同一条口径）。正文那边已经由 `reserve.height` 扣过它，不再重复扣。
    pub(in crate::ui::features::reader) bottom_inset: f32,
    /// 这一层自己的返回栈（弱句柄）。**正文是栈底**，从阅读页压进去的那一屏（设置）叠在
    /// 它上面 —— 借架面那条栈的话，压进来的那一屏会被这一层盖住。
    ///
    /// 弱是因为栈里存着正文自己的 `AnyView`：两边都攥紧就是互相拽着，这一层放手之后
    /// 两边都散不掉。
    pub(in crate::ui::features::reader) stack: WeakEntity<NavStackState>,
    /// 拿着这一层的外壳（同样是弱句柄：外壳攥着这一层，这一层不该反过来攥着外壳）。
    /// 顶栏那颗返回要请它收起这一层 —— 收层是外壳的事，正文这一屏不知道自己盖在谁上面。
    pub(in crate::ui::features::reader) shell: WeakEntity<AppShell>,
    /// 浮层的开合与面板档位。见 [`chrome`]。
    pub(in crate::ui::features::reader) chrome: Chrome,
    /// 进度滑条。`Slider` 是有状态组件，句柄必须跨帧持有 —— 每帧新建会把拖动打断。
    pub(in crate::ui::features::reader) progress: Entity<SliderState>,
    pub(in crate::ui::features::reader) font_size: Entity<SliderState>,
    /// 亮度滑条。它写的是 `appearance.brightness` 那一格（不在 `Reader` 里：压暗不该让任何
    /// 一章重排）；把这一格递给平台的是外壳每一帧那一次问（`AppShell::reading_brightness`），
    /// 这一屏只管教滑条自己。
    pub(in crate::ui::features::reader) brightness: Entity<SliderState>,
    /// 目录那一屏。整棵树、折叠集、搜索、弹层档位、书头都装在它自己那个实体里
    /// （见 [`TocPanel`]），这一屏只留一个句柄：开合状态要报给会话（身上压着东西时
    /// 正文的手势让位），元素要挂在最后（谁在上面由入树顺序定）。
    pub(in crate::ui::features::reader) toc: Entity<TocPanel>,
    /// 图片放大那一层的状态。它画在**最上面**（要盖住顶栏底栏与那块弹层），而写它的
    /// 是会话里那一次点击 / 那一次长按 —— 它不是视图，改动靠下面那次 `observe` 才
    /// 收得到重画，形状与两块弹层一致。
    pub(in crate::ui::features::reader) image_viewer: Entity<ImageViewer>,
    /// 查看器那一层的焦点句柄。它拿在阅读页手里而不是查看器自己里：那一层是 `overlay`
    /// 每帧造的元素树里的 `track_focus`，句柄得属于**这一棵**元素树的那个视图，
    /// 方向键那几条事件才能沿祖先链派发到它（见 `image_viewer::overlay`）。
    pub(in crate::ui::features::reader) image_viewer_focus: FocusHandle,
    pub(in crate::ui::features::reader) _watches: Vec<Subscription>,
}

impl ReadingPage {
    pub fn new(
        stack: WeakEntity<NavStackState>,
        shell: WeakEntity<AppShell>,
        cx: &mut Context<Self>,
    ) -> Self {
        let reader = cx.new(|cx| ReaderSession::boot(cx));
        // 三条滑条的初值先取出来：`cx.new` 要 `&mut cx`，而把 `cx` 再关进构造闭包里就是
        // 同一个 `Context` 的二次可变借用。
        let prefs = ConfigHolder::reader(cx);
        // 「跟随系统」在滑条上就是推到顶：配置里那一格是 `None`，滑条上得有个能显示的数。
        let brightness = ConfigHolder::appearance(cx)
            .brightness
            .unwrap_or(BRIGHTNESS_RANGE.1);
        let progress = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(1.0)
                .step(0.001)
                .default_value(0.0)
        });
        let font_size = cx.new(|_| {
            SliderState::new()
                .min(FONT_SIZE_RANGE.0)
                .max(FONT_SIZE_RANGE.1)
                .step(0.5)
                .default_value(prefs.font_size)
        });
        let brightness_state = cx.new(|_| {
            SliderState::new()
                .min(BRIGHTNESS_RANGE.0)
                .max(BRIGHTNESS_RANGE.1)
                .step(0.01)
                .default_value(brightness)
        });
        // 三条滑条各订阅一次：拖动当场生效（正文滚到那一处 / 换字号重排 / 换背光），
        // 所以这里没有「应用」按钮，也没有改了不生效的中间态。
        let mut watches = vec![
            cx.subscribe(&progress, |this: &mut Self, _, event, cx| {
                if let SliderEvent::Change(value) = event {
                    this.reader
                        .update(cx, |reader, cx| reader.seek_ratio(value.end(), cx));
                }
            }),
            cx.subscribe(&font_size, |_, _, event, cx| {
                if let SliderEvent::Change(value) = event {
                    let value = value.end();
                    ConfigHolder::edit_reader(cx, |prefs| prefs.font_size = value);
                }
            }),
            cx.subscribe(&brightness_state, |_, _, event, cx| {
                if let SliderEvent::Change(value) = event {
                    // 顶格归成 `None`：配置里于是只有「跟随系统」一个写法，滑条与文件不会是
                    // 两套「最大亮度」（同 `Appearance::normalized` 那一条）。
                    let value = value.end();
                    ConfigHolder::edit_appearance(cx, |prefs| {
                        prefs.brightness = if value >= BRIGHTNESS_RANGE.1 {
                            None
                        } else {
                            Some(value)
                        }
                    });
                }
            }),
        ];
        // 目录那一屏自己的实体：它要观察它（弹层的开合与换档都得重画这一屏，
        // `layers_up` 那个数与顶栏底栏的挂载都读它），也要听它报回来的两件事。
        let toc = cx.new(|cx| TocPanel::new(reader.clone(), cx));
        watches.push(cx.observe(&toc, |_, _, cx| cx.notify()));
        watches.push(cx.subscribe(&toc, |this, _, event, cx| match event {
            TocEvent::Navigated => {
                // 跳到哪一章就把浮层整个收干净：读者要看的正是跳到的那几行字。
                this.chrome = Chrome::default();
                cx.notify();
            }
            TocEvent::OpenSettings => this.open_settings(cx),
        }));
        // 会话不是这一屏的子视图以外的东西，但脚注弹层长在**这一屏**的元素树上：
        // 会话里那一次「点中脚注」只把状态写进它自己并 `notify`，而重画这一屏要有人
        // 转一道。下面这次订阅就是那道转发 —— 会话无论为什么重画，这一屏跟着重画。
        watches.push(cx.observe(&reader, |_, _, cx| cx.notify()));
        // 图片放大那一层：状态挂在这一屏（它盖的是整块窗口），句柄交给会话，
        // 会话那一侧的点击与长按只管往里写「要看这张图」。
        let image_viewer = cx.new(|_| ImageViewer::default());
        watches.push(cx.observe(&image_viewer, |_, _, cx| cx.notify()));
        // 查看器那一层的焦点句柄。它拿在阅读页手里而不是查看器自己里：这一层是
        // `overlay` 每帧造的元素树里的 `track_focus`，句柄要属于**这一棵**元素树的
        // 那个视图（`reading-page`），方向键那几条事件才能沿祖先链派发到它。
        let image_viewer_focus = cx.focus_handle();
        reader.update(cx, |reader, _| {
            reader.set_image_viewer(image_viewer.clone())
        });
        Self {
            reader,
            reserve: Reserve::ZERO,
            top_inset: 0.0,
            bottom_inset: 0.0,
            stack,
            shell,
            chrome: Chrome::default(),
            progress,
            font_size,
            brightness: brightness_state,
            toc,
            image_viewer,
            image_viewer_focus,
            _watches: watches,
        }
    }

    pub fn open(&mut self, book_id: i64, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.open_book(book_id, cx));
    }

    /// 落库。外壳在 pop 之前调它。
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.reader.update(cx, |reader, _cx| {
            // 先把挂着的那一趟翻页放下。退场那几百毫秒里这一层的时钟不再续帧，`turn` 会冻在
            // 松手前那一刻 —— 于是仿真翻页那张网格把一片纸背留在书架上（2026-10-19 真机撞见）。
            reader.turn = None;
            reader.persist_position();
            // 位置与时长是同一次离席要结的两笔账：漏掉后一笔，这一段就只能靠
            // 那条时钟在五分钟之内结掉，而读者往往在五分钟之内就再没回来过。
            reader.end_reading();
        });
    }

    /// 只结位置那一笔：外壳在这一层还在、但窗口刚失去焦点时调（Android 切后台之后进程
    /// 随时可能被回收，那是最后一个可靠的机会）。时长那一笔不结 —— 它有自己的 60 秒
    /// 时钟，把一段还没走完的段落在这里结掉只会把它切碎。
    pub fn flush_position(&mut self, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, _cx| reader.persist_position());
    }

    pub fn book_id(&self, cx: &App) -> Option<i64> {
        self.reader.read(cx).book_id()
    }

    pub fn set_reserve(&mut self, reserve: Reserve, cx: &mut Context<Self>) {
        self.reserve = reserve;
        self.reader
            .update(cx, |reader, cx| reader.set_reserve(reserve, cx));
    }

    /// 顶上那条系统边（状态栏 / 挖孔）有多高。外壳每帧报（没装安全区时是 0），与它推整层
    /// 用的是同一个数。
    ///
    /// 变化时重画：顶栏表面向上长的那一段读它。
    pub fn set_top_inset(&mut self, top: f32, cx: &mut Context<Self>) {
        if (self.top_inset - top).abs() > 0.5 {
            self.top_inset = top;
            cx.notify();
        }
    }

    /// 系统手势条那一条有多高。外壳每帧报（没装安全区时是 0）。
    ///
    /// 变化时重画：底栏卡片的内缩与干净态页脚的高度都读它。
    pub fn set_bottom_inset(&mut self, bottom: f32, cx: &mut Context<Self>) {
        if (self.bottom_inset - bottom).abs() > 0.5 {
            self.bottom_inset = bottom;
            cx.notify();
        }
    }

    /// 浮层挡着的时候，系统返回先收它；收干净了才轮到 pop 这一页。
    ///
    /// 返回 `true` 表示这次返回已被浮层消费，调用方不该再往下走。
    ///
    /// 收的顺序与摆的顺序相反：图片放大那一层在最上面，先收它；再收笔记便签、脚注、
    /// 目录，最后才是顶栏底栏。
    /// 焦点的归还不在这里做 —— 这条路上没有 `&mut Window`（`App::on_action` 的派发里窗口是
    /// 借着的），弹层自己在下一帧的 `render` 里还。
    pub fn dismiss_chrome(&mut self, cx: &mut Context<Self>) -> bool {
        if self.image_viewer.read(cx).is_open() {
            self.image_viewer.update(cx, |viewer, _| viewer.close());
            cx.notify();
            return true;
        }
        // 笔记便签排在最上面那一档：它自带遮罩，先收它，再轮到脚注与目录。
        if self.reader.read(cx).note_editor_open() {
            self.reader
                .update(cx, |reader, cx| reader.cancel_note_editor(cx));
            cx.notify();
            return true;
        }
        // 脚注弹层在最上面那一档里，先收它；再收目录那块弹层；最后才是顶栏底栏。
        // 「⋯」那条菜单是 gpui-kit 自己的 `PopupMenu`，返回键那一下由它自己的
        // `Cancel` 绑键吃掉，不走这里。
        if self.reader.read(cx).footnote().is_some() {
            self.reader
                .update(cx, |reader, cx| reader.dismiss_footnote(cx));
            cx.notify();
            return true;
        }
        if self.toc.read(cx).is_open(cx) {
            self.toc.update(cx, |panel, cx| panel.hide(cx));
            cx.notify();
            return true;
        }
        if !self.chrome.dismiss() {
            return false;
        }
        cx.notify();
        true
    }

    /// 音量键要翻的那一页。`delta` 说的是**读书顺序里的那一侧**，不是屏幕的左右：
    /// 页号本来就按阅读序排（左右分区那条换算在 `chrome::zone` 里），所以 +1 在 RTL
    /// 的书里也还是「往后一页」。
    ///
    /// 走的正是点屏幕左右两边那条路（`on_body_tap` 里 `Effect::NextPage` 那一支），
    /// 所以连续滚动与分页两种模式的行为、以及章末自动跨章，全都跟着正文一致。
    /// 目录 / 设置 / 「⋯」那一层浮着的时候也照样翻正文 —— 那几屏自己有滚动条，
    /// 让音量键去翻它们要另写一套消费逻辑，这一版不做。
    pub fn turn_page(&mut self, delta: isize, cx: &mut Context<Self>) {
        // 查看器开着时翻页：那一层还停在旧的一页上，先把界面收干净再翻。
        if self.image_viewer.read(cx).is_open() {
            self.image_viewer.update(cx, |viewer, _| viewer.close());
            cx.notify();
        }
        self.reader
            .update(cx, |reader, cx| reader.scroll_page(delta, cx));
    }

    /// 方向键翻页（PC）。返回 `false` = 这一键不归阅读页，调用方该 `cx.propagate()`。
    ///
    /// 子栈上压着别的屏（紧凑设置）时让开：那一屏的滑条与列表用同一套方向键，抢过来
    /// 就是滑条不动、背后正文在翻页。其余判据（图片查看器开着、文字输入框拿着焦点）
    /// 都在会话与 gpui 那一侧 —— 见 [`ReaderSession::arrow_turn`]。
    pub fn arrow_turn(&mut self, arrow: chrome::Arrow, cx: &mut Context<Self>) -> bool {
        if self
            .stack
            .upgrade()
            .is_some_and(|stack| stack.read(cx).depth() > 1)
        {
            return false;
        }
        self.reader
            .update(cx, |reader, cx| reader.arrow_turn(arrow, cx))
    }

    /// 当前页号。**只由宿主测试问**：方向键那一条要从键绑定一路扎到页号上，而翻页
    /// 落没落位在屏幕上只是「字换了一屏」，没有别的可断言的数。
    #[cfg(test)]
    pub fn page_number(&self, cx: &App) -> usize {
        self.reader.read(cx).page()
    }

    /// 这一章切成了多少页（双页对开时是「多少张纸」）。同样只给宿主测试。
    #[cfg(test)]
    pub fn sheet_count(&self, cx: &App) -> usize {
        self.reader.read(cx).page_count()
    }

    /// 顶栏那颗返回。正文是这一层那条子栈的栈底，弹不动 —— 该请外壳把整层收走。
    ///
    /// 从正文压进来的那一屏（设置）自己带返回，它压着的时候这一屏根本看不见，所以这里
    /// 不必先问子栈。
    ///
    /// 收尾分两步走是因为**这一句跑在本页自己的更新里**（顶栏那一下点击）：
    /// `page.update(|this| this.pop())` 已经在借着自己了，再请外壳 `update` 一次本页
    /// （`close_reading` 干的正是那件事）就是二次可变借用，直接 panic —— release 下
    /// `panic = abort`，读者看到的「点返回直奔桌面」就是它。所以本页该做的事自己做，
    /// 外壳那边只翻退出那一格（[`AppShell::dismiss_reading`]）。
    pub(crate) fn pop(&mut self, cx: &mut Context<Self>) {
        self.close(cx);
        if let Some(shell) = self.shell.upgrade() {
            shell.update(cx, |shell, cx| shell.dismiss_reading(cx));
        }
    }

    /// 正文上的一击：先让选中说话（拖选松手的豁免、选中的清除/菜单回开），
    /// 再按分区照结果动。见 [`chrome::Chrome::handle_body_tap`] 与
    /// `selection.rs` 的 `tap_with_selection`。
    pub(in crate::ui::features::reader) fn on_body_tap(
        &mut self,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let position = event.position();
        // 三条判据的归因与顺序都在 `ReaderSession::tap_on_body` 里（那里才测得到）。
        let consumed = self
            .reader
            .update(cx, |reader, cx| reader.tap_on_body(window, cx, position));
        if consumed {
            cx.notify();
            return;
        }
        // 脚注优先于链接也优先于分区：脚注标记本身就是一条 `<a href="#...">`，但读者
        // 要的是「就地看注文」，不是「跳到注文那一行」。排在选中之后（拖选松手那一下的
        // 豁免仍在 `tap_on_body` 里判过）。
        if self.reader.update(cx, |reader, cx| {
            reader.open_footnote_at(window, cx, position)
        }) {
            return;
        }
        // 链接优先于分区：点中 `<a href>` 就跳过去，不翻页、不开浮层。排在选中之后、
        // 分区之前 —— 拖选松手那一下（`tap_on_body` 已豁免）不该被当成点链接。
        if self
            .reader
            .update(cx, |reader, cx| reader.open_link_at(window, cx, position))
        {
            return;
        }
        // 三区分的是**正文那一栏**，不是整块窗口。宽档的目录改成浮层之后正文不再让宽，
        // 所以这里不需要再减一条栏：整块窗口就是正文的可用宽。
        let width = f32::from(window.viewport_size().width).max(1.0);
        // 左右两档意味着什么，跟着整本的翻页方向走（`zone` 里那一条）。
        let rtl = self.reader.read(cx).progression_rtl();
        let tap = zone(f32::from(position.x), width, rtl);
        match self.chrome.handle_body_tap(tap) {
            Effect::OpenChrome | Effect::DismissChrome => cx.notify(),
            Effect::PrevPage => {
                self.reader
                    .update(cx, |reader, cx| reader.scroll_page(-1, cx));
            }
            Effect::NextPage => {
                self.reader
                    .update(cx, |reader, cx| reader.scroll_page(1, cx));
            }
        }
    }

    /// 把「别处也改得动」的数推回滑条：正文滚了要挪进度滑块，设置页改了字号要挪字号滑块，
    /// 而亮度那一格在「跟随系统」时没有数 —— 滑条上把它显示成顶格，配置里它是 `None`。
    ///
    /// 带阈值比较：拖动的时候滑条自己就是权威，每帧再按正文读回来的数写回去，
    /// 就是拿手指算出来的值和滚动算出来的值互相追。
    pub(in crate::ui::features::reader) fn sync_sliders(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let progress = self.reader.read(cx).progress();
        let prefs = ConfigHolder::reader(cx);
        let brightness = ConfigHolder::appearance(cx)
            .brightness
            .unwrap_or(BRIGHTNESS_RANGE.1);
        let wanted = [
            (&self.progress, progress),
            (&self.font_size, prefs.font_size),
            (&self.brightness, brightness),
        ];
        for (state, value) in wanted {
            if (state.read(cx).value().end() - value).abs() > 0.005 {
                state.update(cx, |state, cx| state.set_value(value, window, cx));
            }
        }
    }

    /// 底栏那排入口里的面板档位切换（现在只剩「阅读设置」一块，目录已经换成弹层）。
    /// 整层保持露出 —— 切面板不该让顶栏闪一下。
    pub(in crate::ui::features::reader) fn pick(&mut self, panel: Panel, cx: &mut Context<Self>) {
        self.chrome.select(panel);
        cx.notify();
    }

    /// 目录弹层：开 or 关。树跟着阅读位置重建、搜索框与「⋯」一并收掉、书头跟着重查，
    /// 那些都在 [`TocPanel::toggle`] 里；这一屏只多管一件事 —— 设置那块面板同时收掉，
    /// 弹层升起来之后卡片还压在它下面，等于两层东西说同一件事。
    pub(in crate::ui::features::reader) fn toggle_toc(&mut self, cx: &mut Context<Self>) {
        if !self.toc.read(cx).is_open(cx) {
            // 设置那块面板同时收掉 —— 弹层升起来之后卡片还压在它下面，等于两层东西说同一件事。
            self.chrome.select(Panel::None);
        }
        self.toc.update(cx, |panel, cx| panel.toggle(cx));
        cx.notify();
    }

    /// 连续 ⇄ 双页。换档不动位置：进度是「第几个字」，两档之间搬的是同一句话。
    pub(in crate::ui::features::reader) fn toggle_spread(&mut self, cx: &mut Context<Self>) {
        let wanted = self.reader.read(cx).wanted_mode();
        let next = wanted.next();
        self.reader
            .update(cx, |reader, cx| reader.set_mode(next, cx));
    }

    /// 浅深一键。
    ///
    /// 判据是**解析出来的**那一档（`cx.theme().mode`），不是配置里写的 `system`：
    /// 跟着系统走的读者点这一下要落到系统此刻的反面，而不是在 `system` 上原地打转。
    pub(in crate::ui::features::reader) fn toggle_night(&mut self, cx: &mut Context<Self>) {
        let dark = cx.theme().mode == ThemeMode::Dark;
        ConfigHolder::edit_appearance(cx, |appearance| {
            appearance.mode = if dark { Mode::Light } else { Mode::Dark }
        });
    }

    /// 「更多设置」= 往正文这条子栈里再压一屏钻入式设置。
    ///
    /// 入口不在底栏那颗「设置」上了（那一颗现在就地展开亮度 / 字号 / 翻页方式那块面板），
    /// 而是这一屏右下角那一格，加上目录那屏的 ⋯ —— 完整那一屏（行距、段距、版心、字体家族）
    /// 得有个看得见的位置，不该只剩一条藏起来的键。
    ///
    /// 手机上没有独立设置窗口（两栏的整页字段塞不进这块小屏），压进栈的这屏吃的是
    /// 与桌面同一份页面规格。压进栈而不是切目的地：系统返回 pop 它就回到刚才那
    /// 一行字，阅读位置一点没动。桌面同样走这条 —— 少一个只在一端存在的窗口，
    /// 就少一处「这功能那端没有」。
    ///
    /// 压的是**这一层自己的**那条栈而不是架面那条：这一层盖在整窗上，架面的栈在它下面，
    /// 压那边的话这一屏会被自己盖住。
    pub(in crate::ui::features::reader) fn open_settings(&mut self, cx: &mut Context<Self>) {
        let settings = cx.new(|_| CompactSettings::new());
        let _ = self.stack.update(cx, |state, cx| {
            state.push(settings, NavMotion::Animated, cx)
        });
    }

    pub(in crate::ui::features::reader) fn advance(
        &mut self,
        delta: isize,
        cx: &mut Context<Self>,
    ) {
        self.reader
            .update(cx, |reader, cx| reader.advance_chapter(delta, cx));
    }

    /// 脚注弹层：从读者点的那一处长出来的一张小纸片，用 gpui-kit 的 [`Popover`]。
    ///
    /// 正文是**画出来的**字与图，不是一棵按链接分好的元素树 —— 没有现成的「标记元素」
    /// 可以当触发器。所以这里放一个**零尺寸的触发器**在锚点上，`Popover` 拿它的 bounds
    /// 定位；开关由会话的状态控制（`open(true)`），点别处 / 按返回把状态清掉，这一格
    /// 下一帧自然消失。
    ///
    /// 宽度给上限而不是铺满：一条注文常常只有一句，铺满整屏读起来像一屏正文。
    fn footnote_popover(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let note = self.reader.read(cx).footnote()?;
        let text = note.text.clone();
        let anchor = note.anchor;
        // 小纸片按**最宽那一条**夹在屏内：靠右缘的标记不能再让卡片从那儿起笔，靠近左缘的
        // 也不能。夹的是最宽值，所以内容窄于它时卡片会偏左一点 —— 偏一点看得见，飞出屏外
        // 看不见，两害相权取其轻。
        let card = FOOTNOTE_CARD_W + FOOTNOTE_PAD * 2.;
        let edge = FOOTNOTE_EDGE;
        let room = window.bounds().size.width - card - edge;
        let left = (anchor.x - card * 0.5).clamp(edge, room.max(edge));
        let page = cx.entity().clone();
        Some(
            div()
                .absolute()
                .left(left)
                .top(anchor.y)
                .child(
                    Popover::new("reader-footnote")
                        .anchor(Anchor::TopLeft)
                        .offset(FOOTNOTE_GAP)
                        .open(true)
                        .on_open_change(move |open, _window, cx| {
                            if !*open {
                                page.update(cx, |this, cx| {
                                    this.reader
                                        .update(cx, |reader, cx| reader.dismiss_footnote(cx))
                                });
                            }
                        })
                        .trigger(
                            Button::new("reader-footnote-trigger")
                                .ghost()
                                .p_0()
                                .w(px(0.))
                                .h(px(0.))
                                .min_w(px(0.))
                                .min_h(px(0.)),
                        )
                        .content(move |_, _, cx| {
                            div()
                                .max_w(FOOTNOTE_CARD_W)
                                .text_sm()
                                .text_color(cx.theme().popover_foreground)
                                .child(text.clone())
                        }),
                )
                .into_any_element(),
        )
    }

    /// 翻页方式写回 `appearance` 那一节：它不是排版失效键，换它不该让任何一章重排
    /// （见 `folio_core::infra::config::PageTurn` 为什么这一格不在 `Reader` 里）。
    pub(in crate::ui::features::reader) fn set_page_turn(
        &mut self,
        turn: PageTurn,
        cx: &mut Context<Self>,
    ) {
        ConfigHolder::edit_appearance(cx, |appearance| appearance.page_turn = turn);
        cx.notify();
    }

    pub(in crate::ui::features::reader) fn slider_row(
        &self,
        label: String,
        value: String,
        state: &Entity<SliderState>,
        cx: &App,
    ) -> AnyElement {
        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(label),
                    )
                    .child(div().text_sm().child(value)),
            )
            .child(Slider::new(state).w_full())
            .into_any_element()
    }
}

impl Render for ReadingPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 正文让出干净态那两条带。浮层不参与：它压字，不推字。
        // 「身上压着东西」也报给会话：那一刻正文的手势让位 —— 那一击的语义是收浮层，
        // 而按住一张图不该同时在底下选中字。报的是**有没有**，不是哪一层。
        //
        // 目录两种表面（手机的底部弹层、平板与桌面的左侧抽屉）都**压在正文之上**：
        // 抽屉不再占宽，正文因此一寸不让，点字照常选得中 —— 遮罩会先把那一击收掉。
        let size = SizeClass::of(window);
        let wide = !size.is_compact();
        let toc_open = self.toc.read(cx).is_open(cx);
        let layers_up = self.chrome.is_open()
            || toc_open
            || self.toc.read(cx).menu_open()
            || self.reader.read(cx).footnote().is_some()
            || self.reader.read(cx).note_editor_open()
            || self.image_viewer.read(cx).is_open();
        self.reader.update(cx, |reader, cx| {
            reader.set_reserve(self.reserve, cx);
            reader.set_chrome_bands(chrome::HEADER_HEIGHT, chrome::FOOTER_HEIGHT, cx);
            reader.set_overlay_open(layers_up, cx);
        });
        self.sync_sliders(window, cx);
        let tap = cx.listener(
            |this: &mut Self, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>| {
                this.on_body_tap(event, window, cx)
            },
        );
        // 目录那一层此刻开着。两种表面的遮罩都盖整块窗口，白底顶栏压在半透明黑下面
        // 会成一片灰 —— 而那条上的返回键正是读者开完目录要按的第一颗：又压暗又点不动是
        // 最坏的组合。所以这一档把顶栏抬到遮罩之上，让它保持原色也保持能按。
        //
        // 底栏**不**跟着抬：那几颗（字号、进度、翻页模式）在目录里没有对应物，压在遮罩下
        // 才是「这一层是临时抬起来的」的正确读法，而当初那句「让那 172 像素反而让人以为
        // 还能点」的担心对顶栏不成立 —— 抬上来之后它是真能点。
        let toc_up = toc_open && self.chrome.is_open();
        let column = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .right_0()
            .child(
                // 正文的点击落在这层而不是 `ReaderSession` 自己身上。它的 hitbox 是整屏的，
                // 而 gpui 的一击会发给**每一个**「自己的 hitbox 含有按下点」的 handler
                // （`Window::dispatch_mouse_event` 倒序遍历帧内的监听列表，不是沿祖先链冒泡），
                // 所以顶栏、底栏与目录那一层各自声明挡住身后的 hitbox，
                // 否则浮层的按钮点下去会连着触发这里，把刚切过去的面板当场收掉。
                div()
                    .id("reader-body")
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .on_click(tap)
                    .child(self.reader.clone()),
            )
            // 干净态那两行与浮层是同一片地方的两种样子，永远只画一套。
            //
            // 顶栏在目录那一层开着时不在这里画 —— 抬到遮罩之上去了（见下面
            // `toc_up` 那一段）。底栏照旧压在遮罩下。
            .when(self.chrome.is_open() && !toc_up, |this| {
                this.child(self.top_bar(wide, cx))
                    .child(self.bottom_bar(cx))
            })
            .when(self.chrome.is_open() && toc_up, |this| {
                this.child(self.bottom_bar(cx))
            })
            .when(!self.chrome.is_open(), |this| {
                this.child(bands::header(&self.reader, cx))
                    .child(bands::footer(&self.reader, self.bottom_inset, cx))
            });
        let mut root = div()
            .id("reader-page")
            .size_full()
            .relative()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(column);
        // 目录那一屏最后挂：`anchored` 层按登记的先后决定谁在上面，它盖的是整块窗口，
        // 顶栏底栏因此压在遮罩下面（让那 172 像素反而会让读者以为底栏还能点，而那一击的
        // 真实归属是遮罩）。它每帧都构造，收着时也构造 —— 弹簧的采样在里面（见
        // `TocPanel::render` 与 `widgets::bottom_sheet` 那段模块注释）。右上角「⋯」那条
        // 菜单是 gpui-kit 自己的 `PopupMenu`，由那颗按钮按下时挂进它自己的浮层。
        root = root.child(self.toc.clone());
        // 抬上来的顶栏：还是原来那一颗条，只是挂在目录之后。包一层绝对定位容器，
        // 几何就不变（正文现在整窗铺开，没有栏要让）。
        if toc_up {
            root = root.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .child(self.top_bar(wide, cx)),
            );
        }
        // 脚注弹层压在目录之上、图片查看器之下：它是「从正文里抬起来的一张小纸」，
        // 盖住目录那种整屏弹层是对的，而放大看图时读者要的是图。
        root = root.children(self.footnote_popover(window, cx));
        // 查看器最后挂：它盖住的正是顶栏、底栏与弹层，所以谁都不该浮在它上面。
        // 没开着时这一格是空的，一个节点都不多。
        //
        // 像素每帧由这一层向图集要（并预取左右那两张），落点与「定位原文」都归会话，
        // 所以这三样都递给它：图集那格解完了要喊**这一屏**重画（`view` 传的是本页的
        // 实体号，不是会话的 —— 喊会话还会绕一圈回到这里，多一趟元素树）。
        let atlas = self.reader.read(cx).atlas.clone();
        root = root.children(image_viewer::overlay(
            &self.image_viewer,
            &atlas,
            &self.reader,
            cx.entity().entity_id(),
            self.top_inset,
            &self.image_viewer_focus,
            window,
            cx,
        ));
        // 「界面亮度」在这一端的**另一手**：桌面上背光不归窗口管（那是显示器的地盘），
        // 于是把读者要的那一成压成一层黑，透明度从同一个数算出来（[`chrome::dim_alpha`]）。
        // Android 上这一格恒为 0 —— 那边的暗走的是真背光（`platform::set_screen_brightness`），
        // 判据只写在 [`platform::BRIGHTNESS_IS_BACKLIGHT`] 那一处：两处各压一次就是压两次。
        let dim = if platform::BRIGHTNESS_IS_BACKLIGHT {
            0.0
        } else {
            chrome::dim_alpha(ConfigHolder::appearance(cx).brightness)
        };
        // 最后挂，连图片查看器那一层也一起暗：这一格说的是「这块屏」，不是「正文那一栏」。
        // 色取主题的 `overlay`（那一格本来就是给屏幕压暗用的颜色），只换它的透明度 ——
        // 应用层不嵌裸色值。
        let scrim = (dim > 0.0).then(|| {
            let overlay = cx.theme().overlay;
            div()
                .absolute()
                .inset_0()
                .bg(Hsla { a: dim, ..overlay })
                .into_any_element()
        });
        root.children(scrim)
    }
}
