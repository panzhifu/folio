//! 排版结果 → 绘制单元：把一段排版 y 覆盖到的行，摊成一批「已定好基线」的
//! [`GlyphRun`] 与若干绝对摆位的图片。
//!
//! 从 [`crate::ui::features::reader::view`] 里抽出来的是**纯计算**：几何全部来自
//! 排版层，这里只加三个偏移 —— 左边这一栏的 `x`（版心在窗口里的位置）、向上的
//! `origin_y`（这一片的起点）、以及 `top`（页眉那条带子占掉的高度，正文从它下面开始
//! 画）。不带任何 gpui 元素，只有 `GlyphRun`（它本身是个数据 struct），所以这一层
//! 能在宿主上被 `cargo test` 跑、也能被别的摆法复用（连续滚动整章一条，分页/双页逐片）。

use std::ops::Range;
use std::sync::Arc;

use folio_core::layout::LineFragment;
use gpui_kit::{Hsla, SharedString};

use super::document::Document;
use super::measure::FontCache;
use super::paint::GlyphRun;
use crate::ui::widgets::run_color;

/// 可见范围内的一张图：位置与尺寸已由排版算好，这里只负责把「画在哪儿」记下来。
pub struct VisibleImage {
    /// 相对本元素原点的左上角与尺寸（逻辑像素）。
    pub x: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    pub image: Arc<gpui_kit::Image>,
}

/// 一屏（或一栏）要画的全部单元。
#[derive(Default)]
pub struct Scene {
    pub runs: Vec<GlyphRun>,
    pub images: Vec<VisibleImage>,
}

impl Scene {
    /// 把 `[lines]` 这段排版行里、被 `cull`（排版 y 区间）覆盖到的行摆进场景。
    ///
    /// 行内每个 run 用自己的宽度累加定 x；基线一律
    /// `line.y + line.baseline − origin_y + top`：gpui 自己的 `paint_line` 先把 run 的
    /// line-height 盒顶摆到 `baseline − (盒高 + ascent − descent)/2`，再由它把基线落回盒内
    /// 同一个偏移，两头抵消，结果就是这一条线。我们在 `paint` 那一层自己画字形，所以直接用它。
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        document: &Document,
        cache: &mut FontCache,
        family: &SharedString,
        lines: Range<usize>,
        cull: Range<f32>,
        origin_y: f32,
        x: f32,
        top: f32,
        foreground: Hsla,
    ) -> Self {
        // 等宽偏好与书的字体整份取一次：一个场景里所有 run 共用，别在逐 run 里克隆。
        let mono = SharedString::from(document.prefs().mono_font_family.clone());
        let faces = document.fonts();
        let layout = document.layout();
        let mut runs = Vec::new();
        let mut images = Vec::new();
        for ix in lines {
            let Some(line) = layout.lines.get(ix) else {
                continue;
            };
            if line.y + line.height < cull.start || line.y > cull.end {
                continue;
            }
            // 行文本住在所属块的那个归一化串里（`LineRun::norm` 是它的字节区间）。
            let Some(block) = layout.block_of_line(ix) else {
                continue;
            };
            let mut x = x + line.x;
            let baseline = line.y + line.baseline - origin_y + top;
            for fragment in &line.fragments {
                match fragment {
                    LineFragment::Text(run) => {
                        let text = block.text.get(run.norm.clone()).unwrap_or("");
                        if text.is_empty() {
                            x += run.width;
                            continue;
                        }
                        runs.push(
                            GlyphRun::new(
                                SharedString::new(text),
                                cache.resolve(&run.style.font, family, &mono, faces),
                                run.style.font_size,
                            )
                            // 每条 run 自己的基线：`vertical-align` 折出来的偏移在这里减掉，
                            // 正数（往上抬）就是基线 y 变小。行盒高与行基线已由排版层算过，
                            // 这里只搬运那一个数。
                            .at(x, baseline - run.style.baseline_shift)
                            .color(run_color(run.style.color, foreground))
                            .decorate(run.style.underline, run.style.line_through),
                        );
                        x += run.width;
                    }
                    LineFragment::Image(placed) => {
                        // 图片交给 gpui 自己的 `img` 元素画（纹理上传由它负责），
                        // 底贴基线：盒顶 = 基线 − 图高。解不出资源的 `<img>` 留位不画。
                        if let Some(image) = document.images().render_source(&placed.href) {
                            images.push(VisibleImage {
                                x,
                                top: baseline - placed.height,
                                width: placed.width,
                                height: placed.height,
                                image,
                            });
                        }
                        x += placed.width;
                    }
                }
            }
        }
        Self { runs, images }
    }
}
