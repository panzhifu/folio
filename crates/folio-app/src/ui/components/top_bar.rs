//! 一屏顶部那一条：返回 + 标题 +（可有可无的）副题、右侧槽。
//!
//! 收成组件是因为同一条东西原先在三个地方各写了一遍，而且已经写歪：阅读页那条是
//! 压在正文之上的浮层（popover 表面、只圆下面两个角、要拦住落在它空隙里的一击），
//! 书信息页与紧凑设置子页那两条在流里，返回那颗一颗带文字、一颗只有图标，内缩三个数。
//! 加一样东西（比如阅读页右侧那颗「目录」）就得记着改三处 —— 而漏掉的那一处只有跑到
//! 那一屏才看得见。
//!
//! 底下仍是 gpui-kit 的 [`Button`]，这里只定四件事：
//!
//! * **返回那颗永远在最左
//! * **内容行贴这一条的上沿**：带高（[`HEIGHT`]）管命中与布局，行管眼睛看到的位置 ——
//!   在带子里居中会把标题往下推半行高，手机上那半行堆在状态栏与字之间，读起来是「字压得低」
//! * **`overlay` 那一档把压在内容之上的三条规矩一起给**：表面、只圆下角、
//!   `block_mouse_except_scroll`。最后这一条不能省 —— 正文的 hitbox 是整屏的，
//!   gpui 的一击会发给**每一个** hitbox 含按下点的 handler（不是沿祖先链冒泡），
//!   不拦住的话，落在顶栏空隙里的一击会顺着打到正文，把刚开起来的浮层当场收掉。
//!   滚动仍然留着：读者从顶栏那一片起手也要能滚字。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt as _, ThemeStyled as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, BoxShadow, Corners, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, RenderOnce, SharedString, Styled, TestSupportExt as _, Window, div, hsla, px,
    rems,
};

/// 这一条的带高：44 是拇指稳定命中的下限，三处用同一个数（原来一处按内容撑、一处 44、
/// 一处吸顶那条 44 —— 同一个应用里同一条东西三种高）。
///
/// **带高管命中与布局，不管字摆在哪儿**：内容行贴这一条的**上沿**排，剩下那点空落到行下面
/// （见 [`RenderOnce`] 那句 `items_start`）。按 Material 的紧凑顶栏读法，标题是跟着状态栏
/// 下沿走的那一格，而不是在一条 44 的带子里居中 —— 居中会把整行往下推 `(带高 - 行高)/2`，
/// 手机上那半行高正好堆在状态栏与字之间，读起来就是「标题压得低」。
const HEIGHT: f32 = 44.0;
/// 内容那一行的高：返回那颗（图标档 = `size_8`，32 逻辑像素）撑出来的那一档，全应用同一条。
///
/// 定死而不是让行按最高的孩子自己撑：后者会让带 24 的小按钮那几条与带 32 的那几条落在
/// 两个不同的 `y` 上 —— 同一个应用里同一条顶栏两种标题高度，正是这个组件要收掉的东西。
const ROW: f32 = 2.0;
/// 默认左右内缩，与底栏那一排入口的 `px_3` 同一档。
const PAD_X: f32 = 12.0;
/// `overlay` 那一档的内缩：阅读页底栏用的是 `px_4`，两条要对齐。
const OVERLAY_PAD_X: f32 = 16.0;
/// 副题的宽度上限。见模块注释第三条。
const SUBTITLE_MAX: f32 = 320.0;

/// 吸顶那一档的投影：**没有** gpui-kit 平层 popover 那圈 1px 的 ring。
///
/// 平层靠 ring 当边框，可这一条是吸在屏幕顶上的：ring 的上缘会在它上方压出一条横线
/// （系统状态栏藏起来之后尤其明显）。只留往下投的一层柔影，上与下靠底色和正文分开。
///
/// 柔影自己也会在**上方**渗出一段（模糊往四周散，实测这一档往上渗约 12 逻辑像素），
/// 那一段被这一条自己的上沿切成一条硬边 —— 所以这一条的表面必须真的贴住窗口顶边，
/// 让上渗的那一段落在屏幕外（见 [`TopBar::overlay`] 的 `top_inset`）。
fn overlay_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow::new(px(0.), px(2.), hsla(0., 0., 0., 0.18)).blur_radius(px(6.))]
}

/// 「按了返回要干什么」那一格：调用方握着的是自己那个实体，这里只把窗口与应用交出去。
type OnBack = Box<dyn Fn(&mut Window, &mut App)>;

/// 一屏顶部那一条。用法照 gpui-kit 的组件：链式配好属性直接 `.child()`。
#[derive(IntoElement)]
pub struct TopBar {
    id: ElementId,
    title: SharedString,
    subtitle: SharedString,
    /// 返回那颗的说明：tooltip 与无障碍标签都用它（图标按钮不能没有名字）。
    back_tooltip: SharedString,
    /// 文案要不要也画出来。手机上画（读者认「返回」比认箭头快），阅读页那一条不画 ——
    /// 它与右侧书签、目录开关共用一条，返回保持最窄。
    show_back_label: bool,
    /// 返回那颗走 `small` 档（与同一排里的其它小按钮同高）。
    back_small: bool,
    height: f32,
    pad_x: f32,
    /// 标题走 SEMIBOLD：那是「这一屏叫什么」的位置。
    strong: bool,
    /// 压在内容之上的那一档。
    overlay: bool,
    /// `overlay` 那一档：这一条自己要让给系统的那一段（状态栏 / 挖孔，逻辑像素）。
    /// 表面从调用方顶边起，内容行从让完之后那条边起。
    overlay_top: f32,
    on_back: Option<OnBack>,
    trailing: Vec<AnyElement>,
}

impl TopBar {
    /// 默认是**在流里**的那一条：44 高、`PAD_X` 内缩、返回那颗只有图标。
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            title: SharedString::default(),
            subtitle: SharedString::default(),
            back_tooltip: SharedString::default(),
            show_back_label: false,
            back_small: false,
            height: HEIGHT,
            pad_x: PAD_X,
            strong: false,
            overlay: false,
            overlay_top: 0.0,
            on_back: None,
            trailing: Vec::new(),
        }
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = title.into();
        self
    }

    /// 标题右边那点次要读数（阅读页放当前章名）。空串不画。
    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    /// 返回那颗的说明文案（三处都给的是同一句 `shelf.back`）。
    pub fn back_tooltip(mut self, label: impl Into<SharedString>) -> Self {
        self.back_tooltip = label.into();
        self
    }

    /// 把那句说明也画在箭头右边。
    pub fn show_back_label(mut self) -> Self {
        self.show_back_label = true;
        self
    }

    pub fn back_small(mut self) -> Self {
        self.back_small = true;
        self
    }

    pub fn pad_x(mut self, pad: f32) -> Self {
        self.pad_x = pad;
        self
    }

    /// 换掉那一条的高。调用方若已经有一个数在管这一条（阅读页的
    /// `chrome::TOP_BAR_HEIGHT` 同时给布局与画面用），必须递进来 ——
    /// 组件里再留一个「默认 44」的第二真相源，改一处就会让浮层与它让出的高度不一致。
    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    /// 标题当「这一屏叫什么」用（SEMIBOLD）。
    pub fn strong(mut self) -> Self {
        self.strong = true;
        self
    }

    /// 换成压在内容之上的那一档：满宽吸顶、popover 表面、只圆下面两个角、
    /// 拦住落在它空隙里的一击。
    ///
    /// `top_inset` 是这一条**自己**要让给系统的那一段（状态栏 / 挖孔，逻辑像素；
    /// 桌面上是 0）。表面从调用方的顶边起 —— 阅读页那一层从窗口 0 起（外壳不再推整层，
    /// 见 `shell::reading_overlay`），所以这一条是从屏幕顶边长出来的，上面不该有缝，
    /// 而投影上渗的那一段也因此落在屏幕外，不会在条子上方压出一条横线（见
    /// [`overlay_shadow`]）。内容行让开这一段：返回那颗不许压在状态栏或挖孔下面。
    pub fn overlay(mut self, top_inset: f32) -> Self {
        self.overlay = true;
        self.overlay_top = top_inset.max(0.0);
        self.pad_x = OVERLAY_PAD_X;
        self
    }

    pub fn on_back(mut self, action: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_back = Some(Box::new(action));
        self
    }
}

impl ParentElement for TopBar {
    /// 右侧槽：副题之后依次排开的那几颗（阅读页宽档上的「目录」开关）。
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.trailing.extend(elements);
    }
}

impl RenderOnce for TopBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let radius = cx.theme().radius_lg;
        let back = self.on_back;
        // 表面向上长回的那一段：只算给 `overlay` 那一档，在流里的那一条没这回事。
        let raised = if self.overlay { self.overlay_top } else { 0.0 };
        // 内容那一行：返回、标题、副题、右端那几颗共用一条基线，行高是 [`ROW`] 那一个数
        // （不是按最高的孩子自己撑 —— 那样带小按钮的那几条会把标题摆在另一个 `y` 上）。
        // 挂个名给宿主测试：这一行的顶边在不在系统那一段下面，就是「有没有让开安全区」。
        let mut row = h_flex()
            .id("top-bar-row")
            .test_support()
            .w_full()
            .h(rems(ROW))
            .items_center()
            .gap_2()
            .child(back_button(
                self.back_tooltip,
                self.show_back_label,
                self.back_small,
                back,
            ))
            .child(title_cell(self.strong, self.title));
        if !self.subtitle.is_empty() {
            row = row.child(
                div()
                    .min_w_0()
                    .max_w(px(SUBTITLE_MAX))
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.subtitle),
            );
        }
        h_flex()
            .id(self.id)
            .when(self.overlay, |this| {
                // `block_mouse_except_scroll` 要挂在有 id 的那一个之上（它是
                // `Stateful<Div>` 的方法）。
                this.block_mouse_except_scroll()
                    .absolute()
                    // 吸在调用方给的那一条顶边上：阅读页那一层从窗口 0 起（外壳不再按
                    // `reading_top_inset` 推整层，纸面铺到系统边），所以这一条的顶边就是
                    // 屏幕顶边。让给系统的那一段在下面用 `pt` 让，不靠这一格往上长 ——
                    // 往上长回去的话，内容行正好落回窗口 0，返回那颗就压在状态栏／挖孔下面。
                    .top_0()
                    .left_0()
                    .right_0()
                    .popover_style(cx)
                    // 盖掉 popover_style 里那圈 ring —— 它会在这一条上方压出一条横线，见
                    // [`overlay_shadow`]。
                    .shadow(overlay_shadow())
                    // 只圆下面两个角：这一条是从屏幕顶边长出来的，上面不该有缝。
                    .corner_radii(Corners {
                        top_left: px(0.),
                        top_right: px(0.),
                        bottom_left: radius,
                        bottom_right: radius,
                    })
            })
            // 满宽两档各有画法：在流里的那一条靠 `w_full`，压在内容之上的那一条靠
            // `left_0` + `right_0`（与 `bands::band` 同一套）—— 同时给两套，等于让
            // 宽度和两条边各说一次话，改哪一头都会静默不一致。
            .when(!self.overlay, |this| this.w_full())
            // 长高的那一段用 `pt` 让给系统，内容行从让完之后那条边起。
            .when(raised > 0.0, |this| this.pt(px(raised)))
            .h(px(self.height + raised))
            // 内容行贴上沿，不是居中（为什么，见 [`HEIGHT`]）。
            .items_start()
            .px(px(self.pad_x))
            .child(row.children(self.trailing))
            .into_any_element()
    }
}

/// 最左那颗返回。图标永远是 `ArrowLeft`，文字按调用方的两档开关走。
fn back_button(
    tooltip: SharedString,
    show_label: bool,
    small: bool,
    on_back: Option<OnBack>,
) -> AnyElement {
    let mut button = Button::new("top-bar-back")
        .ghost()
        .icon(IconName::ArrowLeft);
    if small {
        button = button.small();
    }
    if show_label {
        button = button.label(tooltip.clone().to_string());
    }
    button
        .tooltip(tooltip.clone().to_string())
        .accessibility_label(tooltip)
        .on_click(move |_, window, cx| {
            if let Some(on_back) = on_back.as_ref() {
                on_back(window, cx);
            }
        })
        .into_any_element()
}

/// 标题那一格：吃掉剩下的宽度、单行截断。
fn title_cell(strong: bool, title: SharedString) -> AnyElement {
    div()
        .flex_1()
        .min_w_0()
        .truncate()
        .text_sm()
        .when(strong, |this| this.font_weight(FontWeight::SEMIBOLD))
        .child(title)
        .into_any_element()
}

#[cfg(all(test, not(mobile)))]
mod view_tests {
    //! `overlay` 那一档的几何：表面贴窗口顶边，内容行让开系统那一段。
    //!
    //! 这条从前是反的：阅读页那一层已经不再按 `shell::reading_top_inset` 往下推（纸面从
    //! 窗口 0 起），而这一条还按「父层已经让过」往上长回 `-inset`，于是 `top(-inset)` 与
    //! `pt(inset)` 互相抵消 —— 返回那颗正好落在窗口第 0 行，也就是状态栏／挖孔下面。
    //! 判据量的是**内容行**的顶边，不是条子自己的顶边：只有前者说得清「有没有让开」。

    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Context, InteractiveElement as _, IntoElement,
        ParentElement as _, Render, Styled as _, Window, div, px, size,
    };

    use super::TopBar;

    /// 顶上那条系统边有多高：真机上大约是状态栏 + 一点余量，取一个不属于 44 的整数倍、
    /// 也不等于 0 的数，免得抵消掉了还量不出来。
    const TOP_INSET: f32 = 41.0;

    struct BarHost {
        /// 这一条要不要按 `overlay` 那一档画，以及它让给系统的那一段有多高。
        inset: Option<f32>,
    }

    impl Render for BarHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let mut bar = TopBar::new("test-top-bar")
                .title("《猫城记》")
                .back_tooltip("返回");
            if let Some(inset) = self.inset {
                bar = bar.overlay(inset);
            }
            div().id("bar-host").size_full().relative().child(bar)
        }
    }

    fn boot(cx: &mut gpui_kit::TestAppContext, inset: Option<f32>) -> AnyWindowHandle {
        cx.update(gpui_kit::init);
        cx.open_window(size(px(400.0), px(600.0)), |window, cx| {
            let view = cx.new(|_| BarHost { inset });
            Root::new(view, window, cx)
        })
        .into()
    }

    /// 内容行那一格的顶边（窗口坐标，实测）。
    fn row_top(cx: &mut gpui_kit::TestAppContext, handle: AnyWindowHandle) -> f32 {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            f32::from(window.find("top-bar-row").bounds().origin.y)
        })
        .expect("渲染应成功")
    }

    /// `overlay` 那一档：内容行从让完之后那条边起 —— 返回那颗不压在状态栏／挖孔下面。
    #[gpui_kit::test]
    fn the_overlay_row_yields_the_system_band(cx: &mut gpui_kit::TestAppContext) {
        let handle = boot(cx, Some(TOP_INSET));
        assert_eq!(
            row_top(cx, handle),
            TOP_INSET,
            "让开那一段之后内容行才落在系统边下面"
        );
    }

    /// 桌面上那一段是 0：内容行贴条子自己的顶边，一个像素都不多让。
    #[gpui_kit::test]
    fn the_overlay_row_does_not_invent_an_inset_on_desktop(cx: &mut gpui_kit::TestAppContext) {
        let handle = boot(cx, Some(0.0));
        assert_eq!(row_top(cx, handle), 0.0);
    }

    /// 在流里的那一条（架面、书信息、紧凑设置）不认这一档：顶边就是它自己的顶边，
    /// 带高也仍是 `HEIGHT`，没有被系统那一段撑高。
    #[gpui_kit::test]
    fn the_in_flow_row_keeps_its_own_top(cx: &mut gpui_kit::TestAppContext) {
        let handle = boot(cx, None);
        assert_eq!(row_top(cx, handle), 0.0);
    }
}
