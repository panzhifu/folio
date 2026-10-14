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
//! * **返回那颗永远在最左**，图标是 `ArrowLeft`，文案由调用方给（不给就只画图标 ——
//!   阅读页那一条满宽、右边还要摆章名，带文字的返回会把书名挤没）。
//! * **标题吃掉剩下的宽度并截断**（`flex_1` + `truncate`）：书名与页名都可能很长，
//!   一条顶栏不许把它们撑成两行。
//! * **副题在右边、给上限而不是 `flex_1`**：长章名会把标题挤成三个字，而读者认路的
//!   顺序是先「哪本书」再「哪一章」。
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
    AnyElement, App, Corners, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, RenderOnce, SharedString, Styled, Window, div, px,
};

/// 这一条的高：44 是拇指稳定命中的下限，三处用同一个数（原来一处按内容撑、一处 44、
/// 一处吸顶那条 44 —— 同一个应用里同一条东西三种高）。
const HEIGHT: f32 = 44.0;
/// 默认左右内缩，与底栏那一排入口的 `px_3` 同一档。
const PAD_X: f32 = 12.0;
/// `overlay` 那一档的内缩：阅读页底栏用的是 `px_4`，两条要对齐。
const OVERLAY_PAD_X: f32 = 16.0;
/// 副题的宽度上限。见模块注释第三条。
const SUBTITLE_MAX: f32 = 320.0;

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
    /// 它右边还要摆当前章名，带文字的返回会把书名挤成三个字。
    show_back_label: bool,
    /// 返回那颗走 `small` 档（与同一排里的其它小按钮同高）。
    back_small: bool,
    height: f32,
    pad_x: f32,
    /// 标题走 SEMIBOLD：那是「这一屏叫什么」的位置。
    strong: bool,
    /// 压在内容之上的那一档。
    overlay: bool,
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
    pub fn overlay(mut self) -> Self {
        self.overlay = true;
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
        let mut bar = h_flex()
            .id(self.id)
            .when(self.overlay, |this| {
                // `block_mouse_except_scroll` 要挂在有 id 的那一个之上（它是
                // `Stateful<Div>` 的方法）。
                this.block_mouse_except_scroll()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .popover_style(cx)
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
            .h(px(self.height))
            .items_center()
            .gap_2()
            .px(px(self.pad_x))
            .child(back_button(
                self.back_tooltip,
                self.show_back_label,
                self.back_small,
                back,
            ))
            .child(title_cell(self.strong, self.title));
        if !self.subtitle.is_empty() {
            bar = bar.child(
                div()
                    .min_w_0()
                    .max_w(px(SUBTITLE_MAX))
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.subtitle),
            );
        }
        bar.children(self.trailing).into_any_element()
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
