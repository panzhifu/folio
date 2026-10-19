//! 仿真翻页：一张纸在折痕处**卷起来**。
//!
//! 纸模型是「卷成一段圆筒」，不是「沿一条直线折」。两者在几何上差一个自由度 —— 弯曲半径
//! —— 而观感的差别全在这一格上：硬折的纸是一条直边扫过屏幕，卷起来的纸在折痕旁边有一段
//! **受光的圆筒**，于是纸有了厚度。
//!
//! 折线怎么定，一条律管三件事（[`curl_of`]）：**方向与走量来自这一笔的划动**（按下点 → 手指
//! 此刻），**轴从页边锚点起步**往里推进，**卷径起手是页宽的 [`ROLL`] 那一成、逼近书脊线性收到
//! 0**。于是按在哪儿就从哪儿那一格高度起卷、往哪划就往哪折、起步只揭一条边而不是半页。倾角再
//! 夹一道上限（[`MAX_TILT`]）。
//!
//! 这一律**以手指为时钟**，所以只在跟手时成话。点屏 / 按键 / 滚轮那一趟没有手指，走 [`tap_roll`]
//! 那一条：折线自己从**右下角**扫到书脊之外一个卷径，卷径全程不收。把前一条律拿去做那一趟的
//! 补间，读出来是「首帧跨 8.8% 页宽、第 11 帧一帧走掉页宽三成七、最后 5 帧钉死」—— 2026-10-19
//! 在 Meizu 20 上按应用自己那行 `folio_app::perf` 读数量到的，两处根因都写在那一格的注释里。
//!
//! 律本身**只有一套坐标**：自由边在右、书脊在左、手指往左划。别的情形不另写一条律，进出各映
//! 一次横向（[`cylinder_at`] 配 [`flip_x`]），映不映由「自由边到底在哪一侧」判（
//! [`Pose::free_edge_right`] = `forward != rtl`：往回翻与「从右往左读的那一本」各反一次）。
//! 少映进那一次，往右拖那一笔会被「往翻页反方向走的那一截不算」夹成 0，整趟停在「还没起步」。
//!
//! 整条律照抄 `harism/android-pagecurl` 的 `CurlView.updateCurlPos`（`fi.harism.curl`，安卓上
//! 这一派的事实标准，国产阅读器的「仿真」多数是它的后代）。中间试过另两家，都不对：
//!
//! - **StPageFlip**（`reference/StPageFlip`）：折线是「页的固定右角 ↔ 手指绝对位置」的垂直
//!   平分线，划动方向根本不进式子。按在页面中间往左划一下，揭起来的是右下角 —— 读者报的
//!   「不跟手」就是它。
//! - **把按下点当锚点的垂直平分线**：方向对了，但折线永远落在两点正中 —— 往左拖 500 像素，
//!   折线立刻落到页面正中，半页字一瞬间被一整张空白纸背盖住。真机截图量到左半均值 226、
//!   方差 15、一个字都没有。这一版只活了半天。
//!
//! 卷径也不照 XBPageCurl 的 `16 + l/8`（那是 iPad 尺寸上的点，照搬到 390dp 会涨到屏宽六成，
//! 读起来是一捆纸），也不照 gmpagecurl 那个死常数 —— 起手 [`ROLL`] 页宽，随轴逼近书脊收到 0。
//!
//! 卷过去那一段是真画出来的：`folio_curl::build_page_mesh` 给一张页面网格，顶点按「平摊 /
//! 卷在半径 R 的柱面上 / 绕过柱顶落在背面」三段变形（`gpui-pre-wgpu/src/page_curl.wgsl`），
//! 采的是这一页烘成的那张捕获纹理 —— 所以卷起来那一段**带着正文**。CPU 那一版做不到：gpui 的
//! `paint_glyph` 把字形 sprite 的变换写死成单位阵（`gpui-pre-0.3.9/src/window.rs:4882`），GPU
//! 侧认得那个矩阵，缺的只是参数没透出来，所以转了角度的字画不出来。
//!
//! 圆锥（Hong et al.「纸绕锥而不是绕柱弯，锥尖落在书脊那条线上，所以整页纸不会被拉长」）
//! **没有**实现：半径按「到锥顶的距离」线性收，手指靠近书脊那一段锥顶会跑远、半径随折线角度
//! 剧变，到某一档角度上跳一帧。

use super::{Area, Axis, Crease, Cut, Grip, PageTurnStyle, Pose, Sheet};
use crate::ui::features::reader::model::chrome::PageMode;

/// 折线倾角的上限（弧度，从读书那根轴量起）。
///
/// 读者竖着划那一笔，不夹的话折线会转成近水平 —— 纸从页顶或页底卷走，而书没有那两条自由边。
/// 这一格照的是 `reference/gmpagecurl-ios` 的 `maxPhi = π/4 − 1°`（`InputUtils.swift:64` 那句
/// `phi.clamp(to: -maxPhi...maxPhi)`，默认值在 `MetalPageCurlView.swift:60`）。
const MAX_TILT: f32 = std::f32::consts::FRAC_PI_4 - 0.017_453_293;

/// **起手卷径**占页宽的几成。
///
/// 这是「折成硬片」与「卷成圆筒」之间唯一多出来的自由度，也是苹果那种仿真感的来源：纸不是
/// 绕一条线转过去的，而是在折痕处**卷起来**一段，正对读者的那面被照亮、两次贴着切线的地方
/// 压暗 —— 于是一条纸上出现「暗—亮—暗」，读起来才有厚度。
///
/// 取 **0.14 页宽**：手机上（页宽 360 逻辑像素）卷径约 50px、柱顶横移约 100px。
///
/// 参照 `harism/android-pagecurl` 给的是页宽的 1/3 —— 那是平板上翻相册的比例，搬到 390dp
/// 的阅读页上，`πR` 比一整页还宽，整页都落进「卷在柱面上」那一段：字被剪斜、页面像被掰弯。
/// 真机截图量到的正是这个。再往前一版取 5.5%（约 21px）又小到没人看得出纸在卷。0.14 是
/// 这两个读数中间的一个，还要不要再细，等下一张截图说话。
pub(in crate::ui::features::reader) const ROLL: f32 = 0.14;

/// 没有手指那一条路的折线（点屏 / 按键 / 滚轮）。
///
/// `None` = 这块版心还没量出面积。
///
/// 只有门用得到这一个别名：生产那一头每一步都把（可能确实是 `None` 的）手交给 [`crease_at`]，
/// 不必再留一个不带手的入口。
#[cfg(test)]
pub(in crate::ui::features::reader) fn crease_of(
    progress: f32,
    forward: bool,
    area: Area,
) -> Option<Crease> {
    crease_at(progress, forward, area, None)
}

/// 这一笔的**锚点**：把按下点投影到页的自由边（往后翻是右缘），y 取按下点。
///
/// 不是页角。`harism/android-pagecurl` 在 `CurlView.onTouch` 的 `ACTION_DOWN` 里就是这么做的
/// （`mDragStartPos.x = rightRect.right`，y 夹进页内），于是纸从**手指那一格的高度**起卷。取角
/// 的话，按在页面中间往左划会先从右下角揭起来 —— 那就是读者说的「不跟手」。
///
/// 只在「正向」那一侧给（自由边 = 右缘）；往回翻由 [`flip_x`] + [`cylinder_at`] 那一对映射负责。
fn grabbed_edge(area: Area, grip: Grip) -> (f32, f32) {
    (
        area.right,
        (area.top + grip.from.1 * area.height()).clamp(area.top, area.bottom),
    )
}

/// 把手势读数（版心内归一化）映进「正向」那一套：自由边不在右就是横向 `x → 1 − x`，纵向一个字
/// 不动。
///
/// [`curl_of`] 通篇假设自由边在右、书脊在左、手指往左划，它自己不看翻页方向，也不看书从哪边读。
/// 所以凡是不满足那一套的（往回翻、从右往左读的那一本）要在**进出口各映一次**：出口那一次在
/// [`cylinder_at`] 末尾（法向反号 + 轴映回来），进这一次是它的另一半。只映出口不映进口，读出来
/// 是「只能往左滑不能往右滑」—— 往回拖那一笔的 `dir_x` 在真实坐标里是正的，进律正好被「往翻页
/// 反方向走的那一截不算」夹成 0，于是整趟都停在「还没起步」那一帧，松手才突然卷起来。
fn flip_x(p: (f32, f32), free_right: bool) -> (f32, f32) {
    if free_right { p } else { (1.0 - p.0, p.1) }
}

/// [`curl_of`] 给回的那一组：`(法向, 轴, 卷径)`，全在「正向 + 页局部」那一套里。
type Roll = ((f32, f32), (f32, f32), f32);

/// 点屏 / 按键 / 滚轮那一趟的几何：**折线自己走，不借手**。
///
/// 跟手那一律不能拿来当补间曲线用，两条都是 2026-10-19 在 Meizu 20 上量出来的：
///
/// - 那条律的推进量 `advance = (dist + curl_len) / 2` 在 `dist > 2·页宽 − πR` 那一段里，
///   代进 `curl_len = 2·页宽 − dist` 就是 **`advance ≡ 页宽`，与走了多远无关** —— 而从前点屏
///   那一笔的终点定义恰是 `dist = 2·页宽`，所以最后 22% 的时钟被数学上钉死。真机 17 帧里有
///   **5 帧画的与上一帧逐字相同**。
/// - 起手那一截 `R·sin(π·√(dist/curlLen))` 是给手指用的（√ 在 0 处斜率无穷大 = 「一碰就有
///   反应」），交给固定时钟重放就是**首帧跨 8.8% 页宽、第 11 帧材料平均位移 142.8 dp**（= 页宽
///   的 37% 一帧走完），读者看见的就是「不动、然后突然翻完」。
///
/// 所以这一支直接给折线参数化，三件事一起定：
///
/// 1. **从右下角起卷**（`tilt` 从 [`MAX_TILT`] 收到 0）：参照那一派（静读天下）逐帧读下来，
///    折线起手就是斜 43°、落位时已经转回竖直。拇指落在下缘，纸就从那一角被揭起来。
/// 2. **卷径全程是 [`ROLL`] 页宽，不收**：从前那句 `radius.min(axis.0)` 只在最后两帧起作用，
///    恰好是落位那一刻 —— 纸在最该像纸的地方先塌成一张硬片。
/// 3. **轴走到书脊之外一个卷径才收工**（`end = −卷径`）：平摊那一段的投影最远到 `轴 + 卷径`，
///    所以差一个卷径就是「纸还在画里却被删掉」，那一帧的弹跳就是「要不翻」。
///
/// 时钟不在这格里 —— 进来的是已经缓动过的 `progress`，`EaseInOut` 那条 S 形正好把折线速度
/// 铺成一个单峰。同一把尺量到的改后读数：首帧 0.0%、峰值 59.3 dp/帧、末尾零帧死图。
fn tap_roll(progress: f32, area: Area) -> Roll {
    let (width, height) = (area.width(), area.height());
    let radius = ROLL * width;
    let tilt = MAX_TILT * (1.0 - progress);
    let n = (tilt.cos(), tilt.sin());
    // 过页的右下角那一格的支撑值：轴停在这里时一个角都还没折走。
    let reach = n.0 * width + n.1 * height;
    let d = reach + (-radius - reach) * progress;
    (n, (n.0 * d, n.1 * d), radius)
}

/// 这一帧圆柱的参数，全部在「正向 + 页局部」那一套坐标里给：页占 `u ∈ [0, 宽]`、自由边在
/// `u = 宽`、书脊在 `u = 0`，`v` 从页顶往下。**进来的那个 `grip` 也得是这一套里的**（往回翻
/// 由 [`cylinder_at`] 先映一次），它自己不看翻页方向。
///
/// 法向指向**已经卷走**那一侧（与 [`Crease::past`] 同一读法），所以它可以直接填进 GPU 那份
/// uniform —— 网格的顶点本来就是页局部坐标。
///
/// **跟手那一趟的律照抄 `harism/android-pagecurl` 的 `CurlView.updateCurlPos`**（`fi.harism.curl`，
/// 安卓上这一派的事实标准，国产阅读器的「仿真」多数是它的后代）。三条一起才读成翻书：
///
/// 1. 方向与走量都来自**这一笔的划动**（按下点 → 手指此刻）。参照里那一格是从页边锚点量的，
///    于是按在页面中间、手指一格没动就已经折了半页；这里锚点只决定轴**从哪儿起步推进**。
/// 2. 卷径起手是页宽的 [`ROLL`]，随轴逼近书脊**线性收到 0**（参照里那句
///    `radius = clamp(mCurlPos.x − pageLeftX, 0, radius)`）—— 这一格是「卷」看得见的全部来源。
/// 3. 轴在柱面那一段**跑在手指前面**（`sqrt` 那一档），绕过柱顶之后推进量退到手指后面。起手快、
///    收尾稳 —— 这条就是手感本身；纯线性读起来是一根匀速平移的棍。
///
/// 这三条都以**手指**为时钟，所以只在有 `grip` 时成话：没有手指那一趟走 [`tap_roll`]，那里第 3
/// 条的 `sqrt` 与第 2 条收尾那一句各自都会退化成一处缺陷（一帧跨半页 / 最后几帧钉死不动）。
fn curl_of(progress: f32, area: Area, grip: Option<Grip>) -> Option<Roll> {
    let (width, height) = (area.width(), area.height());
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let t = progress.clamp(0.0, 1.0);
    // 没有手指（点屏 / 按键 / 滚轮）那一趟不借手：见 [`tap_roll`]。
    let Some(grip) = grip else {
        return Some(tap_roll(t, area));
    };
    let (anchor, press, finger) = {
        let grabbed = grabbed_edge(area, grip);
        (
            (grabbed.0 - area.left, grabbed.1 - area.top),
            (grip.from.0 * width, grip.from.1 * height),
            (grip.at.0 * width, grip.at.1 * height),
        )
    };
    let (dir_x, dir_y) = (finger.0 - press.0, finger.1 - press.1);
    // 往翻页方向**反面**走的那一截不算：夹成 0，画面停在「还没起步」那一帧（与
    // `swipe::progress` 那条夹法同一个读法）。这一格读的是**正向那一套**里的横向 —— 往回翻的手
    // 指坐标进律之前已经由 [`flip_x`] 映过一次，否则往右拖那一笔在这里正好被夹死。
    let dir_x = dir_x.min(0.0);
    // 倾角上限：竖着划那一笔不夹的话折线转成近水平，纸从页顶/页底卷走 —— 书没有那两条自由边。
    // 这一格是**故意偏离参照**的：harism 允许（它卷的是一张自由的纸），书不许。夹的是纵向**偏移
    // 量**而不是角度，所以横向往复一格没动的竖划正好退化成「纸还没动」。
    let limit = MAX_TILT.tan() * dir_x.abs();
    let dir_y = dir_y.clamp(-limit, limit);
    let dist = (dir_x * dir_x + dir_y * dir_y).sqrt();
    if dist < 1e-3 {
        // 一格没动：轴停在自由边之外一格，整页都在平摊侧。
        return Some(((1.0, 0.0), (width + 1.0, 0.0), 0.0));
    }
    // 划动方向，与它的反号（= 指向已经卷走那一侧）。
    let (ux, uy) = (dir_x / dist, dir_y / dist);
    let (nx, ny) = (-ux, -uy);
    let pi = std::f32::consts::PI;
    let mut radius = ROLL * width;
    let mut curl_len = pi * radius;
    // 拖到快够着对边之外：还能卷的弧长只剩「两页宽 − 走了多远」，卷径跟着收到 0。
    if dist > 2.0 * width - curl_len {
        curl_len = (2.0 * width - dist).max(0.0);
        radius = curl_len / pi;
    }
    // 轴从锚点往划动方向推进多少，两段各一条。
    let advance = if dist >= curl_len {
        // 绕过柱顶：推进量退到手指**后面**去，纸铺平盖到下面那一张上。
        (dist + curl_len) * 0.5
    } else {
        // 还在柱面上：多走一截 `R·sin(π·sqrt(dist/curlLen))`，轴跑在手指前面。
        dist + radius * (pi * (dist / curl_len).sqrt()).sin()
    };
    let axis = (anchor.0 + ux * advance, anchor.1 + uy * advance);
    // 轴逼近书脊（u = 0）的路上把卷径收掉，到书脊正好为 0。参照里这一句只挂在后段那一支，这里
    // 两段都收 —— 只挂后段的话收尾那一刻卷径从满值直接跳到 0，中间没有过渡。
    let radius = radius.min(axis.0).max(0.0);
    Some(((nx, ny), axis, radius))
}

/// GPU 卷曲所需的参数（**页局部坐标**，原点在页的左上角 —— 与网格顶点同一把尺；进场景前由
/// gpui 统一乘 `scale_factor` 换成设备像素）。
///
/// `nx·x + ny·y = d` 既是折线也是圆柱的轴；`radius` 是「折成硬片」与「卷成圆筒」之间那一个
/// 自由度，也就是这一档全部的仿真感所在。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::ui::features::reader) struct Cylinder {
    pub nx: f32,
    pub ny: f32,
    pub d: f32,
    pub radius: f32,
}

/// 这一帧的圆柱参数。`free_right` = 这一趟纸的**自由边在右**（见 [`Pose::free_edge_right`]：
/// 往回翻与「从右往左读的那一本」都是换手，两个各反一次）。`None` = 这块版心还没量出面积。
pub(in crate::ui::features::reader) fn cylinder_at(
    progress: f32,
    free_right: bool,
    area: Area,
    grip: Option<Grip>,
) -> Option<Cylinder> {
    // 律只有一套坐标，进来的手先映进那一套（自由边不在右 = 横向反号）。
    let grip = grip.map(|g| Grip::new(flip_x(g.from, free_right), flip_x(g.at, free_right)));
    let ((nx, ny), axis, radius) = curl_of(progress, area, grip)?;
    let width = area.width();
    // 算完再映回读数那一侧：`curl_of` 在「自由边在右」那一套里给，那里 u 是 `宽 − x`。法向的
    // 横向分量反号，轴的位置也跟着映回来。
    let (nx, axis_u) = if free_right {
        (nx, axis.0)
    } else {
        (-nx, width - axis.0)
    };
    Some(Cylinder {
        nx,
        ny,
        d: nx * axis_u + ny * axis.1,
        radius,
    })
}

/// 走了 `progress` 那一刻的折线，带上读者这一笔的手（起手点 + 此刻的位置）。
///
/// 就是 [`cylinder_at`] 那条轴映回**版心坐标**的那一份，所以 GPU 判「哪一半已经卷走」与逐字
/// 裁剪判「这个字还留不留」用的是同一个式子 —— 两边差一点，读出来就是「影子和纸不是同一张」。
///
/// `None` = 这块版心还没量出面积。
pub(in crate::ui::features::reader) fn crease_at(
    progress: f32,
    free_right: bool,
    area: Area,
    grip: Option<Grip>,
) -> Option<Crease> {
    let cylinder = cylinder_at(progress, free_right, area, grip)?;
    Some(Crease {
        nx: cylinder.nx,
        ny: cylinder.ny,
        d: cylinder.d + cylinder.nx * area.left + cylinder.ny * area.top,
    })
}

/// 松手之后那一腿的手指落点：沿**这一笔已经划出去的方向**继续走到整页折完（或退回起点）。
///
/// 参照里那句「animation source/target 都是手指位置，走的是同一条 `updateCurlPos`，所以输出与
/// 用户真的一路拖过去完全相同」就是这个意思。只把这一笔拉长，方向一个字不改 —— 写死一个落点
/// 的话，松手那一帧划动方向被硬掰，折线倾角跟着跳一下。
pub(in crate::ui::features::reader) fn release_grip(
    grip: Grip,
    u: f32,
    free_right: bool,
    revert: bool,
    area: Area,
) -> Grip {
    if revert {
        return Grip::new(grip.from, grip.from);
    }
    let (width, height) = (area.width(), area.height());
    let start = (grip.from.0 * width, grip.from.1 * height);
    let finger = (grip.at.0 * width, grip.at.1 * height);
    // 一律换算到「手指往左划」那一套里：自由边不在右就是横向反号。
    let sign = if free_right { 1.0 } else { -1.0 };
    let dx = (finger.0 - start.0) * sign;
    let dy = finger.1 - start.1;
    let long = (dx * dx + dy * dy).sqrt();
    let (ux, uy) = if long > 1e-3 {
        (dx / long, dy / long)
    } else {
        (-1.0, 0.0)
    };
    // 走完这一页要划过多远：那条律的推进量在 `dist` 超过 `2·页宽 − πR` 之后饱和在正好一个页宽
    // （轴落到书脊、卷径收到 0），所以落点得按**当前卷径**算，不能写死一个页宽 —— 那只在
    // R = 页宽/3 时刚好够。多让 8 像素，落位那一刻轴真的出画。
    let end = (2.0 * width - std::f32::consts::PI * ROLL * width).max(width) + 8.0;
    let travelled = long + (end - long) * u.clamp(0.0, 1.0);
    // 起点必须是**按下点**而不是页边锚点，否则 u = 0 那一格手指会先跳一下。
    let landed = (start.0 + ux * travelled * sign, start.1 + uy * travelled);
    Grip::new(grip.from, (landed.0 / width, landed.1 / height))
}

/// 仿真翻页：一张纸被卷起来折走的样子。
///
/// 两张纸都不位移（[`Curl::sheets_at`] 给的 `offset` 恒为 0），全部几何都在「这一帧的折线在哪」
/// 那一格上：新页本来就平躺在原位，旧页绕着折线**卷**过去。所以这一档比 `Cover` 还省一处 ——
/// 命中换算什么都不必减。
///
/// 旧页这一侧由 GPU 那张网格接管：`view::body` 按 [`cylinder_at`] 摆出网格、把这一页烘成的捕获
/// 纹理卷上去，于是折走那一片**带着正文**。这是 CPU 那一版做不到的事（gpui 的 `paint_glyph`
/// 把字形 sprite 的变换写死成单位阵，转了角度的字画不出来），也是这一档从前「不像翻页」的第一
/// 个原因 —— 卷起来那一片是一片空白纸。
///
/// 与 `Cover` 那一档的三处区别，就是先前「奇怪」那三个读数各自的根：
///
/// 1. **动的是旧页**。旧页被折走，新页平躺在它下面等着露出来；先前是 `Cover` 的几何 ——
///    新页盖进来、旧页停在原地被裁字，受力方向整条反了。
/// 2. **折线有倾角**，跟着手指转（见 [`crease_at`]）。先前 `Seam` 只有一个横坐标，那条边是
///    竖直扫过屏幕的一根棍。
/// 3. **折起来那一面是纸**，有自己的圆柱受光。投影是卷筒几何自己的事，不乘到平摊页面上：
///    后者会在右侧纸边罩出一条浅灰带。先前盖在接缝上的是不透明页色矩形，宽按 `α − sin α`
///    反解走，整个行程几乎不塌。
pub(in crate::ui::features::reader) struct Curl;

impl PageTurnStyle for Curl {
    fn axis(&self) -> Axis {
        Axis::Horizontal
    }

    /// 折页**两轴都认**：折线的朝向由这一笔的划动向量给，斜着划本来就对，而手机上没人会把
    /// 手指拖成一条严格的水平线。
    fn claims(&self, _axis: Axis) -> bool {
        true
    }

    fn animates(&self, _: PageMode) -> bool {
        true
    }

    /// 不带手的那一条只按「从左往右读」算（`rtl = false`）：这一格没有地方可以问那本书，
    /// 所以生产那一路走的是 [`PageTurnStyle::sheets_at`]，这条留给门与兜底。
    fn sheets(&self, t: f32, forward: bool, area: Area) -> [Sheet; 2] {
        self.sheets_at(
            t,
            area,
            Pose {
                forward,
                rtl: false,
                grip: None,
            },
        )
    }

    fn sheets_at(&self, t: f32, area: Area, pose: Pose) -> [Sheet; 2] {
        let cut = match crease_at(
            t,
            Pose::free_edge_right(pose.forward, pose.rtl),
            area,
            pose.grip,
        ) {
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
}

#[cfg(test)]
mod tests {
    //! 搬自摆位层那一份：一挡的门跟着那一档走，共用的尺在 `testing.rs`。

    use crate::ui::features::reader::motion::turn_animation::PageTurnStyle as _;
    use crate::ui::features::reader::motion::turn_animation::curl::Curl;
    use crate::ui::features::reader::motion::turn_animation::curl::MAX_TILT;
    use crate::ui::features::reader::motion::turn_animation::curl::ROLL;
    use crate::ui::features::reader::motion::turn_animation::curl::crease_at;
    use crate::ui::features::reader::motion::turn_animation::curl::crease_of;
    use crate::ui::features::reader::motion::turn_animation::curl::cylinder_at;
    use crate::ui::features::reader::motion::turn_animation::curl::release_grip;
    use crate::ui::features::reader::motion::turn_animation::geometry::Grip;
    use crate::ui::features::reader::motion::turn_animation::geometry::Poly;
    use crate::ui::features::reader::motion::turn_animation::geometry::Pose;
    use crate::ui::features::reader::motion::turn_animation::testing::{H, W, complementary, page};

    /// 圆柱的轴**就是**那条折线（逐字相等），而且卷径按那条律走：起手 [`ROLL`] 页宽、逼近书脊
    /// 收 0。
    ///
    /// 轴同源这一条钉的是「GPU 与 CPU 两套几何还是一家」：网格那头按 `n·p − d` 判哪一段该卷
    /// 起来，这一头按同一个式子裁字。两边差一点，读出来的就是「影子和纸不是同一张」。
    #[test]
    fn the_cylinder_axis_is_the_crease() {
        let area = page();
        for (forward, grip) in [
            (true, Grip::new((0.5, 0.15), (0.1, 0.2))),
            (true, Grip::new((0.5, 0.85), (0.2, 0.8))),
            (false, Grip::new((0.5, 0.4), (0.7, 0.3))),
        ] {
            for step in [0.0f32, 0.2, 0.6, 1.0] {
                let crease = crease_at(step, forward, area, Some(grip)).expect("量得出折线");
                let cylinder = cylinder_at(step, forward, area, Some(grip)).expect("量得出圆柱");
                assert_eq!(
                    (cylinder.nx, cylinder.ny, cylinder.d),
                    (crease.nx, crease.ny, crease.d),
                    "t = {step} 时圆柱的轴和折线不是同一条线"
                );
                // 法向是单位长：`n·p − d` 才是一个**距离**，网格那头的三段分界才量得对。
                assert!(
                    (crease.nx.hypot(crease.ny) - 1.0).abs() < 1e-5,
                    "t = {step} 时法向不是单位长：({}, {})",
                    crease.nx,
                    crease.ny
                );
            }
        }

        // 卷径只由「轴走到哪儿」给，不由别的东西给：**起手**是页宽的 [`ROLL`]（参照里那句是
        // `radius = pageRect.width()/3`，手机上量下来太粗，见常数自己的注释），随轴逼近书脊一路
        // 收到 0。先前这里取常数 5.5% 短边（手机上约 21px），卷曲带窄到没人看得出纸卷起来了 ——
        // 「不像翻页」的头一号原因就是这一格。
        let radii = [0.49f32, 0.0, -0.5, -1.0, -1.5]
            .into_iter()
            .map(|at_x| {
                cylinder_at(0.0, true, area, Some(Grip::new((0.5, 0.5), (at_x, 0.5))))
                    .expect("量得出圆柱")
                    .radius
            })
            .collect::<Vec<_>>();
        assert!(
            (radii[0] - ROLL * area.width()).abs() < 1.0,
            "起手卷径该是页宽的 {ROLL}，实得 {}",
            radii[0]
        );
        for pair in radii.windows(2) {
            assert!(
                pair[0] >= pair[1],
                "轴往书脊走的时候卷径只能一路收：{} → {}",
                pair[0],
                pair[1]
            );
        }
        assert!(
            radii[0] > radii[3] && radii[3] > 0.0,
            "中段该看得见一根真的筒，实得 {:?}",
            radii
        );
        assert_eq!(
            *radii.last().unwrap(),
            0.0,
            "走完那一刻卷径该收到 0：纸抬平了"
        );

        // 按下没动的那一格：纸还没离页，卷筒也还没长出来。
        let rest = cylinder_at(0.0, true, area, Some(Grip::new((0.5, 0.5), (0.5, 0.5))))
            .expect("量得出圆柱");
        assert_eq!(rest.radius, 0.0);
    }

    /// 往回翻那一笔是**往右**拖的，律照样得走，而且拖到哪儿折到哪儿。
    ///
    /// 手势读数是真实坐标，`curl_of` 只认「往左划」那一套，所以进出口各要映一次横向。只映
    /// 出口不映进口时症状很具体：往回拖的整趟都停在「还没起步」那一帧，松手才突然卷起来 —— 读者
    /// 读成「只能往左滑不能往右滑」。
    #[test]
    fn a_backward_drag_folds_the_same_way_as_its_mirror() {
        let area = page();
        // 横向镜像的两个版本：归一化那一套（手就是这一套）与版心坐标那一套（折线是这一套）。
        let flip_n = |p: (f32, f32)| (1.0 - p.0, p.1);
        let flip_p = |x: f32| area.left + area.right - x;
        for (from, at) in [
            ((0.5, 0.5), (0.7, 0.45)),
            ((0.2, 0.2), (0.55, 0.3)),
            ((0.9, 0.8), (0.95, 0.7)),
        ] {
            let back = crease_at(0.5, false, area, Some(Grip::new(from, at))).expect("量得出折线");
            let fwd = crease_at(0.5, true, area, Some(Grip::new(flip_n(from), flip_n(at))))
                .expect("同上");
            // 这一笔真的折走了东西：一片都没有就是「停在还没起步」。
            let rolled = Poly::rect(area).past(back).area().abs();
            assert!(
                rolled > 1.0,
                "往回拖到 {at:?} 时一片都没折走，折线停在 ({}, {})",
                back.nx,
                back.ny
            );
            // 而且是正向那一张的镜像：镜像点上「谁已经卷走」必须一致，页的两缘各判一次，中间
            // 再判一次。
            for x in [
                area.left + 1.0,
                (area.left + area.right) * 0.5,
                area.right - 1.0,
            ] {
                assert_eq!(
                    back.past(x, 300.0) > 0.0,
                    fwd.past(flip_p(x), 300.0) > 0.0,
                    "往回翻在 x = {x} 不是正向的镜像",
                );
            }
        }
    }

    /// 那一两帧「纸还没动」的读数（刚按下、或者往翻页反方向拖回去）：折线停在页外，但**两张纸
    /// 各自仍然有一条裁剪**。
    ///
    /// 这一格从前回 `None`，于是两张纸都没有裁剪 —— 两页的字同时出现在屏上，而那是这一层最
    /// 不该出现的读数（`turn_animation::mod` 那条「重叠的档里有一张纸没裁」的门就是这么写的）。
    /// 现在给的是一条停在页外一格的折线：旧页整张画、新页一片不画。
    #[test]
    fn a_press_that_has_not_moved_yet_still_splits_the_two_sheets() {
        let area = page();
        // 按下点在页里，手指往页外那一侧拖回去 —— 律给的就是「一格都没动」。
        let grip = Grip::new((0.5, 0.2), (1.0, 0.0));
        let crease = crease_at(0.0, true, area, Some(grip)).expect("没动也要量得出折线");
        for (x, y) in [
            (area.left, area.top),
            (area.right, area.bottom),
            (area.left, area.bottom),
        ] {
            assert!(
                crease.past(x, y) < 0.0,
                "一格没动的时候 ({x}, {y}) 该还整片平摊着，实得 past = {}",
                crease.past(x, y)
            );
        }
        let [incoming, outgoing] = Curl.sheets_at(
            0.0,
            area,
            Pose {
                forward: true,
                rtl: false,
                grip: Some(grip),
            },
        );
        let incoming_cut = incoming
            .cut
            .expect("退化成「还没动」也要有裁剪，否则两页字叠字");
        let outgoing_cut = outgoing.cut.expect("同上");
        complementary(incoming_cut, outgoing_cut, area);
        assert!(
            outgoing_cut.keeps(area.right - 1.0, area.top + 1.0),
            "整页都该还归那张停着的旧页"
        );
        // 圆柱同样量得出来，而这一刻卷筒还没长出来：网格整页画平的旧页。
        let cylinder = cylinder_at(0.0, true, area, Some(grip)).expect("同上");
        assert_eq!(cylinder.radius, 0.0, "纸还没离页那一格不该长出卷筒");
    }

    /// 从右往左读的那一本里，**往后翻**揭的是**左**边那一窄条：折页读的是「自由边在哪一侧」，
    /// 不是「往哪边翻」。
    ///
    /// 那一套律只有一套坐标（自由边在右、手指往左划），换手是在进出口各映一次横向，而映不映
    /// 由 [`Pose::free_edge_right`] 给 —— 往回翻与 RTL 各反一次，所以 `(往后翻, RTL)` 与
    /// `(往回翻, LTR)` 必须折出**同一条线**。从前这一格直接把 `forward` 当判据：RTL 那一本里
    /// 往后翻是一笔往右拖的，`dir_x` 为正被夹成 0，症状与「只能往左滑不能往右滑」一模一样。
    #[test]
    fn an_rtl_book_folds_from_the_other_edge() {
        let area = page();
        // RTL 那一本里「往后翻」那一笔是往右拖的。
        let grip = Grip::new((0.5, 0.5), (0.7, 0.5));
        for (a, b) in [
            ((true, true), (false, false)),
            ((true, false), (false, true)),
        ] {
            let x = crease_at(0.5, Pose::free_edge_right(a.0, a.1), area, Some(grip))
                .expect("量得出折线");
            let y =
                crease_at(0.5, Pose::free_edge_right(b.0, b.1), area, Some(grip)).expect("同上");
            assert_eq!(
                (x.nx, x.ny, x.d),
                (y.nx, y.ny, y.d),
                "({:?}) 与 ({:?}) 该折出同一条线",
                a,
                b
            );
        }
        // 判据反过来接不住的那一半：RTL 往后翻这一笔得真的折走了东西，而且起的是左边。
        let back =
            crease_at(0.5, Pose::free_edge_right(true, true), area, Some(grip)).expect("同上");
        let rolled = Poly::rect(area).past(back).area().abs();
        assert!(rolled > 1.0, "RTL 往右拖这一笔一片都没折走，实得整页平摊");
        assert!(
            back.past(area.left + 1.0, 300.0) > 0.0,
            "自由边在左，左缘那一窄条该已经离场"
        );
        assert!(
            back.past(area.right - 1.0, 300.0) < 0.0,
            "右缘那一条还该平摊着"
        );
        // 不带手的点屏那一路同理：RTL 里它也得从左边起。
        let tapped = crease_at(0.5, Pose::free_edge_right(true, true), area, None).expect("同上");
        assert!(
            tapped.past(area.left + 1.0, 300.0) > 0.0,
            "点屏那一趟在 RTL 里也得从左边卷起来"
        );
    }

    /// 点屏 / 按键那一趟从「一格没动」扫到「整页折走」，而且**单调**往里走。
    ///
    /// 两端都是硬读数：t = 0 时纸还没被拎起来（折线停在页外，一个角都没折走）；t = 1 时整页
    /// 都已经在空侧。中间每一步「已经卷走的面积」只能变大 —— 变小就是纸在往回长。
    #[test]
    fn a_tapped_turn_sweeps_the_whole_page_once() {
        let area = page();
        let rolled = |t: f32| {
            let crease = crease_of(t, true, area).expect("每一步都量得出折线");
            Poly::rect(area).past(crease).area().abs()
        };
        assert!(
            rolled(0.0) < 1.0,
            "起步一个角都不该折走，实得 {}",
            rolled(0.0)
        );
        let whole = area.width() * area.height();
        // 点屏那一趟走 [`tap_roll`]：折线自己从右下角扫到书脊之外一个卷径，落位那一刻整页折
        // 干净而且纸已经出画。留余数就是轴没走到底。
        assert!(
            rolled(1.0) > whole * 0.999,
            "走完该把整页折干净，实得 {} / {whole}",
            rolled(1.0)
        );
        let mut previous = 0.0f32;
        for step in 1..=60 {
            let t = step as f32 / 60.0;
            let now = rolled(t);
            assert!(
                now >= previous - 1.0,
                "t = {t} 时已经卷走的面积反而变小了：{previous} → {now}"
            );
            previous = now;
        }
    }

    /// 点屏那一趟**一路走到位**：轴每走 1/48 程都要往里推进半格以上，一帧都不许原地不动。
    ///
    /// 钉的是从前那一处代数退化：`curl_of` 里 `dist > 2·页宽 − πR` 那一段同时给出
    /// `curl_len = 2·页宽 − dist` 与 `advance = (dist + curl_len)/2`，代进去 `advance ≡ 页宽`
    /// 而与走了多远无关 —— 折线钉死在书脊。点屏那一笔的终点恰是 `dist = 2·页宽`，于是真机 17 帧
    /// 里 5 帧画的与上一帧逐字相同（读者那句「要翻要不翻」的后一半）。
    ///
    /// 用**轴位**而不是「已卷走的面积」量：面积那一读到 100% 就瞎了，而纸还在往画外走 ——
    /// 起手那几格也真的只揭得起一个角，拿面积卡会把正确的缓起步判成平台。
    #[test]
    fn a_tapped_turn_keeps_moving_to_the_last_frame() {
        let area = page();
        let axis = |t: f32| cylinder_at(t, true, area, None).expect("量得出圆柱").d;
        let mut previous = axis(0.0);
        for step in 1..=48 {
            let t = step as f32 / 48.0;
            let now = axis(t);
            assert!(
                now < previous - 0.5,
                "t = {t} 这一格轴没往前走：{previous:.1} → {now:.1}"
            );
            previous = now;
        }
    }

    /// 点屏那一趟**卷径全程不收**：纸要一直卷着出画，不许在落位前塌成一张硬片。
    ///
    /// 从前那一句 `radius.min(axis.0)` 只在最后两帧起作用（轴离书脊不足一个卷径才夹得到），
    /// 恰好是读者盯着看的那一瞬：真机读数 53.8 → 45.9 → 11.0 → 0，卷感在 33 ms 内消失。
    #[test]
    fn a_tapped_turn_keeps_its_curl_until_the_paper_is_gone() {
        let area = page();
        for step in 1..=48 {
            let t = step as f32 / 48.0;
            let cylinder = cylinder_at(t, true, area, None).expect("量得出圆柱");
            assert!(
                (cylinder.radius - ROLL * area.width()).abs() < 1e-3,
                "t = {t} 时卷径塌到了 {}，全程该是页宽的 {ROLL}",
                cylinder.radius
            );
        }
        // 而走完那一刻纸确实出了画：轴在书脊之外至少一个卷径。
        let end = cylinder_at(1.0, true, area, None).expect("同上");
        assert!(
            end.d <= -end.radius,
            "落位时轴停在 d = {}，卷径 {}：纸还压在页里就被删掉",
            end.d,
            end.radius
        );
    }

    /// 点屏那一趟**从右下角起卷**，落位前折线转回竖直。
    ///
    /// 拇指落在下缘，纸就从那一角被揭起来 —— 静读天下逐帧读下来就是这个形状（起手斜约 43°、
    /// 收尾竖直）。旧那一版取「自由边正中笔直往里」，起手那一帧就把整条右缘连顶角一起揭走。
    #[test]
    fn a_tapped_turn_lifts_from_the_bottom_corner() {
        let area = page();
        let early = crease_of(0.10, true, area).expect("量得出折线");
        assert!(
            early.past(area.right, area.bottom) > 0.0,
            "走一成程时右下角该已经离页，实得 {}",
            early.past(area.right, area.bottom)
        );
        assert!(
            early.past(area.right, area.top) < 0.0,
            "走一成程时右缘顶上那一格还该平摊着，实得 {}",
            early.past(area.right, area.top)
        );
        let end = crease_of(1.0, true, area).expect("同上");
        assert!(
            end.ny.abs() < 0.01,
            "落位时折线该已经转回竖直，实得 ny = {}",
            end.ny
        );
    }

    /// 点屏那一趟**首帧不许跨半页**：一帧（60 Hz 下 1/17 程）走掉的页面积不超过 3%。
    ///
    /// 那条 `R·sin(π·√(dist/curlLen))` 是给手指用的 —— √ 在 0 处斜率无穷大，「一碰就有反应」
    /// 正是它存在的理由。交给固定时钟重放，进度走满一格（1/17 程）就已经翻掉 **25.7% 页面积**；
    /// 真机上那一格还要过一遍 `EaseInOut`（那一刻时钟只走到 0.7% 进度），读数是 8.8% —— 两个都
    /// 是「没有起步这回事」，第 11 帧更是一帧走掉页宽的三成七。
    #[test]
    fn a_tapped_turn_does_not_leap_on_the_first_frame() {
        let area = page();
        let whole = area.width() * area.height();
        let rolled = |t: f32| {
            let crease = crease_of(t, true, area).expect("量得出折线");
            Poly::rect(area).past(crease).area() / whole
        };
        let first = rolled(1.0 / 17.0) - rolled(0.0);
        let leapt = first * 100.0;
        assert!(
            first < 0.03,
            "第一帧就走掉了 {leapt:.1}% 页宽，读者看见的是「突然翻完」而不是起步"
        );
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
        // 走一半时页的四分之一处还在平摊侧，而且离折线有一截（那一格折线正斜着扫过页面中部）。
        let middle = crease_of(0.5, true, page()).expect("同上");
        assert!(
            middle.past(W * 0.25, H * 0.5) < -1.0,
            "走一半时页的四分之一处该明显还在平摊侧，实得 {}",
            middle.past(W * 0.25, H * 0.5)
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
    /// 折线的**朝向**由这一笔的划动方向给：往哪划就往哪折。
    ///
    /// 参照是 `harism/android-pagecurl` 的 `CurlView`：锚点是按下点投影到页的自由边那一点
    /// （既不是页角、也不是按下点本身），折线垂直于划动向量。于是按在页面中间往左划，是从**手指
    /// 那一格的高度**起卷 —— 固定角那一版一上来先揭的是某个角。划动向量这里取
    /// `手指此刻 − 按下点`，参照那一格是从锚点量的，差别见 `curl_of` 的第 1 条。
    #[test]
    fn the_fold_follows_the_drag_direction() {
        let area = page();
        for from in [(0.5, 0.5), (0.5, 0.15), (0.5, 0.85)] {
            for at in [(0.1, 0.5), (0.1, 0.35), (0.1, 0.65), (0.3, 0.45)] {
                let crease =
                    crease_at(0.5, true, area, Some(Grip::new(from, at))).expect("量得出折线");
                let (vx, vy) = (
                    (at.0 - from.0) * area.width(),
                    (at.1 - from.1) * area.height(),
                );
                let long = (vx * vx + vy * vy).sqrt();
                let dot = (crease.nx * vx + crease.ny * vy).abs() / long;
                if (vy / vx).abs() <= MAX_TILT.tan() + 1e-3 {
                    // 倾角上限之内：法向必须与划动向量平行。
                    assert!(
                        (dot - 1.0).abs() < 1e-3,
                        "手指划向 ({vx:.0}, {vy:.0})，折线法向却给到 ({}, {})：|n·v̂| = {dot}",
                        crease.nx,
                        crease.ny,
                    );
                } else {
                    // 超出上限的那几笔：法向只能被夹到上限那一格，仍然不许反向。
                    assert!(
                        crease.nx > 0.0,
                        "斜到超出倾角上限时折线该被夹住而不是翻面，实得 ({}, {})",
                        crease.nx,
                        crease.ny
                    );
                    assert!(
                        (crease.ny / crease.nx).abs() <= MAX_TILT.tan() + 1e-3,
                        "倾角夹住了没有：实得斜率 {}",
                        (crease.ny / crease.nx).abs()
                    );
                }
            }
        }
        // 同一个锚点，往上划与往下划转出**相反**的倾角。
        let up =
            crease_at(0.5, true, area, Some(Grip::new((0.5, 0.5), (0.1, 0.35)))).expect("同上");
        let down =
            crease_at(0.5, true, area, Some(Grip::new((0.5, 0.5), (0.1, 0.65)))).expect("同上");
        assert!(
            up.ny > 0.0 && down.ny < 0.0,
            "折线倾角没跟着划动方向转：往上划 ny = {}，往下划 ny = {}",
            up.ny,
            down.ny
        );
        // 竖着划那一笔被倾角上限夹住：不夹的话折线转成近水平，纸从页顶/页底卷走 —— 书没有
        // 那两条自由边。（这一格是故意偏离参照的：它卷的是一张自由的纸。）
        let steep =
            crease_at(0.5, true, area, Some(Grip::new((0.5, 0.5), (0.48, 0.9)))).expect("同上");
        assert!(
            (steep.ny / steep.nx).abs() <= MAX_TILT.tan() + 1e-3,
            "倾角夹住了没有：实得斜率 {}",
            (steep.ny / steep.nx).abs()
        );
        // 起步那一格纸还没动：整页都在平摊侧。
        let rest =
            crease_at(0.0, true, area, Some(Grip::new((0.5, 0.5), (0.5, 0.5)))).expect("同上");
        for (x, y) in [(area.left, area.top), (area.right, area.bottom)] {
            assert!(
                rest.past(x, y) < 0.0,
                "按下没动的时候 ({x}, {y}) 该还平摊着，实得 past = {}",
                rest.past(x, y)
            );
        }
    }

    /// 松手那一腿沿**同一方向**把这一笔推到走完，中途不改朝向。
    ///
    /// 参照里那句「animation source/target 都是手指位置，走的是同一条 `updateCurlPos`，所以
    /// 输出与用户真的一路拖过去完全相同」就是这个意思。写死一个落点的话，松手那一帧划动方向
    /// 被硬掰，折线倾角跟着跳一下。
    #[test]
    fn the_release_stretches_the_stroke_without_turning_it() {
        let area = page();
        let grip = Grip::new((0.5, 0.5), (0.3, 0.4));
        let slope = |at: (f32, f32)| {
            (at.1 * area.height() - grip.from.1 * area.height())
                / (at.0 * area.width() - grip.from.0 * area.width())
        };
        let start = slope(grip.at);
        let mut previous_length = 0.0f32;
        for u in [0.25, 0.5, 0.75, 1.0] {
            let run = release_grip(grip, u, true, false, area);
            assert!(
                (slope(run.at) - start).abs() < 1e-3,
                "u = {u} 时松手那一腿把这一笔转了方向：斜率 {start} → {}",
                slope(run.at)
            );
            let length = ((run.at.0 - grip.from.0) * area.width()).powi(2)
                + ((run.at.1 - grip.from.1) * area.height()).powi(2);
            assert!(
                length > previous_length,
                "u = {u} 时纸该继续往外走：{previous_length} → {length}"
            );
            previous_length = length;
        }
        // 走完那一刻整页都该已经折走 —— 用**水平**那一笔。参照那条律的收尾是
        // `curlLen = 2·页宽 − dist`，它对斜笔不额外补投影，所以斜到 44° 那一档时轴只能停在
        // 离书脊还有两成的地方（这一格是参照本身的边界，不是这里的走样；真翻页落定靠的是页号
        // 翻过去，不靠轴走到 0）。
        let straight = Grip::new((0.5, 0.5), (0.3, 0.5));
        let end = release_grip(straight, 1.0, true, false, area);
        let crease = crease_at(1.0, true, area, Some(end)).expect("落位量得出折线");
        // 按**面积**判，不按某个角点判：那条律的推进量在绕顶之后饱和在正好一个页宽，轴停在
        // 书脊上，页的左上角正好落在折线那一格（`past = 0`）。按角点判会卡在边界等号上，
        // 而读者在意的是「整页有没有走干净」。
        let rolled = Poly::rect(area).past(crease).area().abs();
        let whole = area.width() * area.height();
        assert!(
            rolled > whole * 0.999,
            "落位那一刻整页都该已经卷走，实得 {rolled} / {whole}"
        );
        // 弹回那一腿退回按下那一点：一格都没动。
        let back = release_grip(grip, 1.0, true, true, area);
        assert_eq!(back.at, back.from, "弹回该退回起点");
    }

    /// 起步那几像素，轴**走得比手指还深** —— 那条 `sqrt` 就是「纸被滚起来」的读数。
    ///
    /// 轴从页边锚点沿划动方向推进，推进量在柱面那一段是 `dist + R·sin(π·sqrt(dist/curlLen))`：
    /// 多出来的那一截让折线比「轴只跟着手指平移」那一版更早进页。把那一截去掉（纯线性 = 一根匀速
    /// 平移的棍），这里量的正好是轴该到达的那一格，读数落在 0 上，门就红。
    #[test]
    fn the_axis_leads_the_finger_on_the_way_in() {
        let area = page();
        let from = (0.5, 0.5);
        for fraction in [1.0 / 16.0, 1.0 / 8.0, 1.0 / 4.0] {
            let stroke = area.width() * fraction;
            let crease = crease_at(
                0.5,
                true,
                area,
                Some(Grip::new(from, (from.0 - fraction, 0.5))),
            )
            .expect("量得出折线");
            // 纯线性那一版把轴停在「锚点 − 划动距离」：锚点在自由边上，页局部就是 `宽 − stroke`。
            let linear = area.width() - stroke;
            let past = crease.past(linear, area.top + 0.5 * area.height());
            assert!(
                past > 1.0,
                "划 {stroke:.0} 像素那一刻折线该已经越过纯线性的那一格，实得 past = {past}"
            );
        }
    }
}
