//! 把这一屏的 run 过一遍章的 shaping 表。

use crate::ui::features::reader::model::chapter_layout::Document;
use crate::ui::features::reader::view::paint::shape;
use crate::ui::features::reader::view::scene::Scene;
use gpui_kit::Window;

/// 把这一屏的 run 逐条过一遍章的 shaping 表：命中的抬一次引用计数，未命中的当场 shape
/// 一次并存进去（[`Document::shaped_line`]）。
///
/// 为什么在这一层而不是绘制层：查表要同时拿到 `Document`（表挂在它身上）与 `Window`
/// （shape 要走窗口级文本系统），而 `GlyphsElement::paint` 只有后者。
///
/// 没有身份的那条 run（`key` 是 `None`：兜底行、测试里手搓的）跳过，绘制时它自己会 shape。
pub(super) fn shape_runs(scene: &mut Scene, doc: &Document, window: &Window) {
    for run in &mut scene.runs {
        let Some(key) = run.key else { continue };
        let shaped = doc.shaped_line(key, || shape(window, run));
        run.shaped = Some(shaped);
    }
}

