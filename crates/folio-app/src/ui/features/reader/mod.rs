//! 阅读功能：两端各有一套渲染实现，这里按 target 选一份，对上层只暴露一个名字。
//!
//! - 桌面：`TextView` + 虚拟列表（[`desktop`]），把排好的行摆进 gpui-kit 的列表。
//! - 移动：自绘字形（[`mobile`] + [`paint`]），把排好的行逐行画出来。
//!
//! 排版本身两端共用（`crate::ui::shared::document`），所以差别只在「怎么把行画上屏」。
//! 这是整个界面层**唯一**按平台分叉的地方；上层（外壳、书架）只认 [`ReaderView`]。

#[cfg(not(mobile))]
mod desktop;
#[cfg(not(mobile))]
pub mod engine;

// 阅读页浮层的决策（点击分区、面板档位、进度念法）。没有 gpui 依赖，所以两端都编，
// 宿主测试直接跑它，渲染才分平台。
pub mod chrome;

// 阅读页：被外壳 push 进书架栈的那一屏（自带工具条/顶栏与状态栏）。
#[cfg(not(mobile))]
mod page_desktop;
#[cfg(mobile)]
mod page_mobile;
#[cfg(not(mobile))]
pub use page_desktop::ReadingPage;
#[cfg(mobile)]
pub use page_mobile::ReadingPage;
#[cfg(mobile)]
mod mobile;
#[cfg(mobile)]
pub mod paint;

#[cfg(not(mobile))]
pub use desktop::ReaderView;
#[cfg(mobile)]
pub use mobile::ReaderView;

use gpui_kit::{App, Context, Render, SharedString};

/// 外壳与「一本书的渲染」之间的**唯一**接口。
///
/// 两端渲染实现差异很大（桌面虚拟列表 / 移动自绘字形），但外壳、书架、将来的双栏
/// 只认这一组方法，于是 `cfg` 不会漏进上层。两套 [`ReaderView`] 各实现它。
pub trait ReaderHost: Render {
    /// 当前这本书的主键；兜底视图没有书时 `None`。
    fn book_id(&self) -> Option<i64>;
    /// 书名。
    fn title(&self) -> SharedString;
    /// 「章标题 · 章内位置」，给顶部条与状态栏用。
    fn position_label(&self, cx: &App) -> String;
    /// 落库用的模式名（进度坐标系随模式变）。
    fn mode_name(&self) -> &'static str;
    /// 能不能往这个方向换章。按钮的 disabled 态由它决定。
    fn can_advance(&self, delta: isize) -> bool;
    /// 换章。
    fn advance_chapter(&mut self, delta: isize, cx: &mut Context<Self>);
    /// 外壳报告「阅读区之外还占着多少」（桌面 = 侧栏宽，移动 = 底栏高），
    /// 正文的有效视口据此缩短。
    fn set_reserve(&mut self, reserve: f32, cx: &mut Context<Self>);
    /// 把当前阅读位置写回书库。
    fn persist(&mut self, cx: &mut Context<Self>);
}

// 为什么用泛型包装而不是直接 `reader.title()`：两套 `ReaderView` 都有同名的**固有**方法，
// 而固有方法会盖住 trait 方法（`title` 在桌面返回 `&SharedString`、移动返回 `SharedString`，
// 直接 `.title()` 会按各自的固有签名解析）。外层代码是同一份、两端都编，所以统一走这里，
// 让它落入 `R: ReaderHost` 的泛型解析、只认 trait。
pub fn book_id<R: ReaderHost>(reader: &R) -> Option<i64> {
    reader.book_id()
}
pub fn title<R: ReaderHost>(reader: &R) -> SharedString {
    reader.title()
}
pub fn position_label<R: ReaderHost>(reader: &R, cx: &App) -> String {
    reader.position_label(cx)
}
pub fn mode_name<R: ReaderHost>(reader: &R) -> &'static str {
    reader.mode_name()
}
pub fn can_advance<R: ReaderHost>(reader: &R, delta: isize) -> bool {
    reader.can_advance(delta)
}
pub fn advance_chapter<R: ReaderHost>(reader: &mut R, delta: isize, cx: &mut Context<R>) {
    reader.advance_chapter(delta, cx);
}
pub fn set_reserve<R: ReaderHost>(reader: &mut R, reserve: f32, cx: &mut Context<R>) {
    reader.set_reserve(reserve, cx);
}
pub fn persist<R: ReaderHost>(reader: &mut R, cx: &mut Context<R>) {
    reader.persist(cx);
}

/// 这台设备上**真能解析出**的字体家族。
///
/// Android 注册的家族与桌面不同，`TextSystem::resolve_font` 解析不到时是 panic，
/// 所以请求的家族要先落到一个存在的上面。桌面端原样返回 —— gpui-kit 的默认字体可用。
pub fn device_family(wanted: &str, cx: &gpui_kit::App) -> String {
    #[cfg(mobile)]
    {
        mobile::device_family(wanted, cx)
    }
    #[cfg(not(mobile))]
    {
        let _ = cx;
        wanted.to_string()
    }
}
