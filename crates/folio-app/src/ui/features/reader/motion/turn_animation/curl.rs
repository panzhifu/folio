//! 仿真翻页：一张纸被**折走**。
//!
//! 纸模型是「沿一条直线折」，不是「卷成圆柱」：纸当一张不会弯的硬片，绕折线做刚体翻转，
//! 全部仿真感来自折线的倾角加沿折线的几条渐变阴影。

/// 起折那一角的偏移，按页高的比例。照 StPageFlip 点屏那一档：`topMargins = height/10`。
///
/// 这一格是**定值**而不是手指位置：一趟翻页的进度只有「走了多少」一个数（`TurnFrame`），
/// 把手指的纵向坐标穿过决策层要动会话、跟手、点屏三条路，而那三处都有各自的钉着的判据。
/// 更要紧的是这条路径自己会把倾角收掉：角从 `(页宽 − 0.1 页高, 0.1 页高)` 走到
/// `(−页宽, 0)`，折线的倾角随行程单调变直，所以一个常数就给了「起步斜、落位正」。
use super::{Area, Axis, Crease, Cut, PageTurnStyle, Poly, Shade, Shades, Sheet, Tint};
use crate::ui::features::reader::model::chrome::PageMode;

pub(in crate::ui::features::reader) const CORNER_TILT: f32 = 0.1;

/// 折页的阴影最宽能到页宽的几成（那是走完那一刻的宽度，此时墨色已经淡到 0）。
pub(in crate::ui::features::reader) const SHADOW_WIDE: f32 = 0.34;

/// 起步那一刻折页的阴影有多黑（按 `1 − 走了多少` 线性淡出）。
pub(in crate::ui::features::reader) const SHADOW_INK: f32 = 0.34;

/// 折谷三段里第一段（贴折线那道接触影）占阴影宽的几成。
const INNER_DARK: f32 = 0.18;

/// 亮芯占阴影宽的几成 —— 到 `INNER_DARK + LIGHT_CORE` 为止都是那道亮芯。
const LIGHT_CORE: f32 = 0.17;

/// 亮芯有多亮。压在一整条阴影中间，读数要的是「纸在折谷那儿翻过去的一道棱」，
/// 给过了就成一条白棍 —— 那正是旧版 `CURL_EDGE` 的毛病。
const LIGHT_INK: f32 = 0.10;

/// 折谷外面那第三道暗带的黑度（相对阴影本身的黑度）。
const OUTER_TAIL: f32 = 0.55;

/// 走了 `progress` 那一刻的折线。往回翻是同一件事沿页的中线镜像。
///
/// `None` = 这块版心还没量出面积，或角与它的原位置重合到折线无从定义。
pub(in crate::ui::features::reader) fn crease_of(progress: f32, forward: bool, area: Area) -> Option<Crease> {
    if area.width() <= 0.0 || area.height() <= 0.0 {
        return None;
    }
    let t = progress.clamp(0.0, 1.0);
    let tilt = CORNER_TILT * area.height();
    // 规范框：一律从 `right` 那一条边起折，角贴着上缘。
    let corner = (area.right, area.top);
    let start = (area.right - tilt, area.top + tilt);
    let dest = (area.right - 2.0 * area.width(), area.top);
    let dragged = (
        start.0 + (dest.0 - start.0) * t,
        start.1 + (dest.1 - start.1) * t,
    );
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

/// 折页那一档这一帧要盖上去的那几块。
pub(in crate::ui::features::reader) fn fold_shades(progress: f32, forward: bool, area: Area) -> Shades {
    let mut out = Shades::new();
    let Some(crease) = crease_of(progress, forward, area) else {
        return out;
    };
    let page = Poly::rect(area);
    // 被折走的那一片先裁出来，再绕折线镜像 —— 折面就是它的镜像。
    let cut = page.past(crease);
    if cut.is_empty() {
        return out;
    }
    let paper = cut.reflected(crease).inside(area);
    if paper.is_empty() || paper.area().abs() < 1.0 {
        return out;
    }
    let t = progress.clamp(0.0, 1.0);
    // 阴影律照抄 StPageFlip，而不是原先那条 `α − sin α` 反解：那条在整个行程里几乎
    // 不塌（走过一半还留着 94% 的宽度），画出来就是一条白带横扫整屏。这一条是**从折
    // 痕里长出来的**：起步没有折起 ⇒ 带宽为 0，落位纸已经走光 ⇒ 墨色为 0。
    let wide = SHADOW_WIDE * area.width() * t;
    let ink = SHADOW_INK * (1.0 - t);
    out.push(Shade {
        shape: paper,
        tint: Tint::Paper,
        angle: None,
        ink: 1.0,
    });
    if wide <= 0.5 {
        return out;
    }
    // 投在**露出来那一张**上的那道：在折线的空侧，从折线往外淡出。
    out.push(Shade {
        shape: page.slab(crease, 0.0, wide),
        tint: Tint::Dark,
        angle: Some(crease.angle()),
        ink,
    });
    // 折谷那三段（都在平摊侧、都裁进折面，所以不会糊到没被折走的那些字上去）：
    // 贴折线的接触影 → 一道亮芯 → 外面一条更淡的、淡出去的暗带。
    out.push(Shade {
        shape: paper.slab(crease, -INNER_DARK * wide, 0.0),
        tint: Tint::Dark,
        angle: Some(crease.angle_back()),
        ink,
    });
    out.push(Shade {
        shape: paper.slab(
            crease,
            -(INNER_DARK + LIGHT_CORE) * wide,
            -INNER_DARK * wide,
        ),
        tint: Tint::Light,
        angle: None,
        ink: LIGHT_INK * (1.0 - t),
    });
    out.push(Shade {
        shape: paper.slab(crease, -wide, -(INNER_DARK + LIGHT_CORE) * wide),
        tint: Tint::Dark,
        angle: Some(crease.angle_back()),
        ink: OUTER_TAIL * ink,
    });
    out
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

    fn animates(&self, _: PageMode) -> bool {
        true
    }

    fn sheets(&self, t: f32, forward: bool, area: Area) -> [Sheet; 2] {
        let cut = match crease_of(t, forward, area) {
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
}


#[cfg(test)]
mod tests {
    //! 搬自摆位层那一份：一挡的门跟着那一档走，共用的尺在 `testing.rs`。

    use crate::ui::features::reader::motion::turn_animation::PageTurnStyle as _;
    use crate::ui::features::reader::motion::turn_animation::curl::{CORNER_TILT};
    use crate::ui::features::reader::motion::turn_animation::curl::{Curl};
    use crate::ui::features::reader::motion::turn_animation::curl::{SHADOW_INK};
    use crate::ui::features::reader::motion::turn_animation::curl::{SHADOW_WIDE};
    use crate::ui::features::reader::motion::turn_animation::curl::{crease_of};
    use crate::ui::features::reader::motion::turn_animation::geometry::{Poly, Tint, POLY_MAX, SHADES_MAX};
    use crate::ui::features::reader::motion::turn_animation::testing::{page, count, paper_of, complementary, past_span, W, H};

    /// 起步：折面已经在了（角被折起一小块），但**一道阴影都还没有** —— 折痕刚起，
    /// 带宽按走了多少算，此刻就是 0。落位：整页折走、折面镜像到页外去了 ⇒ 形状全空。
    /// 这两端是「白条从头挂到尾」那个读数被修掉的凭据：先前那一版在 t = 0 就挂着满宽的
    /// 带子，走到一半还有 94% 宽。
    #[test]
    fn a_fold_grows_its_shadows_out_of_the_crease() {
        let start = Curl.shades(0.0, true, page());
        assert_eq!(
            start.items().len(),
            1,
            "起步只该有一张折起来的纸，不该挂着任何阴影"
        );
        assert!(
            paper_of(&start).is_some_and(|paper| !paper.is_empty()),
            "起步那一帧折起的是一整个角"
        );
        assert_eq!(
            Curl.shades(1.0, true, page()).items().len(),
            0,
            "纸已经整张离场，不该还挂着折面或阴影"
        );
        // 走起来之后五块齐上：纸面 + 投影 + 折谷三段。
        assert_eq!(
            Curl.shades(0.5, true, page()).items().len(),
            5,
            "折到一半该是「纸面 + 投影 + 暗—亮—暗」五块"
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
    /// 阴影律：带宽按走了多少**线性长**、墨色按还剩多少**线性淡**。
    /// 两端都要给准数 —— 起步没有折起就该是零宽，落位纸走光了就该是零墨。
    #[test]
    fn the_shadow_grows_out_of_the_crease_and_fades_away() {
        let mut previous_wide = -1.0;
        let mut previous_ink = SHADOW_INK + 1.0;
        // 只走到九分：落位那一刻折面已经整张离场，那一条由上面那条测试单独钉。
        for step in 1..10 {
            let t = step as f32 / 10.0;
            let shades = Curl.shades(t, true, page());
            let crease = crease_of(t, true, page()).expect("量得出折线");
            // 第二块是投在露出来那一张上的那道。
            let cast = &shades.items()[1];
            let wanted = SHADOW_WIDE * W * t;
            let (low, high) = past_span(crease, &cast.shape);
            assert!(
                (high - wanted).abs() < 1e-3,
                "t = {t} 时投影该正好走到 {wanted} 宽，实得外缘 {high}"
            );
            assert!(low.abs() < 1e-3, "t = {t} 时投影该从折线本身起算，实得内缘 {low}");
            assert!(
                (cast.ink - SHADOW_INK * (1.0 - t)).abs() < 1e-6,
                "t = {t} 时墨色不线性"
            );
            assert!(cast.shape.area().abs() > 0.5, "t = {t} 时投影不该是个零面积的壳");
            assert!(wanted > previous_wide, "带宽只能越翻越宽");
            assert!(cast.ink <= previous_ink, "墨色只能越翻越淡");
            previous_wide = wanted;
            previous_ink = cast.ink;
        }
        assert!(
            Curl.shades(1.0, true, page()).items().is_empty(),
            "落位那一刻带宽与墨色同时归零 ⇒ 不该还剩一条挂着的边"
        );
    }
    /// 折谷那道按 StPageFlip 是**四个色标**（暗 → 0.05 → 暗 → 淡出），而 gpui 的
    /// `Background` 只装得下两个 ⇒ 这里拆成三条互不重叠的带叠出来。这条钉三件事：
    /// 顺序（暗—亮—暗，从折线往外）、不重叠（重叠就会把亮芯盖掉）、都在平摊侧。
    #[test]
    fn the_fold_valley_is_dark_then_light_then_dark() {
        let t = 0.6;
        let shades = Curl.shades(t, true, page());
        let crease = crease_of(t, true, page()).expect("量得出折线");
        let items = shades.items();
        assert_eq!(items.len(), 5, "纸面 + 投影 + 折谷三段");
        let valley = &items[2..5];
        assert_eq!(valley[0].tint, Tint::Dark, "贴折线那道是接触影");
        assert_eq!(valley[1].tint, Tint::Light, "中间那道是亮芯");
        assert_eq!(valley[2].tint, Tint::Dark, "外面那道是淡出去的暗带");
        let mut previous_high = 0.0f32;
        for shade in valley {
            let (low, high) = past_span(crease, &shade.shape);
            assert!(
                high <= previous_high + 1e-3,
                "折谷那三段只能一段比一段更往外，实得 high = {high}，上一段到 {previous_high}"
            );
            assert!(
                low < high,
                "这一段是倒着裁的：low = {low}, high = {high}"
            );
            previous_high = low;
        }
        // 亮芯只有一条窄带的量级，压过了就成一条白棍 —— 上一版 `CURL_EDGE` 就是这个毛病。
        assert!(valley[1].ink <= 0.12, "亮芯不该盖过折谷本身，实得 {}", valley[1].ink);
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
        assert_eq!(
            most, SHADES_MAX,
            "折页那一档最满应该是满配额，实得 {most}"
        );
    }
    /// 折谷那三段与投影各占一边：投影整片在**已经空出来**那一侧，折谷三段整片在平摊侧。
    /// 这条是给绘制层的层级承诺 —— 两边要是重叠，被折走的那半页上就会同时出现纸和它的影子。
    #[test]
    fn the_fold_shadows_sit_on_opposite_sides() {
        let t = 0.6;
        let shades = Curl.shades(t, true, page());
        let crease = crease_of(t, true, page()).expect("量得出折线");
        let items = shades.items();
        let (low, high) = past_span(crease, &items[1].shape);
        assert!(low > -1e-3 && high > 1.0, "投影该整片在空侧，实得 [{low}, {high}]");
        for shade in &items[2..5] {
            let (_, high) = past_span(crease, &shade.shape);
            assert!(
                high <= 1e-3,
                "折谷那道越过折线跑到空侧去了：high = {high}"
            );
        }
    }
}
