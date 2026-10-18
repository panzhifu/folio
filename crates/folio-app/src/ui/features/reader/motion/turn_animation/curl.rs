//! 仿真翻页：一张纸在折痕处**卷起来**。
//!
//! 纸模型是「卷成一段圆筒」，不是「沿一条直线折」。两者在几何上差一个自由度 —— 弯曲半径
//! —— 而观感的差别全在这一格上：硬折的纸是一条直边扫过屏幕，卷起来的纸在折痕旁边有一段
//! **受光的圆筒**，于是纸有了厚度。
//!
//! 参考的几处公开实现（都是「卷筒」这一派，`XBPageCurl` 是 iBooks 那一档的开源复刻）：
//!
//! - **XBPageCurl**（OpenGL ES，iOS）：把纸的顶点投影到一段可移动/缩放/旋转的**虚拟圆柱**上，
//!   圆柱的受光决定明暗 —— 本文件的 `ROLL` / `ROLL_INK` / `ROLL_LIGHT` 那条「暗—亮—暗」
//!   就是它那条受光曲线的二维近似；
//! - **PaperStack**（iOS，`PageCurl` 那个 CIFilter 的几何说明）：卷筒的半径与折线方向由
//!   手指位置与纸的挺度定，**不随拖动距离变化** —— 对应 [`ROLL_LIFT`]：卷径前 12% 长出来，
//!   之后定住；
//! - **Hong et al., "Turning Pages of 3D Electronic Books"**：纸绕**圆锥**而不是圆柱弯曲，
//!   锥尖落在书脊那条线上，所以整页纸不会被拉长。folio 画不出锥（正文是逐字形自绘的、
//!   用不了投影变换，见下），圆柱在二维投影里与本档要的那条受光带一致，就取圆柱；
//! - **StPageFlip**（canvas）：折线怎么算（角与其镜像连线的垂直平分线）沿用它的做法，
//!   [`crease_of`] 那一格没动。
//!
//! 画面上**没有转了角度的字**：gpui 的 `paint_glyph` 把字形 sprite 的变换写死成单位阵
//! （`gpui-pre-0.3.9/src/window.rs:4882`），GPU 侧的顶点着色器认得那个矩阵，缺的只是参数
//! 没透出来。所以卷筒上画的是**纸**，不是转过弯的字 —— 这一点与 iBooks 一致（它卷起来
//! 那一段也不带正文）。等 gpui 透出那个矩阵，这张纸才能真正贴上字。

use super::{Area, Axis, Crease, Cut, Grip, PageTurnStyle, Poly, Shade, Shades, Sheet, Tint};
use crate::ui::features::reader::model::chrome::PageMode;

pub(in crate::ui::features::reader) const CORNER_TILT: f32 = 0.1;

/// **卷筒半径**占页短边的几成。
///
/// 这是「折成硬片」与「卷成圆筒」之间唯一多出来的自由度，也是苹果那种仿真感的来源：
/// 纸不是绕一条线转过去的，而是在折痕处**卷起来**一段，正对读者的那面被照亮、两次贴着
/// 切线的地方压暗 —— 于是一条纸上出现「暗—亮—暗」，读起来才有厚度。
///
/// 取 5.5%：手机上（短边 390）卷径约 21px、整条卷筒 42px 宽，与 iBooks 的量级一致；
/// 再粗就是「一捆纸」，再细那条高光会窄到看不见。
pub(in crate::ui::features::reader) const ROLL: f32 = 0.07;

/// 卷筒起势：翻页进度前几成才把卷筒**长出来**。
///
/// 不这么写，一按下去就有一根满宽的圆筒贴在那儿 —— 而那一刻纸其实还没离页。
/// 12% 是个手感值：比一次点按的加速段还短，读者只会觉得「纸是被拎起来的」。
const ROLL_LIFT: f32 = 0.12;

/// 卷筒贴折线与贴外缘那两处的黑度（卷筒两侧都是切线，受光最少）。
const ROLL_INK: f32 = 0.34;

/// 卷筒的受光拆成几条**平的**带。
///
/// 为什么不用两条互相对着的渐变（那是上一版）：gpui 的渐变只有两个色标（`Background`
/// 装的是 `[LinearColorStop; 2]`），两段渐变各有一端是满墨、另一端透明，接在卷顶上就是
/// 两道**边是硬的**灰条 —— 真机上读到的正是「后面有两条灰色的条」。平带子按 `sin` 取浓淡，
/// 一条比一条淡，边是软的，卷筒才读成一根圆筒。
const ROLL_BANDS: usize = 12;

/// 卷筒正对读者那一侧的高光有多亮。压过了就成一条白棍。
const ROLL_LIGHT: f32 = 0.10;

/// 纸**背面**（折起来那一张）压暗多少。
///
/// 0.0 = 不压：背面就是纸色。苹果那一档折起来的背面也是一整片干净的纸，压暗会让它读成
/// 「一块半透明的灰色贴纸」—— 上一版那第二条灰条就是它。
const BACK_INK: f32 = 0.0;

/// 卷筒抬起来之后投在下面那张纸上的影，最宽到页宽的几成。
///
/// 取 0.75：那是 StPageFlip 的 `width = pageWidth * 3/4 * progress`（`Render.ts` 的
/// `setShadowData`）。比这个窄，影子就像贴在折痕上的一条硬带，看不出「整张纸被抬起来」。
const SHADOW_WIDE: f32 = 0.75;

/// 那道投影起步有多黑。
const SHADOW_INK: f32 = 0.26;

/// 折痕**内侧**那道影：贴着卷筒的外缘、盖在纸背上，宽度按行程收（同 StPageFlip 的
/// `drawHardInnerShadow`：`(100 − progress) * 2.5 * pageWidth / 100 + 20`）。
///
/// 有没有这一道，是「一张纸被折起来」与「一块灰色形状贴在纸上」之间的差别 —— 折页最难看的
/// 读数就是纸背整片均匀、边缘一刀切。
const INNER_WIDE: f32 = 0.35;

/// 折痕内侧那道影起步有多黑。
const INNER_INK: f32 = 0.34;

/// 走了 `progress` 那一刻的折线。往回翻是同一件事沿页的中线镜像。
///
/// `None` = 这块版心还没量出面积，或角与它的原位置重合到折线无从定义。
pub(in crate::ui::features::reader) fn crease_of(
    progress: f32,
    forward: bool,
    area: Area,
) -> Option<Crease> {
    crease_at(progress, forward, area, None)
}

/// 同 [`crease_of`]，但带上读者这一笔的手（起手点 + 此刻的位置）。
///
/// **折线就是「那个角」与「手指此刻」的垂直平分线** —— 与 StPageFlip 那套点屏折叠同一条
/// 判据，只是被拖的点从「沿一条写死的对角线走」换成了手指本身。于是：
///
/// - 起折的角看 `from`：落在下半屏从右下角卷起，落在上半屏从右上角卷起（从前角是常数，
///   所以竖屏上「只有上面能翻」）；
/// - 折线的倾角看 `at`：手指往左划、斜着划、往上划，折痕跟着转（从前它只会沿一条对角线走）。
pub(in crate::ui::features::reader) fn crease_at(
    progress: f32,
    forward: bool,
    area: Area,
    grip: Option<Grip>,
) -> Option<Crease> {
    if area.width() <= 0.0 || area.height() <= 0.0 {
        return None;
    }
    let t = progress.clamp(0.0, 1.0);
    let from_bottom = grip.is_some_and(|grip| grip.from.1 > 0.5);
    let (edge, inward) = if from_bottom {
        (area.bottom, -1.0)
    } else {
        (area.top, 1.0)
    };
    let corner = (area.right, edge);
    let dragged = match grip {
        // 手指握着：折线跟着手指走（两个轴都跟）。
        Some(grip) => (
            area.left + grip.at.0 * area.width(),
            area.top + grip.at.1 * area.height(),
        ),
        // 没有手指（点屏 / 按键 / 滚轮）：沿用「角沿对角线走到页外」那条写死的路径。
        None => {
            let tilt = CORNER_TILT * area.height();
            let start = (area.right - tilt, edge + inward * tilt);
            let dest = (area.right - 2.0 * area.width(), edge);
            (
                start.0 + (dest.0 - start.0) * t,
                start.1 + (dest.1 - start.1) * t,
            )
        }
    };
    let crease = Crease::from_corner(corner, dragged)?;
    Some(if forward {
        crease
    } else {
        // x ↦ left + right − x 代进 `past`：法向反号，`d` 补回那段镜像平移。
        Crease {
            nx: -crease.nx,
            ny: crease.ny,
            d: crease.d - crease.nx * (area.left + area.right),
        }
    })
}

/// 折页那一档这一帧要盖上去的那几块：一张卷起来的纸 + 它投在下面那张上的影。
///
/// 画的东西从远到近（也就是从 `past` 负得多的地方往折线走）：
///
/// 1. **纸背**：被折走那一片绕折线镜像之后落在平摊侧 —— 读者看到的是纸的背面，
///    所以它先铺一层页色，再压一道朝折线渐深的暗，免得读成一块剪出来的硬纸板；
/// 2. **卷筒**：折痕那一侧 `2R` 宽的一条。纸面铺一层，两侧各压一道切线暗、中间一道高光
///    —— 这三段合起来就是「纸在这里卷过去」的全部观感；
/// 3. **投影**：卷筒悬在纸面上方，下面那张纸被压出一道从卷筒外缘往外淡出的影。
///
/// 三处都不碰字形：正文是逐字形自绘的，转角度的字画不出来（`paint_glyph` 的变换是
/// 单位阵），所以卷筒上画的就是**纸**，不是转过弯的字 —— 这一点与 iBooks 一致。
pub(in crate::ui::features::reader) fn fold_shades(
    progress: f32,
    forward: bool,
    area: Area,
) -> Shades {
    fold_shades_at(progress, forward, area, None)
}

/// 同 [`fold_shades`]，带上读者这一笔的起手点（决定从哪个角起折）。
pub(in crate::ui::features::reader) fn fold_shades_at(
    progress: f32,
    forward: bool,
    area: Area,
    grip: Option<Grip>,
) -> Shades {
    let mut out = Shades::new();
    let Some(crease) = crease_at(progress, forward, area, grip) else {
        return out;
    };
    let page = Poly::rect(area);
    let t = progress.clamp(0.0, 1.0);
    // 卷筒的半径：前 12% 长出来，之后定住。定住这一条也是物理的 —— 纸的弯曲半径由
    // 纸的挺度决定，不随手指走多远而变。
    let radius = ROLL * area.width().min(area.height()) * (t / ROLL_LIFT).min(1.0);
    let roll = 2.0 * radius;

    // ① 纸背。裁掉卷筒占的那一段：那一小截归卷筒自己画，两边都画会叠出一道深边。
    //
    // 下界用一个**有限的**很大的数而不是 `NEG_INFINITY`：`Poly::clip` 在跨边界那一支要算
    // `sa / (sa - sb)`，喂进无穷大就是 `inf / inf = NaN`，顶点一旦被 NaN 毒到，那一块
    // 画出来是一大片斜着的淡色楔子（真机第一版就是这个读数）。
    let far = -(area.width() + area.height()) * 4.0;
    let flap = page
        .past(crease)
        .reflected(crease)
        .inside(area)
        .slab(crease, far, -roll);
    push(&mut out, flap, Tint::Paper, None, 1.0);
    if BACK_INK > 0.0 {
        push(
            &mut out,
            flap,
            Tint::Dark,
            Some(crease.angle_back()),
            BACK_INK,
        );
    }

    // ①′ 折痕**内侧**的影：贴卷筒外缘压在纸背上，随行程收窄、变淡。
    //
    // 画在纸背之上、卷筒之下 —— 它是「纸背靠近折痕那一侧被压暗」，不该盖住卷筒。
    let inner = INNER_WIDE * area.width() * (1.0 - t) + 8.0;
    if t > 0.02 {
        push(
            &mut out,
            page.slab(crease, -roll - inner, -roll),
            Tint::Dark,
            Some(crease.angle_back()),
            INNER_INK * (1.0 - 0.4 * t),
        );
    }

    // ② 卷筒：一整块纸面 + 几条**平**的暗带。
    //
    // 受光按圆柱取：`u` 从折线（0）走到外缘（1），明暗 ∝ 1 − sin(πu) —— 两个切线处最暗、
    // 正对读者那一侧最亮。带中点采样，前六条带子就是「暗—渐亮—最亮—渐暗—暗」。
    let body = page.slab(crease, -roll, 0.0);
    push(&mut out, body, Tint::Paper, None, 1.0);
    for band in 0..ROLL_BANDS {
        let u0 = band as f32 / ROLL_BANDS as f32;
        let u1 = (band + 1) as f32 / ROLL_BANDS as f32;
        let mid = (u0 + u1) * 0.5;
        let shadow = 1.0 - (std::f32::consts::PI * mid).sin();
        let ink = ROLL_INK * shadow;
        // 卷顶那几条几乎没有墨，推上去只是占配额。
        if ink < 0.012 {
            continue;
        }
        push(
            &mut out,
            page.slab(crease, -roll * u1, -roll * u0),
            Tint::Dark,
            None,
            ink,
        );
    }
    // 卷顶那道高光：正对读者的那一窄条，窄且淡。
    push(
        &mut out,
        page.slab(crease, -radius * 0.6, -radius * 0.4),
        Tint::Light,
        None,
        ROLL_LIGHT,
    );

    // ③ 投影：卷筒悬在纸面上方，下面那张纸被压出一道宽而软的影。
    // 宽度随行程长、墨色随行程淡：走到最后纸已经离场，不该还留一条带子。
    let wide = SHADOW_WIDE * area.width() * t;
    if wide > 0.5 {
        push(
            &mut out,
            page.slab(crease, -roll - wide, -roll),
            Tint::Dark,
            Some(crease.angle_back()),
            SHADOW_INK * (1.0 - t),
        );
    }
    out
}

/// 只把**画得出来**的那一块放进去：零面积的形状（折到尽头、裁到页外、还没长出来）
/// 在绘制层也是空转，提前丢掉能让「这一帧还剩几块」这个读数干净。
fn push(out: &mut Shades, shape: Poly, tint: Tint, angle: Option<f32>, ink: f32) {
    if shape.is_empty() || shape.area().abs() < 0.5 || ink <= 0.0 {
        return;
    }
    out.push(Shade {
        shape,
        tint,
        angle,
        ink,
    });
}

/// 仿真翻页：一张纸被折走的样子。
///
/// **纸模型是「沿一条直线折」，不是「卷成圆柱」**。前者是 StPageFlip（canvas 那一族事实
/// 上的标准库）的做法：纸当一张不会弯的硬片，绕折线做刚体翻转，全部仿真感来自折线的倾角
/// 加沿折线的两条渐变阴影。另一派是 crengine 的 `drawDistorted`：逐列横向压缩，纸真的弯
/// 成一条弧 —— 那一派要求「压扁一张页面位图」，而 folio 的正文是逐字形自绘的，没有位图可压。
///
/// 与旧版那一档的三处区别，就是「奇怪」那三个读数各自的根：
///
/// 1. **动的是旧页**。旧页被折走，新页平躺在它下面等着露出来；先前是 `Cover` 的几何 ——
///    新页盖进来、旧页停在原地被裁字，受力方向整条反了。
/// 2. **折线有倾角**，而且随行程自己变直（见 [`CORNER_TILT`]）。先前 `Seam` 只有一个横
///    坐标，那条边是竖直扫过屏幕的一根棍。
/// 3. **折起来那一面画的是纸**（[`fold_shades`]：一张纸面 + 一道投影 + 折谷暗—亮—暗三段，
///    全部随走了多少生长）。先前盖在
///    接缝上的是不透明页色矩形，宽按 `α − sin α` 反解走，整个行程几乎不塌。
///
/// 折面上**没有转了角度的字**：gpui 的 `paint_glyph` 把字形 sprite 的变换写死成单位阵
/// （gpui-pre-0.3.9 `src/window.rs:4882`），GPU 侧的顶点着色器其实认这个矩阵，缺的只是
/// 参数没透出来。所以这一档画的是纸的背面。
pub(in crate::ui::features::reader) struct Curl;

impl PageTurnStyle for Curl {
    fn axis(&self) -> Axis {
        Axis::Horizontal
    }

    /// 折页**两轴都认**：折线跟着手指走，从右下角往左上划是最自然的一笔。
    fn claims(&self, _axis: Axis) -> bool {
        true
    }

    fn animates(&self, _: PageMode) -> bool {
        true
    }

    fn sheets(&self, t: f32, forward: bool, area: Area) -> [Sheet; 2] {
        self.sheets_at(t, forward, area, None)
    }

    fn sheets_at(&self, t: f32, forward: bool, area: Area, grip: Option<Grip>) -> [Sheet; 2] {
        let cut = match crease_at(t, forward, area, grip) {
            Some(crease) => crease,
            // 版心还没量出来：两张纸都停在原位整张画，等下一帧。
            None => {
                return [
                    Sheet {
                        offset: (0.0, 0.0),
                        z: 0,
                        cut: None,
                    },
                    Sheet {
                        offset: (0.0, 0.0),
                        z: 0,
                        cut: None,
                    },
                ];
            }
        };
        // 两张纸都不位移：折页里新页本来就平躺在原位，`page_shift` 因此恒为 0，
        // 命中换算什么都不必减 —— 这是这一档比 `Cover` 还省的一处。
        [
            Sheet {
                offset: (0.0, 0.0),
                z: 0,
                // 折线「之内」= 纸已经折走、现在空出来的那一侧 = 新页露出来的地方。
                cut: Some(Cut::new(cut, true)),
            },
            Sheet {
                offset: (0.0, 0.0),
                z: 1,
                // 折线「之外」= 还平摊着的那一侧 = 旧页剩下的字。
                cut: Some(Cut::new(cut, false)),
            },
        ]
    }

    fn shades(&self, t: f32, forward: bool, area: Area) -> Shades {
        fold_shades(t, forward, area)
    }

    fn shades_at(&self, t: f32, forward: bool, area: Area, grip: Option<Grip>) -> Shades {
        fold_shades_at(t, forward, area, grip)
    }
}

#[cfg(test)]
mod tests {
    //! 搬自摆位层那一份：一挡的门跟着那一档走，共用的尺在 `testing.rs`。

    use crate::ui::features::reader::motion::turn_animation::PageTurnStyle as _;
    use crate::ui::features::reader::motion::turn_animation::curl::CORNER_TILT;
    use crate::ui::features::reader::motion::turn_animation::curl::Curl;
    use crate::ui::features::reader::motion::turn_animation::curl::ROLL;
    use crate::ui::features::reader::motion::turn_animation::curl::ROLL_INK;
    use crate::ui::features::reader::motion::turn_animation::curl::SHADOW_INK;
    use crate::ui::features::reader::motion::turn_animation::curl::SHADOW_WIDE;
    use crate::ui::features::reader::motion::turn_animation::curl::crease_at;
    use crate::ui::features::reader::motion::turn_animation::curl::crease_of;
    use crate::ui::features::reader::motion::turn_animation::geometry::{Crease, Grip};
    use crate::ui::features::reader::motion::turn_animation::geometry::{
        POLY_MAX, Poly, SHADES_MAX, Tint,
    };
    use crate::ui::features::reader::motion::turn_animation::testing::{
        H, W, complementary, count, page, paper_of, past_span,
    };

    /// 卷筒**从折痕里长出来**，走完那一刻整条跟着纸一起离场。
    ///
    /// 两端都是硬读数：t = 0 时纸还没被拎起来（一块都不该有）；卷径在 [`ROLL_LIFT`]
    /// 之后定住（纸的弯曲半径由纸的挺度决定，不随手指走多远变）；t = 1 时纸已经整张
    /// 折走，折线扫到页外，一条带子都不该剩下 —— 从前那一版在落位还挂着满宽的带子，
    /// 画面上就是一道白条横在屏上。
    #[test]
    fn a_roll_grows_out_of_the_crease_and_rides_off() {
        // t = 0：角刚被抠起一个小三角 —— 只有纸背那两块，卷筒还没长出来、影也没有。
        let start = Curl.shades(0.0, true, page());
        assert_eq!(
            start.items().len(),
            1,
            "起步只该有纸背那一片，不该有卷筒或投影"
        );
        assert!(
            start
                .items()
                .iter()
                .all(|shade| shade.shape.area().abs() > 0.0),
            "起步那两块不该是空壳"
        );
        assert_eq!(
            Curl.shades(1.0, true, page()).items().len(),
            0,
            "纸已经整张离场，不该还挂着卷筒或投影"
        );
        // 卷筒那一块（纸面）在折线法向上的宽度：前 12% 长、之后定住。
        let width_at = |t: f32| {
            let shades = Curl.shades(t, true, page());
            let crease = crease_of(t, true, page()).expect("量得出折线");
            // 卷筒本体 = 贴着折线那一块纸色（纸背在它更外侧，跨度随行程缩）。
            shades
                .items()
                .iter()
                .filter(|shade| shade.tint == Tint::Paper)
                .find(|shade| past_span(crease, &shade.shape).1 > -1.0)
                .map(|shade| {
                    let (low, high) = past_span(crease, &shade.shape);
                    high - low
                })
                .unwrap_or(0.0)
        };
        let mut previous = 0.0;
        // 卷筒贴到页左缘之后会被裁掉一截，所以「只长不缩」钉到 85% 为止。
        for step in 1..=17 {
            let t = step as f32 / 20.0;
            let now = width_at(t);
            assert!(
                now >= previous - 0.01,
                "t = {t} 时卷筒反而变窄了：{previous} → {now}"
            );
            previous = now;
        }
        assert!(
            width_at(0.6) > 20.0,
            "卷起来之后该有一条真的卷筒，实得 {}",
            width_at(0.6)
        );
    }
    /// 折面永远落在**还平摊那一侧**、而且裁在页内。这一条是「折过去的纸盖在旧页上，
    /// 不伸到屏幕外面去」的全部内容 —— 折面要是跑到折线的空侧，就是字面意义上的
    /// 「纸穿过了它自己」。
    #[test]
    fn the_flap_always_lies_on_the_flat_side_and_inside_the_page() {
        for forward in [true, false] {
            for step in 1..=20 {
                let t = step as f32 / 20.0;
                let Some(paper) = paper_of(&Curl.shades(t, forward, page())) else {
                    continue;
                };
                if paper.is_empty() {
                    continue;
                }
                let crease = crease_of(t, forward, page()).expect("量得出折线");
                let area = page();
                paper.each(|x, y| {
                    assert!(
                        crease.past(x, y) <= 1e-3,
                        "t = {t} forward = {forward} 时折面有点跑到折线的空侧：({x}, {y})"
                    );
                    assert!(
                        x >= area.left - 1e-3
                            && x <= area.right + 1e-3
                            && y >= area.top - 1e-3
                            && y <= area.bottom + 1e-3,
                        "t = {t} 时折面有点伸到页外：({x}, {y})"
                    );
                });
            }
        }
    }
    /// 倾角随行程**自己变直**：法向的纵向分量从 1/√2 收到 0。这一个读数就是
    /// 「一个常数（[`CORNER_TILT`]）为什么够用」的凭据 —— 若它不收敛，落位那一刻
    /// 那条折线还是斜的，纸就没抬平。
    #[test]
    fn the_crease_straightens_as_the_corner_travels() {
        let start = crease_of(0.0, true, page()).expect("起步量得出折线");
        let end = crease_of(1.0, true, page()).expect("落位量得出折线");
        assert!(
            (start.ny.abs() - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01,
            "起步该是 45° 的一个角，实得 ny = {}",
            start.ny
        );
        assert!(end.ny.abs() < 1e-6, "落位该是正竖的一条线，ny = {}", end.ny);
        assert_eq!(end.past(200.0, 300.0), 200.0, "落位时整页都该在空侧");

        let mut previous = start.ny.abs();
        for step in 1..=20 {
            let now = crease_of(step as f32 / 20.0, true, page())
                .expect("中间每一步都量得出")
                .ny
                .abs();
            assert!(
                now <= previous + 1e-6,
                "t = {} 时倾角又回来了：{previous} → {now}",
                step as f32 / 20.0
            );
            previous = now;
        }
    }
    /// 折线**只往页内走一遍**：起步时角还整页都在平摊侧，落位时整页都在空侧。
    /// 这条钉的是起折点与终点那两个坐标；取错的话纸走半程就已经全没了，或者走满还剩半页。
    #[test]
    fn the_fold_sweeps_the_whole_page_once() {
        let start = crease_of(0.0, true, page()).expect("同上");
        assert!(
            start.past(0.0, 0.0) < 0.0 && start.past(0.0, H) < 0.0 && start.past(W * 0.5, H) < 0.0,
            "起步时页的左边与下边都该还整片平摊"
        );
        let end = crease_of(1.0, true, page()).expect("同上");
        assert!(
            end.past(W * 0.5, 0.0) > 0.0 && end.past(W * 0.5, H) > 0.0,
            "落位时整页都该已经折走了"
        );
        // 走满一整程时，页中心正好在折线上（角走到 −页宽 ⇒ 折线过页的左缘）。
        let middle = crease_of(0.5, true, page()).expect("同上");
        assert!(
            (middle.past(W * 0.25, H * 0.5)).abs() > 1.0,
            "走一半时页的四分之一处该明显还在平摊侧"
        );
    }
    /// 折页两张纸的裁剪互补，而且各归对的那一侧。
    #[test]
    fn the_two_culls_split_the_page_with_no_overlap() {
        let [incoming, outgoing] = Curl.sheets(0.5, true, page());
        let incoming_cut = incoming.cut.expect("折页两张纸都要裁");
        let outgoing_cut = outgoing.cut.expect("同上");
        complementary(incoming_cut, outgoing_cut, page());
        // 页的左右两缘各归一张：左缘还平摊着，右缘已经被折走。按点判，不碰结构体内部。
        assert!(
            outgoing_cut.keeps(0.0, 300.0) && !incoming_cut.keeps(0.0, 300.0),
            "页的左缘该归还停着的旧页"
        );
        assert!(
            incoming_cut.keeps(W - 1.0, 300.0) && !outgoing_cut.keeps(W - 1.0, 300.0),
            "页的右缘已经被折走，该归露出来的新页"
        );
        assert_eq!(incoming.offset, (0.0, 0.0), "折页里新页平躺，不该位移");
        assert_eq!(outgoing.offset, (0.0, 0.0), "旧页被折走的是裁剪，不是位移");
        assert!(
            outgoing.z > incoming.z,
            "旧页必须后画：折面要盖在它上面，而它盖在新页露出来的那块上"
        );
    }
    /// 往回翻是同一件事的镜像：折线从**左**边起，折面往右展开。
    /// 除了这个方向，两边共用同一条判断 —— 少这一条就会只有单向有折页、另一向还是白条。
    #[test]
    fn a_backward_fold_is_the_mirror() {
        for t in [0.2, 0.5, 0.8] {
            let forward = crease_of(t, true, page()).expect("量得出");
            let back = crease_of(t, false, page()).expect("同上");
            let middle = (page().left + page().right) * 0.5;
            // 镜像点上「谁已经折走」必须一致：p 与页中线对称的那一点。
            for x in [0.0, 120.0, 260.0, 400.0] {
                let y = 300.0;
                assert_eq!(
                    forward.past(x, y) > 0.0,
                    back.past(middle + (middle - x), y) > 0.0,
                    "t = {t} 时往回翻不是正向的镜像"
                );
            }
            assert_eq!(forward.ny, back.ny, "纵向分量不该跟着反号");
        }
    }
    /// 投影**在卷筒外面**（不压在卷筒上），宽度随行程长、墨色随行程淡。
    ///
    /// 这三条合起来是「卷筒悬在纸面上方」的全部观感：卷筒自己那一条已经画满了，
    /// 再往里压一道影就会读成「卷筒是黑的」。
    #[test]
    fn the_cast_shadow_sits_outside_the_roll_and_fades_away() {
        let mut previous_wide = -1.0;
        let mut previous_ink = SHADOW_INK + 1.0;
        // 只走到九分：落位那一刻整条都裁没了，那一端由 `a_roll_grows...` 单独钉。
        for step in 4..10 {
            let t = step as f32 / 10.0;
            let shades = Curl.shades(t, true, page());
            let crease = crease_of(t, true, page()).expect("量得出折线");
            let radius = ROLL * W.min(H);
            // 投影永远是**最后一块**（它压在卷筒外面）；没有它说明这一刻影子已经出画了。
            let Some(cast) = shades.items().last().filter(|shade| {
                shade.tint == Tint::Dark
                    && (past_span(crease, &shade.shape).1 + 2.0 * radius).abs() < 2.0
            }) else {
                continue;
            };
            let (low, high) = past_span(crease, &cast.shape);
            assert!(
                high <= -1.0,
                "t = {t} 时投影压到卷筒上去了：外缘 past = {high}"
            );
            assert!(low < high, "t = {t} 时投影是倒着裁的：{low}..{high}");
            assert!(
                (cast.ink - SHADOW_INK * (1.0 - t)).abs() < 1e-6,
                "t = {t} 时墨色不线性：{}",
                cast.ink
            );
            let wide = high - low;
            // 页的左缘会把影子裁掉一截，所以「越翻越宽」只在不贴边的这几步里钉。
            // 页的左缘会把影子裁掉一截，所以「越翻越宽」只钉到影还完整的那几步。
            if t <= 0.4 {
                assert!(wide > previous_wide, "t = {t} 时投影只能越翻越宽");
                previous_wide = wide;
            }
            assert!(cast.ink <= previous_ink, "t = {t} 时墨色只能越翻越淡");
            previous_ink = cast.ink;
        }
    }
    /// 卷筒的受光：**贴折线最暗 → 卷顶最亮 → 贴外缘又暗**。
    ///
    /// 一组平带子按 `1 − sin(πu)` 取浓淡，所以读数只有一条：**从折线往外走，墨色先降后升**。
    /// 上一版是两段互相对着的渐变，接在卷顶上就是两道硬边灰条 —— 真机上读到的
    /// 「后面有两条灰色的条」就是它。这条把「先降后升」钉死，接缝一硬就红。
    #[test]
    fn the_roll_reads_dark_then_light_then_dark() {
        let t = 0.6;
        let shades = Curl.shades(t, true, page());
        let crease = crease_of(t, true, page()).expect("量得出折线");
        let radius = ROLL * W.min(H);
        // 卷筒里的暗带：整块落在 [−2R, 0] 之内（投影在它外面，被这条筛掉）。
        let mut bands: Vec<(f32, f32)> = shades
            .items()
            .iter()
            .filter(|shade| shade.tint == Tint::Dark && shade.angle.is_none())
            .map(|shade| (past_span(crease, &shade.shape), shade.ink))
            .map(|((low, high), ink)| (low, ink.max(high * 0.0 + ink)))
            .collect();
        assert!(!bands.is_empty(), "卷筒里该有暗带");
        bands.sort_by(|a, b| b.0.total_cmp(&a.0)); // 从折线往外
        let inks: Vec<f32> = bands.iter().map(|(_, ink)| *ink).collect();
        assert!(
            inks[0] >= ROLL_INK * 0.6,
            "贴折线那条该是最暗的一档：{inks:?}"
        );
        let mid = inks.len() / 2;
        assert!(inks[mid] < inks[0] * 0.35, "卷顶附近该几乎不压暗：{inks:?}");
        assert!(
            (*inks.last().unwrap() - inks[0]).abs() < 0.06,
            "贴外缘那条该和最暗处一样暗：{inks:?}"
        );
        // 高光只有一窄条，而且落在卷筒里。
        let highlight = shades
            .items()
            .iter()
            .find(|shade| shade.tint == Tint::Light)
            .expect("卷顶该有一道高光");
        let (light_low, light_high) = past_span(crease, &highlight.shape);
        assert!(light_low > -2.0 * radius && light_high < 1.0);
        assert!((light_high - light_low) < radius, "高光只是一窄条");
        assert!(highlight.ink <= 0.12, "高光太亮了：{}", highlight.ink);
    }
    /// **起折的角跟着手指走**：手指落在下半屏就从右下角卷起，落在上半屏从右上角卷起。
    ///
    /// 这一条是「只能上面能翻」那个读数的反面。判据取折线在中线处的纵向位置：从下面起折
    /// 时它偏下（`past` 在页下半的符号相反），从上面起折时偏上。
    #[test]
    fn the_corner_follows_the_finger() {
        let area = page();
        // 起点附近：折的还只是那个角，所以「哪个角折了」这件事判得干净。
        let t = 0.06;
        let upper =
            crease_at(t, true, area, Some(Grip::new((0.5, 0.15), (0.9, 0.2)))).expect("量得出折线");
        let lower =
            crease_at(t, true, area, Some(Grip::new((0.5, 0.85), (0.9, 0.8)))).expect("量得出折线");
        // 判据看**哪个角已经被折走**：这一条与「从哪个角卷起来」是同义的。
        // （折线在横向中线处上下两条正好左右镜像，所以不能拿中线取点比较。）
        assert!(
            upper.past(area.right, area.top) > 0.0,
            "从上往下卷：右上角该已经折走"
        );
        assert!(
            upper.past(area.right, area.bottom) < 0.0,
            "从上往下卷：右下角该还平摊着"
        );
        assert!(
            lower.past(area.right, area.bottom) > 0.0,
            "从下往上卷：右下角该已经折走"
        );
        assert!(
            lower.past(area.right, area.top) < 0.0,
            "从下往上卷：右上角该还平摊着"
        );
        // 没有手指（点屏 / 按键 / 滚轮）时沿用**写死的对角线**，但那一条也是从上缘起折的。
        let tapped = crease_of(t, true, area).expect("同上");
        assert!(
            tapped.past(area.right, area.top) > 0.0,
            "没有起手点时也该从上缘起折"
        );
        // 而且「手指握着」与「写死路径」不是同一条：握着时折线由手指给。
        assert_ne!(
            tapped, upper,
            "握着手指时折线该由手指算，不是那条写死的路径"
        );
    }

    /// 裁剪与镜像这两步是折面几何的全部家当，直接钉住定义：**被折走的那个角落在它被拖
    /// 到的那一点上**。这一条不是近似 —— 折线既然是对称轴，镜像就必须把角送到那儿；
    /// 送错了就是整块折面与手指脱开，而那是这一档唯一一处「看着不对但没人报错」的错。
    #[test]
    fn the_folded_corner_lands_where_it_was_dragged() {
        let area = page();
        let crease = crease_of(0.0, true, area).expect("起步量得出折线");
        // 起步时被折走的那一片就是右上角那个角，镜像它就是折面。
        let folded = Poly::rect(area).past(crease).reflected(crease);
        assert!(!folded.is_empty(), "起步该折出一个角");
        let tilt = CORNER_TILT * H;
        let mut points = Vec::new();
        let mut landed = false;
        folded.each(|x, y| {
            points.push((x, y));
            if (x - (W - tilt)).abs() < 1e-2 && (y - tilt).abs() < 1e-2 {
                landed = true;
            }
        });
        assert!(
            landed,
            "页的右上角 ({W}, 0) 折过去必须正好落在被拖到的 ({}, {tilt})，实得 {points:?}",
            W - tilt
        );
    }
    /// `Poly` 与 `Shades` 的容量兜底：顶点上界是「每次裁一个多一个」，从矩形出发最多
    /// 折线两次 + 页四条；一块一帧的配额是五块。溢出只在 debug 里断言，所以这两条要在
    /// **正常构建里跑得到** —— 静默丢点的话画面只会少一块影子，谁都看不见。
    #[test]
    fn a_fold_stays_within_its_budget() {
        let area = page();
        let mut worst = 0usize;
        let mut most = 0usize;
        for step in 0..=40 {
            let t = step as f32 / 40.0;
            for forward in [true, false] {
                let shades = Curl.shades(t, forward, area);
                most = most.max(shades.items().len());
                for shade in shades.items() {
                    worst = worst.max(count(&shade.shape));
                }
            }
        }
        assert!(
            worst < POLY_MAX,
            "最坏情况 {worst} 个点，离容量 {} 还有余量才算没偷偷丢点",
            POLY_MAX
        );
        assert!(worst >= 3, "一趟都没裁出多边形说明折面根本没生成");
        assert!(
            (5..=SHADES_MAX).contains(&most),
            "折页那一档最满该在 5..={SHADES_MAX} 之间，实得 {most}"
        );
    }
    /// 卷筒、纸背、投影**全在平摊侧**（`past <= 0`），已经空出来那一侧一块都不许有。
    ///
    /// 空出来那一侧是**进场那张纸**的地盘：那里多画一块影子，读者看到的就是
    /// 「新页上有一道莫名其妙的脸盆大的暗斑」。
    #[test]
    fn nothing_is_painted_on_the_vacated_side() {
        for t in [0.2, 0.4, 0.6, 0.8] {
            let shades = Curl.shades(t, true, page());
            let crease = crease_of(t, true, page()).expect("量得出折线");
            for shade in shades.items() {
                let (_, high) = past_span(crease, &shade.shape);
                assert!(
                    high <= 1e-3,
                    "t = {t} 时有一块画到了已经空出来那一侧：past 上界 {high}"
                );
            }
        }
    }
}
