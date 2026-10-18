//! 架面那三块「要读者答一句话」的浮层的**容器**：建组、从链接导入、扫描结果。
//!
//! 同一份内容，两种容器：桌面与平板上是一张 [`Popover`]，手机的 compact 档上是同一块
//! 内容压进 [`BottomSheet`]。换容器换的不是好看，是那张卡钉不钉得住：手机上 ⋮ 那颗按钮
//! 离屏幕右边只有十几像素，从它下面开一张 420 宽的卡有一半要掉到屏幕外，而底部弹层
//! 天生满幅。
//!
//! 面板本体（表单里那几行、清单那几列）留在各自的文件里（[`super::view`] / [`super::scan`]），
//! 这一层只管两件事：装进哪一块容器、读者自己关掉时派回哪儿。
//!
//! 内容一律是**在外头搭好的元素**而不是闭包：`Popover` 的 `.content()` 每帧重跑一遍，
//! 而搭那一屏要 `&mut Context<ShelfView>`（按钮的回调要架面的句柄），弹窗自己的绘制
//! 回调里只有 `&mut App`。`.child()` 收的是搭完的那一块，一次搭好、挂在弹窗层里，
//! 两件事就都成立。
//!
//! 三块弹层各握一份 [`BottomSheetState`]，不是共用一份：共用一份的话，A 屏正在往下沉的
//! 那半程被 B 屏接管，读者看到的是 A 的内容原地换成 B。退场动画各归各的那一份才对得上
//! 「我刚关掉的是哪一屏」。

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::popover::Popover;
use gpui_kit::{Anchor, AnyElement, Context, Entity, IntoElement, ParentElement, Styled, div, px};

use crate::ui::components::{BottomSheet, BottomSheetState};

use super::view::ShelfView;

/// `Popover` 那一圈写死的内边距（组件里的 `p_3`，默认 rem 下 12 逻辑像素）。
///
/// 居中要往上抬的是**整张卡**的一半，而卡的高 = 内容高 + 上下这一圈，所以这个数要在
/// 这里出现一次。它由组件写死、不由主题给：改了组件那一处而没改这里，浮层会偏出半个
/// 内边距（24 像素的一半）。
const POPOVER_PAD: f32 = 12.0;

/// 「从链接导入」那张居中浮层的宽与内容高。
pub(in crate::ui::features::shelf) const URL_WIDTH: f32 = 420.0;
pub(in crate::ui::features::shelf) const URL_HEIGHT: f32 = 152.0;

/// 扫描结果那张。高是**定死**的而不是「至少」：里面是一条虚拟列表，列表要滚就得有一个
/// 能滚的框，「至少多高」给不出这个框。
pub(in crate::ui::features::shelf) const SCAN_WIDTH: f32 = 460.0;
pub(in crate::ui::features::shelf) const SCAN_HEIGHT: f32 = 460.0;

/// 建组那张的宽：比另两张窄，因为它只有「一个名字 + 两颗按钮」。它钉在按钮底下，
/// 不需要报高度 —— 只有居中的那两张要靠高度算往上抬多少。
pub(in crate::ui::features::shelf) const GROUP_WIDTH: f32 = 340.0;

/// 底部弹层展开那一档占视口高的比例。
///
/// 比目录那一屏（九成）矮一档：这三块是「答一句话就走」的层，压掉九成屏幕等于把架面
/// 整个藏起来，而读者关掉它之后要看的就是那片架面。扫描那一屏要看得下几行清单，
/// 所以它走 [`SCAN_SHEET_EXPANDED`] 那一档。
pub(in crate::ui::features::shelf) const SHEET_EXPANDED: f32 = 0.6;
pub(in crate::ui::features::shelf) const SCAN_SHEET_EXPANDED: f32 = 0.85;
/// 一张浮层。`anchor`/`offset` 决定它钉在哪儿，`close` 只在**读者自己关掉**那一刻派回去
/// （点卡外面、按返回）—— 关掉之后把架面那份状态清掉的是调用方，不是这里。
///
/// 那颗零尺寸的触发器不是省事：`Popover` 要拿一个元素的 bounds 当锚点，而这三屏的开关
/// 握在架面的状态里，不是握在「点了这颗按钮」里 —— 长按菜单那条路开出来的建组面板，
/// 触发它的那个元素下一帧就不在了。与阅读页脚注那一屏同一条路
/// （`reader::page` 的 `footnote_popover`）。
///
/// `offset` 沿锚轴方向，可以为负：居中那两张靠它往上抬掉半张卡的高。
fn popover(
    key: &'static str,
    anchor: Anchor,
    offset: f32,
    shelf: &Entity<ShelfView>,
    close: impl Fn(&mut ShelfView, &mut Context<ShelfView>) + 'static,
    body: AnyElement,
) -> AnyElement {
    let dismiss = shelf.clone();
    Popover::new(key)
        .anchor(anchor)
        .offset(px(offset))
        .open(true)
        .on_open_change(move |open, _, cx| {
            if *open {
                return;
            }
            dismiss.update(cx, |this, cx| close(this, cx));
        })
        .trigger(
            Button::new(format!("{key}-trigger"))
                .ghost()
                .p_0()
                .w(px(0.))
                .h(px(0.))
                .min_w(px(0.))
                .min_h(px(0.)),
        )
        .child(body)
        .into_any_element()
}

/// 把一张浮层挂在**内容区的正中央**。
///
/// 一层铺满的 flex 容器把那颗零尺寸的触发器推到正中，卡片本体由 `Popover` 画在弹窗层里、
/// 不占这一层的任何位置 —— 所以这一层既不给卡片定尺寸，也不接点击（它没有事件监听，
/// gpui 的命中测试不收它，底下那片格子照旧点得动）。
///
/// `height` 是调用方**报上来**的整张卡高，不是画完再量的：`Popover` 开完之后不回调报尺寸，
/// 而「往上抬半张高」需要一个数。报的数与真实高对不上时卡会偏上或偏下几像素 —— 不截断、
/// 不挡别的东西，这是换来「不必等一帧才定得准」的代价。
pub(in crate::ui::features::shelf) fn centered(
    key: &'static str,
    height: f32,
    shelf: &Entity<ShelfView>,
    close: impl Fn(&mut ShelfView, &mut Context<ShelfView>) + 'static,
    body: AnyElement,
) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(div().size(px(0.)).child(popover(
            key,
            Anchor::TopCenter,
            -(height + POPOVER_PAD * 2.0) / 2.0,
            shelf,
            close,
            body,
        )))
        .into_any_element()
}

/// 钉在某个锚点旁边的浮层（建组那一张：跟着顶部那颗分组按钮走）。
pub(in crate::ui::features::shelf) fn anchored(
    key: &'static str,
    anchor: Anchor,
    offset: f32,
    shelf: &Entity<ShelfView>,
    close: impl Fn(&mut ShelfView, &mut Context<ShelfView>) + 'static,
    body: AnyElement,
) -> AnyElement {
    popover(key, anchor, offset, shelf, close, body)
}

/// 移动端的一块底部弹层。
///
/// 收稳了那一程**照样要交元素**（弹簧的采样在里面），只是不给内容 —— 判据是
/// [`BottomSheetState::is_idle`]，不是 `is_open()`：退场那半程 `is_open()` 已经是 `false`
/// 而卡片还浮在屏幕上，那时断掉内容就等于看着它空着沉下去。判内容留不留的活儿在调用方
/// （`ShelfView` 那一侧握着内容），这里只负责装。
pub(in crate::ui::features::shelf) fn sheet(
    key: &'static str,
    state: &Entity<BottomSheetState>,
    expanded: f32,
    body: Option<AnyElement>,
) -> AnyElement {
    let sheet = BottomSheet::new(state, key).expanded(expanded);
    match body {
        Some(body) => sheet.child(body).into_any_element(),
        None => sheet.into_any_element(),
    }
}
