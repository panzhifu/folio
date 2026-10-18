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
use gpui_kit::assets::IconName;
use gpui_kit::base::{NavMotion, NavStackState};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, StyledExt as _, ThemeMode, ThemeStyled as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, ClickEvent, Context, Corners, Entity, FocusHandle,
    Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, TestSupportExt as _, WeakEntity, Window, div,
    px,
};

use crate::ui::features::reader::view::bands;
use crate::ui::features::reader::model::chrome::{
    self, BRIGHTNESS_RANGE, Chrome, Effect, FONT_SIZE_RANGE, PageMode, Panel, Reserve, zone,
};
use crate::ui::features::reader::components::image_viewer::{self, ImageViewer};
use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::features::reader::components::toc_panel::{TocEvent, TocPanel};
use crate::ui::features::reader::motion::turn_animation;
use crate::ui::features::settings::CompactSettings;
use crate::ui::platform;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::tr;
use crate::ui::shell::AppShell;
use crate::ui::shell::navigation::SizeClass;
use crate::ui::components::TopBar;

/// 脚注小纸片的最宽那一条：内容的 `max_w` 与夹边算的是同一个数。
const FOOTNOTE_CARD_W: Pixels = px(300.);
/// 弹层自己那层内边距的**上估**。算少了卡片会压到屏外，算多了只是偏进来一点 —— 宁可偏进来。
const FOOTNOTE_PAD: Pixels = px(12.);
/// 卡片离屏边最少留这么多。
const FOOTNOTE_EDGE: Pixels = px(8.);
/// 标记下缘与卡片之间的缝。
const FOOTNOTE_GAP: Pixels = px(10.);

pub struct ReadingPage {
    reader: Entity<ReaderSession>,
    /// 外壳报来的「不属于正文的那一段」。顶层盖满整窗，所以那一段只剩顶上那条系统状态栏 /
    /// 挖孔（底边见 [`ReadingPage::bottom_inset`]）—— `shell::reading_reserve`。
    reserve: Reserve,
    /// 顶上那条系统状态栏 / 挖孔占掉多高（逻辑像素）。见 [`shell::reading_top_inset`]。
    ///
    /// 整层已经由外壳按这一个数往下推过了，这里只给顶栏用：那条的表面要**再往上长回屏幕顶边**，
    /// 不然它投影的上半截会露在条子上方，被条子自己的边切成一条横线（见 [`TopBar::overlay`]）。
    ///
    /// [`shell::reading_top_inset`]: crate::ui::shell::reading_top_inset
    top_inset: f32,
    /// 底下那条系统手势条占掉多高（逻辑像素）。
    ///
    /// 它**不拿去推整层**，只给底栏卡片与干净态页脚用：那两处把内容抬高 `bottom_inset`、
    /// 底色铺到屏幕底，所以卡片与屏幕底之间不是一条空缝（与读者那句「位于系统底部栏之上、
    /// 颜色保持一致」同一条口径）。正文那边已经由 `reserve.height` 扣过它，不再重复扣。
    bottom_inset: f32,
    /// 这一层自己的返回栈（弱句柄）。**正文是栈底**，从阅读页压进去的那一屏（设置）叠在
    /// 它上面 —— 借架面那条栈的话，压进来的那一屏会被这一层盖住。
    ///
    /// 弱是因为栈里存着正文自己的 `AnyView`：两边都攥紧就是互相拽着，这一层放手之后
    /// 两边都散不掉。
    stack: WeakEntity<NavStackState>,
    /// 拿着这一层的外壳（同样是弱句柄：外壳攥着这一层，这一层不该反过来攥着外壳）。
    /// 顶栏那颗返回要请它收起这一层 —— 收层是外壳的事，正文这一屏不知道自己盖在谁上面。
    shell: WeakEntity<AppShell>,
    /// 浮层的开合与面板档位。见 [`chrome`]。
    chrome: Chrome,
    /// 进度滑条。`Slider` 是有状态组件，句柄必须跨帧持有 —— 每帧新建会把拖动打断。
    progress: Entity<SliderState>,
    font_size: Entity<SliderState>,
    /// 亮度滑条。它写的是 `appearance.brightness` 那一格（不在 `Reader` 里：压暗不该让任何
    /// 一章重排）；把这一格递给平台的是外壳每一帧那一次问（`AppShell::reading_brightness`），
    /// 这一屏只管教滑条自己。
    brightness: Entity<SliderState>,
    /// 目录那一屏。整棵树、折叠集、搜索、弹层档位、书头都装在它自己那个实体里
    /// （见 [`TocPanel`]），这一屏只留一个句柄：开合状态要报给会话（身上压着东西时
    /// 正文的手势让位），元素要挂在最后（谁在上面由入树顺序定）。
    toc: Entity<TocPanel>,
    /// 图片放大那一层的状态。它画在**最上面**（要盖住顶栏底栏与那块弹层），而写它的
    /// 是会话里那一次点击 / 那一次长按 —— 它不是视图，改动靠下面那次 `observe` 才
    /// 收得到重画，形状与两块弹层一致。
    image_viewer: Entity<ImageViewer>,
    /// 查看器那一层的焦点句柄。它拿在阅读页手里而不是查看器自己里：那一层是 `overlay`
    /// 每帧造的元素树里的 `track_focus`，句柄得属于**这一棵**元素树的那个视图，
    /// 方向键那几条事件才能沿祖先链派发到它（见 `image_viewer::overlay`）。
    image_viewer_focus: FocusHandle,
    _watches: Vec<Subscription>,
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
    fn on_body_tap(&mut self, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
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
    fn sync_sliders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// 顶栏：返回 + 书名，右边是这一章的章名。那条本身的样子（高、内缩、popover 表面、
    /// 只圆下角、拦住落在它空隙里的一击）都在 [`TopBar`] 里，与书信息页、紧凑设置那一屏
    /// 共用一份；高从 [`chrome::TOP_BAR_HEIGHT`] 递进去，那个数同时管布局与画面。
    ///
    /// 右边不摆页码 —— 底栏那条状态行已经念过一遍，而「读到哪一章了」是两栏都想一眼看到的。
    /// `wide` 决定目录开关长不长在这一条上：手机档的目录是底部弹层，入口在底栏；平板 / PC
    /// 档的目录是盖在正文上的抽屉，没有常驻的栏可以挂关闭键，开关就长在这里。
    fn top_bar(&self, wide: bool, cx: &mut Context<Self>) -> AnyElement {
        let title = self.reader.read(cx).title();
        let chapter: SharedString = self.reader.read(cx).chapter_title().into();
        let page = cx.entity().clone();
        // 宽档上目录开关长在顶栏：底栏浮层收起时读者常驻看见的是这一条，而抽屉本身
        // 也带一颗关闭 —— 开的关的两个入口都在这块屏上。
        let toggle = wide.then(|| {
            let open = self.toc.read(cx).is_open(cx);
            let toc_page = cx.entity().clone();
            let label = tr(cx, "reader.toc");
            Button::new("toc-toggle")
                .ghost()
                .small()
                .selected(open)
                .icon(IconName::ListTree)
                .tooltip(label.clone().to_string())
                .accessibility_label(label)
                .on_click(move |_, _, cx| {
                    toc_page.update(cx, |this, cx| this.toggle_toc(cx));
                })
                .into_any_element()
        });
        TopBar::new("reader-top-bar")
            .overlay(self.top_inset)
            .height(chrome::TOP_BAR_HEIGHT)
            .title(title)
            .subtitle(chapter)
            .back_tooltip(tr(cx, "shelf.back"))
            .on_back(move |_, cx| {
                page.update(cx, |this, cx| this.pop(cx));
            })
            .children(toggle)
            .into_any_element()
    }

    /// 底栏：一块卡片，从上到下是「面板（可有可无）+ 状态行 + 进度条 + 一排入口」。
    ///
    /// 面板和入口条在**同一块**表面里，不是两张卡片：两张会各有一套圆角与投影，
    /// 接缝处读起来是两个东西叠着，而它们说的是同一件事。
    fn bottom_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let radius = cx.theme().radius_lg;
        let mut card = v_flex()
            .id("reader-bottom-bar")
            // 底栏与顶栏同理：正文的 hitbox 是整屏的，不拦住空隙里的一击，它会顺着
            // 帧内的监听列表接着打到正文上，把刚切过去的面板当场收掉。滚动留着，
            // 读者从底栏那一片起手也要能滚字。
            .block_mouse_except_scroll()
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .px_4()
            .pt_3()
            // 底下那条系统手势条那一段也归卡片：卡片一直铺到屏幕底，内容抬高
            // `bottom_inset`（多出来的那一段是卡片自己的底色，不是一条缝）。
            .pb(px(8.0 + self.bottom_inset))
            .gap_3()
            .popover_style(cx)
            .corner_radii(Corners {
                top_left: radius,
                top_right: radius,
                bottom_left: px(0.),
                bottom_right: px(0.),
            });
        card = match self.chrome.panel() {
            Panel::None => card,
            // 目录不在这块卡片里：它盖的是整块窗口，那一屏自己的实体在 [`TocPanel`]。
            // 阅读设置留在这里：两条滑条加一行下拉，抬着正文看一眼再放回去正合适。
            Panel::Reading => card.child(self.reading_panel(cx)),
        };
        card.child(self.status_row(cx))
            .child(self.progress_row(cx))
            .child(self.entry_row(cx))
            // 宿主门：阅读层整层 occlude 之后，正文自己的点击必须还能把它叫出来。
            .test_support()
            .into_any_element()
    }

    /// 状态行：章内位置与进度各占一端的边。分页 / 双页时左边那半念「第几页 / 共几页」，
    /// 连续滚动念「第几节 / 共几节」—— 读者认的是自己那一档的刻度。
    fn status_row(&self, cx: &mut Context<Self>) -> AnyElement {
        // 与干净态那条页脚共用 `bands::readouts`：两处念的必须是同一份数。
        let bands::Readouts {
            position, percent, ..
        } = bands::readouts(&self.reader, cx);
        let progress_label = format!("{} {}", tr(cx, "reader.progress"), percent);
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(position),
            )
            .child(div().text_sm().child(progress_label))
            .into_any_element()
    }

    /// 进度条两端是上一章 / 下一章：手指在条上既能「跳到这一章的哪儿」，
    /// 也能顺手跨到隔壁一章。
    fn progress_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let reader = self.reader.read(cx);
        // 箭头仍然只指「往哪边走」，翻书方向跟着整本走：从右往左读的书里，左边那颗
        // 按下去是**下一章**（图标不用换 —— 它指左，而左就是阅读的前方），要换的是
        // 动作、标签和可不可用三样。
        let rtl = reader.progression_rtl();
        let (left_delta, right_delta) = if rtl {
            (1isize, -1isize)
        } else {
            (-1isize, 1isize)
        };
        let can_left = reader.can_advance(left_delta);
        let can_right = reader.can_advance(right_delta);
        // 两个闭包各自要一份句柄（`Entity` 不 `Copy`），所以左边右边各克隆一次。
        let page_left = cx.entity().clone();
        let page_right = cx.entity().clone();
        let prev_label = tr(cx, "reader.prev_chapter");
        let next_label = tr(cx, "reader.next_chapter");
        let (left_label, right_label) = if left_delta > 0 {
            (next_label.clone(), prev_label.clone())
        } else {
            (prev_label.clone(), next_label.clone())
        };
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .child(
                Button::new("prev-chapter")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronLeft)
                    .tooltip(left_label.to_string())
                    .accessibility_label(left_label)
                    .disabled(!can_left)
                    .on_click(move |_, _, cx| {
                        page_left.update(cx, |this, cx| this.advance(left_delta, cx))
                    }),
            )
            .child(Slider::new(&self.progress).flex_1().min_w_0())
            .child(
                Button::new("next-chapter")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronRight)
                    .tooltip(right_label.to_string())
                    .accessibility_label(right_label)
                    .disabled(!can_right)
                    .on_click(move |_, _, cx| {
                        page_right.update(cx, |this, cx| this.advance(right_delta, cx))
                    }),
            )
            .into_any_element()
    }

    /// 一排入口。目录是一块弹层（选中态读弹层自己的状态），设置是底栏卡片里的面板，夜间
    /// 是当场生效的动作，双页是当场换摆法 —— 四件事不同，所以只有带档位的那三格吃 `selected`。
    ///
    /// 「排版」那一格并进了设置面板（亮度 / 字号 / 翻页方式都在那块里），入口因此从五格减到
    /// 四格；完整那一屏（行距、段距、版心、字体）从面板右下角那颗进去，也与目录那屏的 ⋯
    /// 同一条路。
    fn entry_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let panel = self.chrome.panel();
        let toc_open = self.toc.read(cx).is_open(cx);
        let dark = cx.theme().mode == ThemeMode::Dark;
        let spread = self.reader.read(cx).wanted_mode() != PageMode::Continuous;
        let page_toc = cx.entity().clone();
        let page_night = cx.entity().clone();
        let page_spread = cx.entity().clone();
        let page_settings = cx.entity().clone();
        let night_label = tr(cx, if dark { "reader.day" } else { "reader.night" });
        let spread_label = tr(cx, "reader.mode_spread");
        h_flex()
            .w_full()
            .items_center()
            .child(
                Button::new("entry-toc")
                    .ghost()
                    .small()
                    .flex_1()
                    .justify_center()
                    .selected(toc_open)
                    .icon(IconName::ListTree)
                    .label(tr(cx, "reader.toc").to_string())
                    .on_click(move |_, _, cx| page_toc.update(cx, |this, cx| this.toggle_toc(cx))),
            )
            .child(
                Button::new("entry-settings")
                    .ghost()
                    .small()
                    .flex_1()
                    .justify_center()
                    .selected(panel == Panel::Reading)
                    .icon(IconName::Settings)
                    .label(tr(cx, "account.settings").to_string())
                    .on_click(move |_, _, cx| {
                        page_settings.update(cx, |this, cx| this.pick(Panel::Reading, cx))
                    }),
            )
            .child(
                // 这一格只给图标：一排几格入口再各带两个字，手机上每格会被挤到点不准。
                // 双页按宽度退档，所以这格在窄屏上也仍然按得下去（按下去就是分页）。
                Button::new("entry-spread")
                    .ghost()
                    .small()
                    .flex_1()
                    .justify_center()
                    .selected(spread)
                    .icon(IconName::Columns3)
                    .tooltip(spread_label.clone().to_string())
                    .accessibility_label(spread_label)
                    .on_click(move |_, _, cx| {
                        page_spread.update(cx, |this, cx| this.toggle_spread(cx))
                    }),
            )
            .child(
                Button::new("entry-night")
                    .ghost()
                    .small()
                    .flex_1()
                    .justify_center()
                    .icon(if dark {
                        IconName::SunMedium
                    } else {
                        IconName::Moon
                    })
                    .label(night_label.to_string())
                    .on_click(move |_, _, cx| {
                        page_night.update(cx, |this, cx| this.toggle_night(cx))
                    }),
            )
            .into_any_element()
    }

    /// 底栏那排入口里的面板档位切换（现在只剩「阅读设置」一块，目录已经换成弹层）。
    /// 整层保持露出 —— 切面板不该让顶栏闪一下。
    fn pick(&mut self, panel: Panel, cx: &mut Context<Self>) {
        self.chrome.select(panel);
        cx.notify();
    }

    /// 目录弹层：开 or 关。树跟着阅读位置重建、搜索框与「⋯」一并收掉、书头跟着重查，
    /// 那些都在 [`TocPanel::toggle`] 里；这一屏只多管一件事 —— 设置那块面板同时收掉，
    /// 弹层升起来之后卡片还压在它下面，等于两层东西说同一件事。
    fn toggle_toc(&mut self, cx: &mut Context<Self>) {
        if !self.toc.read(cx).is_open(cx) {
            // 设置那块面板同时收掉 —— 弹层升起来之后卡片还压在它下面，等于两层东西说同一件事。
            self.chrome.select(Panel::None);
        }
        self.toc.update(cx, |panel, cx| panel.toggle(cx));
        cx.notify();
    }

    /// 连续 ⇄ 双页。换档不动位置：进度是「第几个字」，两档之间搬的是同一句话。
    fn toggle_spread(&mut self, cx: &mut Context<Self>) {
        let wanted = self.reader.read(cx).wanted_mode();
        let next = wanted.next();
        self.reader
            .update(cx, |reader, cx| reader.set_mode(next, cx));
    }

    /// 浅深一键。
    ///
    /// 判据是**解析出来的**那一档（`cx.theme().mode`），不是配置里写的 `system`：
    /// 跟着系统走的读者点这一下要落到系统此刻的反面，而不是在 `system` 上原地打转。
    fn toggle_night(&mut self, cx: &mut Context<Self>) {
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
    fn open_settings(&mut self, cx: &mut Context<Self>) {
        let settings = cx.new(|_| CompactSettings::new());
        let _ = self.stack.update(cx, |state, cx| {
            state.push(settings, NavMotion::Animated, cx)
        });
    }

    fn advance(&mut self, delta: isize, cx: &mut Context<Self>) {
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
    fn footnote_popover(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
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

    /// 阅读设置面板：亮度、字号两条滑条，最后一行左边是翻页方式、右边是「更多设置」。
    /// 标签在左、当前值在右、滑条在下面 —— 手机上横着排三样会让滑条只剩一半宽，
    /// 调一档要划好几下。
    ///
    /// 顺序照读者动它们的频度排：亮度一天里跟着光线走，字号换一本书动一次，翻页方式是
    /// 钉下来就不再碰的那一格。
    fn reading_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let prefs = ConfigHolder::reader(cx);
        // 「跟随系统」念成一句话而不是「100%」：那一档应用确实什么都没接管。
        let brightness_label = match ConfigHolder::appearance(cx).brightness {
            None => tr(cx, "appearance.follow_system").to_string(),
            Some(value) => format!("{}%", (value * 100.0).round()),
        };
        v_flex()
            .w_full()
            .gap_3()
            .child(self.slider_row(
                tr(cx, "reader.brightness").to_string(),
                brightness_label,
                &self.brightness,
                cx,
            ))
            .child(self.slider_row(
                tr(cx, "field.font_size").to_string(),
                format!("{:.1}", prefs.font_size),
                &self.font_size,
                cx,
            ))
            .child(self.turn_row(cx))
            .into_any_element()
    }

    /// 面板最后一行：左边那颗下拉选翻页怎么走，右边那颗把完整的设置屏压进来。
    ///
    /// 下拉**往上开**（`Anchor::BottomLeft`）：这一行长在底栏卡片里、贴在屏幕底，往下开的话
    /// 那两块菜单有一半掉到屏幕外，读者只点得到上面那一块。
    ///
    /// 选项按**当前摆法**筛过（[`turn_animation::applies`]）：连续里只有「上下滚动」与「不走
    /// 动画」讲得通，分页里「上下滚动」没有东西可滚。筛掉而不是灰着摆 —— 一排点不动的选项
    /// 在手机上读起来像坏了。
    ///
    /// 按钮上念的是 `label_of`（与列表同源、对每一档都有答案），不是「在列表里找一找」：
    /// 读者在连续里选了「上下滚动」再换成分页，那一档被筛掉了，但按钮不该因此退成一句
    /// 「翻页动画」—— 他选的东西还在配置里，只是眼下这一屏用不上。
    /// 右边那一格存在是因为底栏的「设置」不再压屏了：完整那一屏（行距、段距、版心、
    /// 字体家族）得在这一屏上有个入口。
    fn turn_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.reader.read(cx).mode();
        let turn = ConfigHolder::appearance(cx).page_turn;
        let label_of = |turn| match turn {
            PageTurn::Slide => tr(cx, "page_turn.slide"),
            PageTurn::SlideUp => tr(cx, "page_turn.slide_up"),
            PageTurn::Cover => tr(cx, "page_turn.cover"),
            PageTurn::Curl => tr(cx, "page_turn.curl"),
            PageTurn::Scroll => tr(cx, "page_turn.scroll"),
            PageTurn::None => tr(cx, "page_turn.none"),
        };
        let items: Vec<(PageTurn, SharedString)> = PageTurn::ALL
            .iter()
            .copied()
            .filter(|value| turn_animation::applies(*value, mode))
            .map(|value| (value, label_of(value)))
            .collect();
        let current = label_of(turn);
        let page_menu = cx.entity().clone();
        let page_more = cx.entity().clone();
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                Button::new("reader-page-turn")
                    .outline()
                    .small()
                    .label(current.to_string())
                    .dropdown_caret(true)
                    .accessibility_label(tr(cx, "field.page_turn").to_string())
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                        items.iter().fold(menu, |menu, (value, label)| {
                            let checked = *value == turn;
                            let value = *value;
                            // 菜单闭包是 `Fn`，每一项的回调要各自一份句柄（`Entity` 不 `Copy`）。
                            let page = page_menu.clone();
                            menu.item(PopupMenuItem::new(label.clone()).checked(checked).on_click(
                                move |_, _, cx| {
                                    page.update(cx, |this, cx| this.set_page_turn(value, cx))
                                },
                            ))
                        })
                    }),
            )
            .child(div().flex_1())
            .child(
                Button::new("reader-more-settings")
                    .ghost()
                    .small()
                    .icon(IconName::Settings)
                    .label(tr(cx, "reader.more_settings").to_string())
                    .on_click(move |_, _, cx| {
                        page_more.update(cx, |this, cx| this.open_settings(cx))
                    }),
            )
            .into_any_element()
    }

    /// 翻页方式写回 `appearance` 那一节：它不是排版失效键，换它不该让任何一章重排
    /// （见 `folio_core::infra::config::PageTurn` 为什么这一格不在 `Reader` 里）。
    fn set_page_turn(&mut self, turn: PageTurn, cx: &mut Context<Self>) {
        ConfigHolder::edit_appearance(cx, |appearance| appearance.page_turn = turn);
        cx.notify();
    }

    fn slider_row(
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
