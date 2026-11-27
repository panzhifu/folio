//! 仿真翻页的网格与捕获键：页面网格的顶点/索引按尺寸缓存一份，三张纹理各自
//! 「哪一帧把哪一格交给了捕获 pass」。单开一间是因为 `session.rs` 贴着行数基线；
//! 消费路径经 `session` 的 re-export 沿用旧名。

use std::sync::Arc;

/// 仿真翻页那张页面网格的顶点与索引。
///
/// 它们只由页宽页高决定 —— 翻页进度改的是 uniform，顶点一个都不动。所以按尺寸缓存一份：
/// 每帧重造是 9409 个顶点加 55296 个索引（近 1 MB）的分配与拷贝，全在 `render()` 里跑，
/// 而算出来的字节和上一帧一模一样。
#[derive(Clone)]
pub(in crate::ui::features::reader) struct CurlMesh {
    pub(in crate::ui::features::reader) width: f32,
    pub(in crate::ui::features::reader) height: f32,
    pub(in crate::ui::features::reader) vertices: Arc<[gpui_kit::PageCurlVertex]>,
    pub(in crate::ui::features::reader) indices: Arc<[u32]>,
}

/// 「哪一帧把完整页框交给了捕获 pass」这一格：`(章号, 页号)` + 那一刻的窗口尺寸（逻辑像素）。
///
/// 键与尺寸要一起比，所以合成一个类型：页号章号在分屏 / 折叠展开 / surface 重建后一个字不动，
/// 只有尺寸这一格拦得住那张被裁过的纹理。
pub(in crate::ui::features::reader) type CaptureCommit = ((usize, usize), (f32, f32));

/// 背面那张纹理里躺着的是哪一格：`(章号, 页号, 物理栏)` + 那一刻的窗口尺寸。
///
/// 比正面那一格多出**栏**：双页折页揭起来那张纸的背面是目标跨页的另一半，同一个页号在
/// 往前翻与往回翻时要的是左右两栏里不同的那一栏（栏号是物理的，RTL 的书与逻辑序相反）。
pub(in crate::ui::features::reader) type BackCommit = ((usize, usize, usize), (f32, f32));

/// 进场页那张纹理里躺着的是哪一格：`(章号, 页号)` + 那一刻的窗口尺寸。
///
/// 口径同 [`CaptureCommit`]。只服务平摊翻页（左右平移 / 上下平移 / 覆盖）：那一族没有
/// 背面与栏可言，进场页整张进纹理，键不需要栏那一格。
pub(in crate::ui::features::reader) type IncomingCommit = ((usize, usize), (f32, f32));
