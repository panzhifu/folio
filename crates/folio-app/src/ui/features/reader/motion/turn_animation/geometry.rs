//! 摆位层的几何：一张纸占的那块地方、折线、裁剪、多边形与影子配额。
//!
//! 这一份里没有一个式子是「某一档」的：全部档位共用，改它等于改所有档。某一档自己的常数
//! 与式子在各自的文件里。

/// 那一张纸沿哪根轴走。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui::features::reader) enum Axis {
    Horizontal,
    Vertical,
}

/// 读者这一笔的手：**从哪儿起**、**此刻在哪儿**（两个点都在版心内归一化：0..1）。
///
/// 折页那一档全用它，而且用的是这两点**之差**：
///
/// - 划动向量决定折线的**朝向**（往哪划就往哪折）和轴往里推进**多少**；
/// - `from` 因此就是被捏住的那一点 —— 不是页的某个固定角。从前取固定角（按下点在上半屏就
///   揭右上角、下半屏揭右下角），划动方向根本不进式子，读出来是「按在页面中间，揭起来的是
///   右下角」。
///
/// 位置本身由档位自己算：折页把 `from` 投影到页的自由边当**锚点**，轴从锚点起步（见
/// `curl::grabbed_edge`），不是停在两点中间。
///
/// `at` 允许走到 0..1 之外：那一格折线要越过书脊那一侧的页边，整页才算折完。走完那一腿由
/// `curl::release_grip` 沿同一方向把这一笔拉长到落点，所以纸不会在松手那一帧跳；弹回那一腿
/// **不借手**，读的是 [`Pose::revert`] 那一个数。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::ui::features::reader) struct Grip {
    pub from: (f32, f32),
    pub at: (f32, f32),
}

impl Grip {
    pub(in crate::ui::features::reader) fn new(from: (f32, f32), at: (f32, f32)) -> Self {
        Self { from, at }
    }

    /// 把一手从**整屏**归一化坐标映进一张 leaf 的 0..1。
    ///
    /// 只映横轴：双页的 leaf 是左右半张，纵向与整页同一把尺。`from` 是手势归一化用的那块
    /// 版心，`to` 是这一趟真正被揭起来的那半张 —— 两处同一把尺，折线才会从手指那一格起卷。
    pub(in crate::ui::features::reader) fn remap(self, from: Area, to: Area) -> Self {
        let x = |value: f32| ((value * from.width() + from.left) - to.left) / to.width().max(1.0);
        Self {
            from: (x(self.from.0), self.from.1),
            at: (x(self.at.0), self.at.1),
        }
    }
}

/// 摆位层每一帧读的那一组：往哪边翻、这一本从哪边读、读者这一笔的手。
///
/// 只有带手的两条入口（[`super::PageTurnStyle::sheets_at`] / [`super::PageTurnStyle::shades_at`]）
/// 用它：平移/覆盖那几档整张纸沿轴走，起点在哪儿、书从哪边读都不改变它们的几何，所以默认的
/// [`super::PageTurnStyle::sheets`] 仍然只吃 `forward`。
#[derive(Clone, Copy, Debug)]
pub(in crate::ui::features::reader) struct Pose {
    /// `true` = 沿读书顺序往后（下一页）。
    pub forward: bool,
    /// `true` = 这一本从右往左读。
    pub rtl: bool,
    /// 读者这一笔的手；`None` = 不是手指起的（点屏 / 按键 / 滚轮）。
    pub grip: Option<Grip>,
    /// 弹回那一腿自己走了几成（[`super::release_clock`] 给的那一个数）。`Some` = 这一趟正在往
    /// 回退、手指已经不在上了，那一腿**不借手**：折线保持松手那一刻的朝向与卷径，只沿自己的法向
    /// 一路退到「一个角都没折走」之外。
    ///
    /// 为什么不能照走完那一腿的写法把手指原路缩回按下点：那条律的推进量在 `dist` 缩向 0 的一段里
    /// 由 `R·sin(π·√(dist/curlLen))` 主导（√ 在 0 处斜率无穷大，收尾比手指快得多），而它的终点是
    /// 「轴停在锚点」—— 锚点在**自由边上**，对一条斜的折线来说那还差着一截，最后一帧只能由「按下
    /// 没动」那条早退硬摆成一条竖线。2026-10-22 实测：按下点在上半屏、斜划到下缘那一笔，按
    /// `1 − u` 缩回时最坏的一帧走掉 366 逻辑像素（17 帧那一档是平均速度的 12.3 倍）。
    ///
    /// 只有折页那一档读它：别的档的纸是整张平移/覆盖的，退回去不需要任何几何。
    pub revert: Option<f32>,
}

impl Pose {
    /// 纸的**自由边**在哪一侧（`true` = 右）。
    ///
    /// 折页那一档只认这一个数：它那套律只有一套坐标（自由边在右、书脊在左、手指往左划），
    /// 进出口各映一次横向。所以「往回翻」与「从右往左读的那一本」在这里是**各反一次**而不是
    /// 照抄 `forward` —— 后翻揭的是左边那一窄条，RTL 那一本里连往后翻都换边。
    ///
    /// 写成两个数而不是一条 `self` 的方法：补间手指落点那一步（`release_grip`）只要这一个判据，
    /// 手还没算出来。竖着走的档位不读它（手势那头的 `swipe::toward` 对竖轴不看读书方向）。
    pub(in crate::ui::features::reader) fn free_edge_right(forward: bool, rtl: bool) -> bool {
        forward != rtl
    }
}

/// 一张纸占的那块矩形，**元素坐标**（与字形的 `run.x` / `run.baseline` 同一把尺）。
///
/// 折线、裁剪、折面多边形全都算在这一套坐标里，画的那一头就一个偏移都不必再加：原先
/// 阴影带按 `margin + seam.at` 摆、裁剪按 `seam.at` 判，两处差着一个页边距。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::ui::features::reader) struct Area {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Area {
    pub(in crate::ui::features::reader) fn new(
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    ) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub(in crate::ui::features::reader) fn width(self) -> f32 {
        (self.right - self.left).max(0.0)
    }

    pub(in crate::ui::features::reader) fn height(self) -> f32 {
        (self.bottom - self.top).max(0.0)
    }

    /// 双页里靠右/靠左的那半张纸。书脊落在整屏中线；`right = true` 时自由边在右，
    /// 那一趟揭起来的是右半张。
    pub(in crate::ui::features::reader) fn leaf(self, right: bool) -> Self {
        let middle = self.left + self.width() * 0.5;
        if right {
            Self::new(middle, self.top, self.right, self.bottom)
        } else {
            Self::new(self.left, self.top, middle, self.bottom)
        }
    }

    /// 按轴取那一屏的距离。取错那个数不会报错，只会让纸走半屏就落定。
    pub(in crate::ui::features::reader) fn distance(self, axis: Axis) -> f32 {
        match axis {
            Axis::Horizontal => self.width(),
            Axis::Vertical => self.height(),
        }
    }
}

/// 折线（一条有向直线）。
///
/// 谁给出它谁定这条线的读法：折页那一档给的是「绕着它卷起来」的那根轴（见
/// `curl::cylinder_at`），覆盖那一档给的是移动那张纸的前缘。这里的式子只认三个数：纸沿法向
/// 那一侧已经离场，另一侧还平摊着。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::ui::features::reader) struct Crease {
    /// 单位法向，指向**已经被折走**那一侧。
    pub(in crate::ui::features::reader) nx: f32,
    pub(in crate::ui::features::reader) ny: f32,
    /// `nx·x + ny·y` 在折线上的取值。
    pub(in crate::ui::features::reader) d: f32,
}

impl Crease {
    /// 这一点离折线多远，带符号：正 = 这一片纸已经折走了，负 = 还平摊着。
    pub(in crate::ui::features::reader) fn past(self, x: f32, y: f32) -> f32 {
        self.nx * x + self.ny * y - self.d
    }

    /// 法向在 gpui 渐变口径下的角（度，`linear_gradient` 那个「0 = 朝上、顺时针」）：
    /// 阴影带要从折线往外淡出，渐变方向就取这个。
    pub(in crate::ui::features::reader) fn angle(self) -> f32 {
        90.0 + self.ny.atan2(self.nx).to_degrees()
    }

    /// 上一条的反号：移动那张纸投出去的影在它**外面**那一侧，淡出方向正好相反。
    pub(in crate::ui::features::reader) fn angle_back(self) -> f32 {
        90.0 + (-self.ny).atan2(-self.nx).to_degrees()
    }
}

/// 一张纸占的**那一块**（一条边界切出的半平面）。
///
/// 折页用的是折线那一侧；覆盖用的是移动那张纸的前缘。半平面本来就是凸的，所以逐点判一句
/// 就够，不需要真的裁出多边形。
///
/// 为什么覆盖也要裁：那一档里两张纸是**真的重叠**的，而它们的字形烘在同一个元素里、
/// 谁也没有自己的底色去挡谁（页底挂在父节点上，两张纸共用同一层）。不裁的结果就是压住
/// 那一段两页的字同时在那儿 —— 读起来不是「一张盖住一张」，是「字叠在字上」。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::ui::features::reader) struct Cut {
    pub(in crate::ui::features::reader) crease: Crease,
    /// `true` = 只画这条边界 `past > 0` 那一侧（压在上面那一张）；`false` = 只画另一侧。
    pub(in crate::ui::features::reader) keeps_inside: bool,
}

impl Cut {
    /// 一条边界：`keeps_inside` 取「`past > 0` 那一侧」，否则取反。
    pub(in crate::ui::features::reader) fn new(crease: Crease, keeps_inside: bool) -> Self {
        Self {
            crease,
            keeps_inside,
        }
    }

    /// 这一点落在这一侧吗。两侧用**严格大于**分开：正好压在边界上的那个字只归一张纸画，
    /// 否则它会同时在两页上出现一遍，那一帧字色深一档。
    pub(in crate::ui::features::reader) fn keeps(self, x: f32, y: f32) -> bool {
        (self.crease.past(x, y) > 0.0) == self.keeps_inside
    }
}

/// 一次翻页里一张纸这一帧的摆位。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::ui::features::reader) struct Sheet {
    /// 窗口坐标下的位移。进场那一张的数就是 `ReaderSession::page_shift` —— 命中换算要
    /// 减掉同一个数，否则补间走完之前按在某个字上的手指算到隔壁那行去。
    pub offset: (f32, f32),
    /// 入树顺序：大的后画、压在上面。`Slide` 两张始终不重叠，层级无所谓；`Cover` 靠它
    /// 把新页抬到停着的旧页之上，折页靠它把**旧页**抬到新页之上。
    pub z: u8,
    /// 这张纸占哪一块（一到两条半平面，见 [`Cut`]）。`None` = 整张都画 —— 只有**不重叠**
    /// 的那些档可以没有：两张纸的字形共用一个元素、谁也没有自己的底色，一旦重叠又不裁，
    /// 画面上就是两页的字叠在一起。
    pub cut: Option<Cut>,
}

/// 凸多边形，容量写死：一趟翻页最多裁出十来个点，一帧一次的堆分配不值得。
///
/// 只做**凸**的裁剪（Sutherland–Hodgman 逐边），因为矩形被直线裁、被直线裁的图形做镜像，
/// 结果一定还是凸的。溢出只在 debug 构建里断言 —— 这一条长在绘制路径上，发行版不该 panic。
#[derive(Clone, Copy, Debug)]
pub(in crate::ui::features::reader) struct Poly {
    pub(in crate::ui::features::reader) pts: [(f32, f32); POLY_MAX],
    len: usize,
}

pub(in crate::ui::features::reader) const POLY_MAX: usize = 16;

impl PartialEq for Poly {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.pts[..self.len] == other.pts[..other.len]
    }
}

impl Poly {
    /// 空的多边形（顶点不够三个就是一张纸都没裁出来）。
    const EMPTY: Poly = Poly {
        pts: [(0.0, 0.0); POLY_MAX],
        len: 0,
    };

    pub(in crate::ui::features::reader) fn empty() -> Self {
        Self::EMPTY
    }

    pub(in crate::ui::features::reader) fn rect(area: Area) -> Self {
        let mut out = Self::empty();
        out.push((area.left, area.top));
        out.push((area.right, area.top));
        out.push((area.right, area.bottom));
        out.push((area.left, area.bottom));
        out
    }

    pub(in crate::ui::features::reader) fn is_empty(self) -> bool {
        self.len < 3
    }

    /// 多边形围住的面积（绝对值）。
    #[cfg(test)]
    ///
    /// 判空**必须**用它而不是顶点数：落位那一刻折面镜像到页外面去，裁回来剩下贴着页
    /// 左缘的一条退化四边形 —— 四个点、零面积。按顶点数判会留下一个宽零像素却真的入树
    /// 的路径，而「纸已经走光了画面还挂着一张」正是这一档最不该出现的读数。
    #[cfg(test)]
    pub(in crate::ui::features::reader) fn area(self) -> f32 {
        let mut sum = 0.0f32;
        if self.len < 3 {
            return 0.0;
        }
        for index in 0..self.len {
            let (ax, ay) = self.pts[index];
            let (bx, by) = self.pts[(index + 1) % self.len];
            sum += ax * by - bx * ay;
        }
        sum * 0.5
    }

    /// 逐点给回回调。绘制层用它拼路径，测试用它验每个点都在该在的那一侧。
    pub(in crate::ui::features::reader) fn each(&self, mut at: impl FnMut(f32, f32)) {
        for index in 0..self.len {
            let (x, y) = self.pts[index];
            at(x, y);
        }
    }

    pub(in crate::ui::features::reader) fn push(&mut self, point: (f32, f32)) {
        debug_assert!(self.len < POLY_MAX, "多边形顶点溢出：折纸的裁剪超出了容量");
        if self.len < POLY_MAX {
            self.pts[self.len] = point;
            self.len += 1;
        }
    }

    /// 只留下 `signed >= 0` 那一侧。
    pub(in crate::ui::features::reader) fn clip(
        &self,
        mut signed: impl FnMut(f32, f32) -> f32,
    ) -> Self {
        let mut out = Self::empty();
        if self.len < 3 {
            return out;
        }
        for index in 0..self.len {
            let a = self.pts[index];
            let b = self.pts[(index + 1) % self.len];
            let (sa, sb) = (signed(a.0, a.1), signed(b.0, b.1));
            if sa >= 0.0 {
                out.push(a);
            }
            // 异号才跨过边界；同号时 `sa - sb` 可能为 0，所以只在异号那一支除。
            if (sa >= 0.0) != (sb >= 0.0) {
                let k = sa / (sa - sb);
                out.push((a.0 + k * (b.0 - a.0), a.1 + k * (b.1 - a.1)));
            }
        }
        out
    }

    /// 只留已经被折走那一侧（`past >= 0`）。
    #[cfg(test)]
    pub(in crate::ui::features::reader) fn past(self, crease: Crease) -> Self {
        self.clip(|x, y| crease.past(x, y))
    }

    /// 折线两侧之间的那一条平行带（`from <= past <= to`）。
    pub(in crate::ui::features::reader) fn slab(self, crease: Crease, from: f32, to: f32) -> Self {
        self.clip(|x, y| crease.past(x, y) - from)
            .clip(|x, y| to - crease.past(x, y))
    }
}

/// 一块要盖在两张纸之上的形状。几何与浓淡全在这里算好，绘制层只做「填」。
///
/// 只有**不平铺在 GPU 网格上**的那些形状走这一条：覆盖那一档的前缘投影与纸边光。折页那一张
/// 的受光（暗—亮芯—暗）在顶点着色器里按柱面算法线算，不占这一层的配额 —— 一条两段渐变糊出
/// 不来一摞带子，而 gpui 的 `Background` 本来就只装**两个**色标（color.rs 里
/// `colors: [LinearColorStop; 2]`）。
#[derive(Clone, Copy, Debug)]
pub(in crate::ui::features::reader) struct Shade {
    pub shape: Poly,
    pub tint: Tint,
    /// 渐变方向（gpui `linear_gradient` 那一套角）。`None` = 纯色填。
    pub angle: Option<f32>,
    /// 峰值不透明度。`0` = 这一条不必画（宽度或墨色已经归零）。
    pub ink: f32,
}

/// 一块形状用哪种底色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui::features::reader) enum Tint {
    /// 页色：不透明的纸底。目前没有哪一块形状用它 —— 折页那张纸的正面背面都改由 GPU 网格
    /// 采捕获纹理来画，CPU 这一层只剩半透明的带子。它是 `BLANK_SHADE` 那一格的填充值。
    Paper,
    /// 黑：压暗。投影与纸边光外侧那道接触影用它。
    Dark,
    /// 白：提亮。移动那张纸自己的前缘用它。
    Light,
}

/// 一趟翻页里所有要盖上去的形状。
#[derive(Clone, Copy, Debug)]
pub(in crate::ui::features::reader) struct Shades {
    pub(in crate::ui::features::reader) items: [Shade; SHADES_MAX],
    len: usize,
}

/// 最多同时挂几块：折页那一档最满（纸背 + 卷筒纸面 + 十几条受光带 + 高光 + 投影），
/// 覆盖两到三块。留几格余量，加一层不必动这个数。
pub(in crate::ui::features::reader) const SHADES_MAX: usize = 20;

impl Shades {
    pub(in crate::ui::features::reader) fn new() -> Self {
        Self {
            items: [BLANK_SHADE; SHADES_MAX],
            len: 0,
        }
    }

    pub(in crate::ui::features::reader) fn push(&mut self, shade: Shade) {
        debug_assert!(self.len < SHADES_MAX, "盖上去的形状超过了一帧的配额");
        if self.len < SHADES_MAX {
            self.items[self.len] = shade;
            self.len += 1;
        }
    }

    /// 绘制层按顺序填：先入树的在下面。
    pub(in crate::ui::features::reader) fn items(&self) -> &[Shade] {
        &self.items[..self.len]
    }
}

pub(in crate::ui::features::reader) const BLANK_SHADE: Shade = Shade {
    shape: Poly::EMPTY,
    tint: Tint::Paper,
    angle: None,
    ink: 0.0,
};

/// 覆盖那一档：移动那张纸的**前缘**所在的那条竖线。
///
/// 法向 `n` 指向纸走进来的那一侧（也就是这张纸自己占的那一侧），所以「前缘之前」=
/// `past < 0` = 下面那张还露着的地方，投影就裁在那一侧。
pub(in crate::ui::features::reader) fn travel_edge(
    progress: f32,
    forward: bool,
    area: Area,
) -> Crease {
    let t = progress.clamp(0.0, 1.0);
    // 往后翻时新页从阅读方向的前方盖进来，前缘是它的左缘（从 `页宽` 走到 `0`）；
    // 往前翻时是它的右缘（从 `0` 走到 `页宽`）。
    let (nx, at) = if forward {
        (1.0, area.width() * (1.0 - t))
    } else {
        (-1.0, area.width() * t)
    };
    Crease {
        nx,
        ny: 0.0,
        d: nx * at,
    }
}

/// 移动那张纸自己的前缘那道纸边光有多宽（像素）。
pub(in crate::ui::features::reader) const EDGE_WIDE: f32 = 2.0;
/// 纸边光有多亮。
pub(in crate::ui::features::reader) const EDGE_INK: f32 = 0.14;

/// 一张纸沿自己某条边投出去的一条带 + 那条边上的一道纸边光。
///
/// `n` 指向纸那一侧 ⇒ 投影落在**纸外面**（`past < 0`），纸边光落在**纸里面**
/// （`past > 0`）：前者读起来是「这张压着下面那张」，后者读起来是「它有自己的厚度」。
pub(in crate::ui::features::reader) fn edge_pair(
    page: Poly,
    crease: Crease,
    wide: f32,
    ink: f32,
    out: &mut Shades,
) {
    out.push(Shade {
        shape: page.slab(crease, -wide, 0.0),
        tint: Tint::Dark,
        angle: Some(crease.angle_back()),
        ink,
    });
    out.push(Shade {
        shape: page.slab(crease, 0.0, EDGE_WIDE),
        tint: Tint::Light,
        angle: Some(crease.angle()),
        ink: EDGE_INK,
    });
}

#[cfg(test)]
mod tests {
    //! 搬自摆位层那一份：一挡的门跟着那一档走，共用的尺在 `testing.rs`。

    use crate::ui::features::reader::motion::turn_animation::curl::crease_of;
    use crate::ui::features::reader::motion::turn_animation::geometry::{Area, Cut, Grip};
    use crate::ui::features::reader::motion::turn_animation::testing::{H, W, page};

    /// 双页的 leaf 是整屏的一半；手势从整屏归一化坐标映进 leaf 之后，右缘 = 1、中缝 = 0
    /// （左 leaf 反过来）。映错一格，折痕就从手指旁边起卷。
    #[test]
    fn a_leaf_is_half_the_page_and_grips_remap_into_it() {
        let area = Area::new(0.0, 0.0, W, H);
        let right = area.leaf(true);
        let left = area.leaf(false);
        assert_eq!(right.width(), W * 0.5);
        assert_eq!(left.width(), W * 0.5);
        assert_eq!(right.left, W * 0.5);
        assert_eq!(left.right, W * 0.5);
        assert_eq!(right.top, area.top);
        assert_eq!(right.height(), H);

        let from = (W * 0.75, H * 0.5);
        let grip = Grip::new((from.0 / W, from.1 / H), (W * 0.99 / W, from.1 / H));
        let mapped = grip.remap(area, right);
        assert!(
            (mapped.from.0 - 0.5).abs() < 1e-5,
            "整屏 75% 该是右 leaf 的 50%"
        );
        assert!((mapped.from.1 - 0.5).abs() < 1e-5, "纵向不该被映");
        assert!(
            (mapped.at.0 - 0.98).abs() < 1e-5,
            "整屏 99% 该是右 leaf 的 98%"
        );

        let mapped_left = Grip::new((0.25, 0.5), (0.01, 0.5)).remap(area, left);
        assert!((mapped_left.from.0 - 0.5).abs() < 1e-5);
        assert!((mapped_left.at.0 - 0.02).abs() < 1e-5);
    }

    /// 裁剪两侧用的是**同一个** `Cut` 判据（`paint.rs` 逐字剔那一步读的式子），
    /// 而且按点给的是严格不等号：正翻到「页的中线」与反翻到同一位置必须判得一样。
    #[test]
    fn the_cut_is_a_signed_distance_test() {
        let area = page();
        let crease = crease_of(0.5, true, area).expect("量得出");
        let keep_past = Cut::new(crease, true);
        assert!(!keep_past.keeps(0.0, 300.0), "平摊侧不该被留进空侧那一张");
        assert!(keep_past.keeps(W, 300.0), "空侧该留给进场那一张");
        // 正好落在折线上的那一点：两侧的判据必须**恰好相反**（哪一个浮点余量都不能让
        // 两张纸同时画它，或同时不画）。
        let y = 300.0;
        let on = (crease.d - crease.ny * y) / crease.nx;
        assert!(
            crease.past(on, y).abs() < 1e-3,
            "取点失败：该点在折线上，实得 past = {}",
            crease.past(on, y)
        );
        let other = Cut::new(crease, false);
        assert_ne!(
            keep_past.keeps(on, y),
            other.keeps(on, y),
            "压在折线上的那个字被画了两遍，或者一遍都没画"
        );
        assert!(crease.angle().is_finite(), "渐变角要能用");
        assert!(crease.angle_back().is_finite());
    }
}
