//! 内嵌 `<audio>` 的排版占位行：一条固定高的块级行，几何进块表、节点指回元素。
//!
//! 屏上的播放器样子（按钮/进度）由 app 叠加绘制 —— 引擎只管几何与归属；播放状态
//! 不进排版，改了不许重排。排版对音频的全部认知就两条：有 `src` 的 `<audio>` 占
//! 一条 [`ROW_HEIGHT`] 高的行，除此之外与普通空块无异。

use super::collect::Ctx;
use folio_format::dom::NodeId;

/// 一条音频行的几何与资源：注册时折一次，命中与绘制读这份短表
///（与栏线、装饰种子同一条纪律：不每帧扫整张块表）。
#[derive(Debug)]
pub struct AudioRow {
    /// 占位行的归属元素（`<audio>`），命中侧用它反查属性。
    pub node: NodeId,
    /// `src` 属性原样（章内相对路径）；折成容器路径由 app 按 `resolve_href` 办。
    pub src: String,
    /// 边框盒。横排是排版坐标；竖排经组装那段与块表同一刀投影成物理坐标。
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// 占位行的固定高（逻辑像素）。音频元素没有内容可量，高度写死。
pub(super) const ROW_HEIGHT: f32 = 48.0;

/// 这一节点是不是音频行：有 `src` 的 `<audio>`。没有 `src` 的音频元素播不了，
/// 不占位（与浏览器「无声的 audio 不渲染控件」同一口径）。
pub(super) fn is_row(ctx: &Ctx<'_>, node: NodeId) -> bool {
    ctx.dom.is_element(node, "audio") && ctx.dom.attr(node, "src").is_some()
}

/// 全量路径：注册音频行（块表一条 border 盒 + 短表一条），返回占掉的高。
pub(super) fn register(ctx: &mut Ctx<'_>, node: NodeId, x: f32, cursor: f32, width: f32) -> f32 {
    let index = ctx.push_block(node, x, cursor, width, String::new(), true);
    ctx.close_block(index, ROW_HEIGHT);
    let src = ctx.dom.attr(node, "src").unwrap_or_default().to_string();
    ctx.audio_rows.push(AudioRow {
        node,
        src,
        x,
        y: cursor,
        width,
        height: ROW_HEIGHT,
    });
    ROW_HEIGHT
}
