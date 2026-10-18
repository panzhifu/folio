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
/// 折页那一档全用它：
/// - `from` 决定**从哪个角起折**（下半屏按下的就从右下角卷起）；
/// - `at` 决定**折线此刻在哪** —— 折线是「那个角」与「手指此刻的位置」的垂直平分线，
///   所以纸是真的跟着手指走，不是沿一条写死的对角线扫过去。
///
/// 松手之后 `at` 由绘制层补间到落点（见 `view::body`），折线跟着走完，不会在松手那一帧跳。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::ui::features::reader) struct Grip {
    pub from: (f32, f32),
    pub at: (f32, f32),
}

impl Grip {
    pub(in crate::ui::features::reader) fn new(from: (f32, f32), at: (f32, f32)) -> Self {
        Self { from, at }
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
/// 构造照 StPageFlip：手上捏的是页的那个**角**，折线就是「角的原位置」与「它被拖到的位置」
/// 这两点连线的垂直平分线 —— 纸绕这条线翻过去，翻出去的那一片正好落在线另一侧的镜像位置上。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::ui::features::reader) struct Crease {
    /// 单位法向，指向**已经被折走**那一侧。
    pub(in crate::ui::features::reader) nx: f32,
    pub(in crate::ui::features::reader) ny: f32,
    /// `nx·x + ny·y` 在折线上的取值。
    pub(in crate::ui::features::reader) d: f32,
}

impl Crease {
    /// 由「被折走那一角的原位置」与「它被拖到哪」作垂直平分线。两点重合时折线不存在 ⇒ `None`。
    pub(in crate::ui::features::reader) fn from_corner(
        corner: (f32, f32),
        dragged: (f32, f32),
    ) -> Option<Self> {
        let dx = corner.0 - dragged.0;
        let dy = corner.1 - dragged.1;
        let long = (dx * dx + dy * dy).sqrt();
        if long < 1e-3 {
            return None;
        }
        let (nx, ny) = (dx / long, dy / long);
        let middle = ((corner.0 + dragged.0) * 0.5, (corner.1 + dragged.1) * 0.5);
        Some(Self {
            nx,
            ny,
            d: nx * middle.0 + ny * middle.1,
        })
    }

    /// 这一点离折线多远，带符号：正 = 这一片纸已经折走了，负 = 还平摊着。
    pub(in crate::ui::features::reader) fn past(self, x: f32, y: f32) -> f32 {
        self.nx * x + self.ny * y - self.d
    }

    /// 法向在 gpui 渐变口径下的角（度，`linear_gradient` 那个「0 = 朝上、顺时针」）：
    /// 阴影带要从折线往外淡出，渐变方向就取这个。
    pub(in crate::ui::features::reader) fn angle(self) -> f32 {
        90.0 + self.ny.atan2(self.nx).to_degrees()
    }

    /// 上一条的反号：折面那道阴影在折线的**平摊侧**，淡出方向正好相反。
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
    ///
    /// 判空**必须**用它而不是顶点数：落位那一刻折面镜像到页外面去，裁回来剩下贴着页
    /// 左缘的一条退化四边形 —— 四个点、零面积。按顶点数判会留下一个宽零像素却真的入树
    /// 的路径，而「纸已经走光了画面还挂着一张」正是这一档最不该出现的读数。
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
    pub(in crate::ui::features::reader) fn past(self, crease: Crease) -> Self {
        self.clip(|x, y| crease.past(x, y))
    }

    /// 裁回页的那块矩形：折过去的那一片不能伸到屏幕外面去。
    pub(in crate::ui::features::reader) fn inside(self, area: Area) -> Self {
        self.clip(|x, _| x - area.left)
            .clip(|_, y| y - area.top)
            .clip(|x, _| area.right - x)
            .clip(|_, y| area.bottom - y)
    }

    /// 折线两侧之间的那一条平行带（`from <= past <= to`）。
    pub(in crate::ui::features::reader) fn slab(self, crease: Crease, from: f32, to: f32) -> Self {
        self.clip(|x, y| crease.past(x, y) - from)
            .clip(|x, y| to - crease.past(x, y))
    }

    /// 绕折线做镜像 —— 「被折走那一片」翻过去之后落在哪，就是这一步。
    pub(in crate::ui::features::reader) fn reflected(self, crease: Crease) -> Self {
        let mut out = Self::empty();
        for index in 0..self.len {
            let (x, y) = self.pts[index];
            let step = 2.0 * crease.past(x, y);
            out.push((x - step * crease.nx, y - step * crease.ny));
        }
        out
    }
}

/// 一块要盖在两张纸之上的形状。几何与浓淡全在这里算好，绘制层只做「填」。
#[derive(Clone, Copy, Debug)]
pub(in crate::ui::features::reader) struct Shade {
    pub shape: Poly,
    pub tint: Tint,
    /// 渐变方向（gpui `linear_gradient` 那一套角）。`None` = 纯色填。
    ///
    /// 为什么只有两挡颜色：gpui 的 `Background` 装的是**两个**色标（color.rs 里
    /// `colors: [LinearColorStop; 2]`），而 StPageFlip 折谷那道阴影是四个色标
    /// （暗 5% → 0.05 → 暗 35% → 0）。所以那道阴影在这里被拆成三条**互不重叠**的带
    /// 叠出来 —— 见 [`fold_shades`]，那三条合起来才是「暗—亮芯—暗—淡出」。
    pub angle: Option<f32>,
    /// 峰值不透明度。`0` = 这一条不必画（宽度或墨色已经归零）。
    pub ink: f32,
}

/// 一块形状用哪种底色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui::features::reader) enum Tint {
    /// 页色：折起来那一张的**背面**。只有它是不透明的。
    Paper,
    /// 黑：压暗。投影、接触影、折谷都用它。
    Dark,
    /// 白：提亮。纸的前缘、折谷里那道亮芯用它。
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
    use crate::ui::features::reader::motion::turn_animation::geometry::Cut;
    use crate::ui::features::reader::motion::turn_animation::testing::{W, page};

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
