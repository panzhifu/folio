//! 更新日志那一屏：一块从底边升起来的卡片，正文按 Markdown 排。
//!
//! ## 为什么是弹层，不是居中那张 Dialog
//!
//! 从前这一屏走 `window.open_dialog`，正文区写死 `max_h_96`（384 逻辑像素）。`CHANGELOG.md`
//! 里 0.1.1 那一节是四十来行中文，384 像素要滚七八趟；而那一层没有退场动画，也没有「往下
//! 一划就收」这一步，与本应用其余那几张弹层（目录、阅读设置、架面那三块）各说各的话。换成
//! [`BottomSheet`] 之后，收这一层的三条路（下拉过头、点遮罩、系统返回键）与别处同一条，
//! 正文拿到视口高的 85%。
//!
//! ## 为什么正文交给框架的富文本元素
//!
//! 手写那一版只认 `#`、`- `、普通段落三样，认不出的一律原样成一行字 —— 于是屏上露出
//! `**听书（合成朗读）。**` 这两串星号（`CHANGELOG.md` 里每条重点都是这个形状），而缩进两格
//! 的子条目（`  - **Android**：…`）因为行首不是 `- ` 整条退化成裸文本。`TextView::markdown`
//! 认标题、粗体、斜体、有序与无序的嵌套、代码、表格，并且**不新增依赖**：`markdown` 那颗包
//! 本来就在锁里（folio-core 读 `.md` 的书用同一颗）。
//!
//! ## 宿主：每屏一份
//!
//! 与书架那三块同一形状 —— **功能自己持有实体、在自己的 `render` 里挂**。所以设置那两屏
//! （桌面 `SettingsView` 与手机 `CompactSettings`）各建一份本实体、各挂各的。不共用全进程
//! 那一份：两屏可以同时活着（「我的」那一个目的地的根，加上从阅读页压进来的那一屏），共用
//! 一份弹层状态时，读者关掉的是头顶那一屏，沉下去的却是底下那一屏的内容。

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{Sizable as _, h_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement, Render, SharedString, Styled, TestSupportExt as _, Window, div,
    rems,
};

use crate::ui::components::{BottomSheet, BottomSheetState, muted_label};
use crate::ui::platform;
use crate::ui::services::l10n::tr;
use crate::ui::services::update::Notes;

/// 卡片展开那一档占视口高的比例。与目录同一档：更新日志是一篇要读完的正文，而不是
/// 「调一下看一眼身后那几行重排」的控件屏，底下不必给设置那一屏留着能点的行 —— 这一层
/// 收掉就是收掉（点遮罩、下拉、返回键三条路都在）。
const SHEET_EXPANDED: f32 = 0.85;
/// 往下拖还能停的那一档：半屏。留着它是为了让「先读一段、再往下滚设置那一屏」这个动作
/// 有一档可以停，与目录那两档同一条理由。
const SHEET_PEEK: f32 = 0.5;
/// 正文那一栏的最大行宽（42 rem = 默认缩放下 672 逻辑像素）。
///
/// 弹层那块表面是满幅的：桌面上不封顶时一行中文能排到八九十个字，读完一句要回到行首找
/// 下一句。手机上这一档不绑，屏幕本来就比它窄。写 `rems` 而不是 `px`：整屏的刻度要跟着
/// 界面缩放一起动。
const MEASURE_REMS: f32 = 42.0;

/// 更新日志那张卡。开合握在自带的 [`BottomSheetState`] 里，正文握在 [`Self::payload`] 里。
pub(in crate::ui::features::settings) struct NotesSheet {
    /// 句柄必须跨帧持有：弹层每帧都要构造（弹簧的采样在里面），换一份就断了退场动画。
    sheet: Entity<BottomSheetState>,
    /// 这一屏要摆的东西：**正文**，或者**一句为什么没有**。只在卡片开着的时候有意义。
    payload: Option<Notes>,
}

impl NotesSheet {
    pub(in crate::ui::features::settings) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            sheet: cx.new(|cx| BottomSheetState::closed(cx)),
            payload: None,
        }
    }

    /// 抬起来，正文就是交来的这一份。
    ///
    /// 调用点是**拿到正文之后**（`services::update::with_notes` 那三条分支），不是先开一张空
    /// 卡片赌它会不会自己重画 —— 这一层没有「空着等内容」那一档，空着升起来就是给读者看一片白。
    pub(in crate::ui::features::settings) fn open(&mut self, notes: Notes, cx: &mut Context<Self>) {
        self.payload = Some(notes);
        self.sheet.update(cx, |sheet, cx| sheet.show(cx));
        cx.notify();
    }

    /// 这一层开着吗。宿主的返回键要先问这一句：那一层自己收，别让同一按把底下的子页也弹掉。
    pub(in crate::ui::features::settings) fn is_open(&self, cx: &App) -> bool {
        self.sheet.read(cx).is_open()
    }

    /// 宿主主动收这一层（返回键走到这一档的时候）。不是读者自己关的，所以不发
    /// [`Dismissed`](crate::ui::components::BottomSheetEvent::Dismissed)。
    pub(in crate::ui::features::settings) fn hide(&mut self, cx: &mut Context<Self>) {
        self.sheet.update(cx, |sheet, cx| sheet.hide(cx));
    }

    /// 钉在把手下面那一行：这一版是哪个版本，加上离线时唯一还能读到正文的那条路。
    ///
    /// 版本号是**跑着的这一份**，不是查到的最新一版：这一屏的正文按 `v{当前版本}` 那条 tag 取，
    /// 装了 0.1.1 而最新是 0.2.0 时讲的仍是 0.1.1。
    fn header(&self, cx: &App) -> AnyElement {
        let version = env!("CARGO_PKG_VERSION");
        let title: SharedString = format!("{} · v{}", tr(cx, "update.notes"), version).into();
        measure(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .pt_2()
                .pb_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(
                    Button::new("notes-open-releases")
                        .label(tr(cx, "update.open_page").to_string())
                        .ghost()
                        .small()
                        .on_click(|_, _, _| {
                            let url = folio_core::net::update::RELEASES_PAGE;
                            if !platform::open_url(url) {
                                tracing::warn!("打不开发布页，读者只能自己复制：{url}");
                            }
                        }),
                ),
        )
    }

    /// 正文三档：Markdown 排出来、这一版没留下正文、或者取不到（念的是**为什么**取不到）。
    fn body(&self, cx: &App) -> AnyElement {
        let Some(payload) = self.payload.as_ref() else {
            // 卡片只由 [`NotesSheet::open`] 抬起来，正常走不到这一档；留给退场那半程里
            // 实体被换掉的形状 —— 画空，不编一句话填在这儿。
            return measure(div());
        };
        let shown = match payload {
            // 取不到时念的是**为什么**取不到（连不上 / 被限流 / 没发过版），不是一句
            // 「没有留下更新日志」：那会把读者的网络问题说成作者没写，而他真会去翻发布页。
            Notes::Failed(why) => return measure(muted_label(why.to_string(), cx)),
            Notes::Ready(body) if body.trim().is_empty() => {
                return measure(muted_label(tr(cx, "update.notes_missing").to_string(), cx));
            }
            Notes::Ready(body) => body.trim().to_owned(),
        };
        // `pb_6`：滚到最末一行时别让文字贴着卡片下沿，与目录那屏底部那 24 同一档。
        // 那一格挂个名并 `test_support`：宿主测试就靠它判「卡片升起来的时候内容真给了」，
        // 非 `test-support` 构建里这一层是透明的，真机不多花一个节点。
        measure(
            div()
                .id("update-notes-body")
                .test_support()
                .w_full()
                .child(TextView::markdown("update-notes-markdown", shown).pb_6()),
        )
    }
}

impl Render for NotesSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sheet = BottomSheet::new(&self.sheet, "update-notes")
            .expanded(SHEET_EXPANDED)
            .peek(SHEET_PEEK);
        // 收稳了那一程照样要交元素（弹簧的采样在里面），只是不给内容。判据是 `is_idle()`
        // 而不是 `is_open()`：退场那半程 `is_open()` 已经是 `false` 而卡片还浮在屏幕上，
        // 那时断掉内容就等于看着它空着沉下去。
        if self.sheet.read(cx).is_idle() {
            return sheet.into_any_element();
        }
        sheet
            .header(self.header(cx))
            .child(self.body(cx))
            .into_any_element()
    }
}

/// 把一栏内容收进 [`MEASURE_REMS`] 那一档行宽，并在这块满幅的卡里水平居中。
///
/// 横向留白写在栏内（`px_4`）：手机上卡片满幅，文字不该贴住屏幕边；桌面上这一栏已经
/// 被封顶并居中，那 16 只是居中栏自己的内缩。标题行与正文用同一个函数，两者的左边界
/// 才对得上。
fn measure(body: impl IntoElement) -> AnyElement {
    div()
        .w_full()
        .flex()
        .justify_center()
        .child(div().w_full().max_w(rems(MEASURE_REMS)).px_4().child(body))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    //! 这一屏能被宿主测到的是**「卡片开得出、开出来时正文真交进去排了」**。
    //! 排出来的样子（标题多大、粗体认不认得、嵌套子条目缩进多少）只有真机量得出来，
    //! 这里钉的是接线：换容器时最容易漏的正是「元素每帧都要构造，内容只在没停稳时给」。

    use std::time::Duration;

    use folio_core::Config;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, px, size};

    use super::{Notes, NotesSheet};
    use crate::ui::services::config::ConfigHolder;

    /// 一帧的模拟时长：40 帧 @16ms 足够一根 280ms 的弹簧走完进出。
    const FRAME: Duration = Duration::from_millis(16);

    /// 一扇把**这一张卡**摆在根上的窗：底部弹层要真窗口才量得到视口高，而这一屏的渲染路径
    /// 正是这里要测的那条。
    fn boot(cx: &mut TestAppContext) -> (Entity<NotesSheet>, AnyWindowHandle) {
        cx.update(gpui_kit::init);
        let mut built = None;
        let handle = cx.open_window(size(px(900.), px(600.)), |window, cx| {
            ConfigHolder::install(Config::default(), cx);
            let sheet = cx.new(NotesSheet::new);
            built = Some(sheet.clone());
            Root::new(sheet, window, cx)
        });
        (built.expect("这一屏该建起来"), handle.into())
    }

    /// 走 `n` 帧：先把测试时钟推一格再画。弹簧按 `background_executor().now()` 计步，而
    /// `run_until_parked` 只跑事件，不替「没有定时器可等」的那个钟前进。
    fn frames(cx: &mut TestAppContext, handle: AnyWindowHandle, n: usize) {
        for _ in 0..n {
            cx.executor().advance_clock(FRAME);
            cx.run_until_parked();
            cx.update_window(handle, |_, window, cx| window.render_frame(cx))
                .expect("渲染应成功");
        }
    }

    /// 面板这一帧真在树里吗。
    fn mounted(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("bottom-sheet-surface").is_some()
        })
        .expect("渲染应成功")
    }

    /// 正文那一栏这一帧真在树里吗。
    fn body_shown(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("update-notes-body").is_some()
        })
        .expect("渲染应成功")
    }

    /// 抬起卡片并让弹簧走完 —— 与读者按下那颗按钮之后真机上发生的事同一条路。
    fn rise(
        cx: &mut TestAppContext,
        sheet: &Entity<NotesSheet>,
        handle: AnyWindowHandle,
        notes: Notes,
    ) {
        sheet.update(cx, |this, cx| this.open(notes, cx));
        frames(cx, handle, 40);
    }

    /// 没按下之前不该有这块弹层；`open` 之后升起来了，正文那一栏也必须跟着在。
    ///
    /// 钉这一条是因为换容器时最容易漏的正是「元素每帧都要构造，内容只在没停稳时给」：
    /// 写反了的症状是卡片升起来了而里面是空的。
    #[gpui_kit::test]
    fn the_card_only_gives_its_body_once_it_rises(cx: &mut TestAppContext) {
        let (sheet, handle) = boot(cx);
        frames(cx, handle, 1);
        assert!(!mounted(cx, handle), "没按下就不该有这块弹层");
        rise(
            cx,
            &sheet,
            handle,
            Notes::Ready("### 新增\n- **听书（合成朗读）。**\n  - **Android**：系统语音".into()),
        );
        assert!(mounted(cx, handle), "`open` 之后这块弹层要升起来");
        assert!(
            body_shown(cx, handle),
            "升起来时要把正文交给 Markdown 排，不能空着升起来"
        );
    }

    /// 取不到正文时卡片照样开、念的是那一句为什么；但没有正文就不该有那一栏空 Markdown。
    #[gpui_kit::test]
    fn a_failed_fetch_still_rises_and_says_why(cx: &mut TestAppContext) {
        let (sheet, handle) = boot(cx);
        rise(cx, &sheet, handle, Notes::Failed("暂时连不上网络".into()));
        assert!(
            mounted(cx, handle),
            "失败也要让读者看见那句为什么，不能什么都不摆"
        );
        assert!(
            !body_shown(cx, handle),
            "没有正文时不该摆一栏空的 Markdown 在那儿"
        );
    }

    /// 这一版没留下正文：卡片开得出，念的是「这一版没写」而不是空白。
    #[gpui_kit::test]
    fn an_empty_body_rises_with_the_missing_sentence(cx: &mut TestAppContext) {
        let (sheet, handle) = boot(cx);
        rise(cx, &sheet, handle, Notes::Ready("   ".into()));
        assert!(mounted(cx, handle), "没有正文也要开得出那一屏的说明");
        assert!(!body_shown(cx, handle), "空的正文不进 Markdown 那一栏");
    }
}
