//! 阅读视图：`Render` 实现 + 触摸滚动。
//!
//! 没有 gpui-kit 的组件层，所以这里只有两样东西：一个铺满窗口、接滚动事件的 `div`，
//! 和里面那个自己写的 [`GlyphsElement`]。滚动**不自己做惯量**：
//! `gpui-mobile` 的平台层已经把触摸翻译成了 `ScrollWheel` 事件
//! （`gpui-mobile/src/android/window.rs:1316` 起，用 `momentum::VelocityTracker` 估速度、
//! 用 `MomentumScroller` 在松手后每帧派发一次衰减增量），
//! 视图只要把事件里的 dy 累加到 `scroll_y`。在这里再套一个 `MomentumScroller`
//! 会把同一份惯量算两遍，滚起来直接翻倍。

use std::path::{Path, PathBuf};

use folio_core::layout::{FontKey, Layout, LineFragment};
use folio_core::Reader;
use gpui::{
    div, hsla, App, Context, Hsla, InteractiveElement, IntoElement, ParentElement, Render, Rgba,
    ScrollDelta, ScrollWheelEvent, SharedString, Styled, Window,
};

use crate::document::{chapter_xhtml, open, Page};
use crate::measure::mobile_font;
use crate::paint::{GlyphRun, GlyphsElement};

/// 纸色背景 + 近黑正文。主题系统（桌面端从框架主题目录读）还没接到移动端，
/// 所以先给一组固定值，不假装是主题。
pub(crate) fn paper() -> Hsla {
    hsla(0.13, 0.30, 0.97, 1.0)
}

pub(crate) fn ink() -> Hsla {
    hsla(0.0, 0.0, 0.13, 1.0)
}

/// 次要文字（未选中的导航项、空状态的说明行）与正文/导航栏之间那条发丝线。
/// 色相跟 `paper` 一致，免得灰线在纸色上偏冷。
pub(crate) fn muted() -> Hsla {
    hsla(0.13, 0.12, 0.42, 1.0)
}

/// folio-core 的 `Color` 是 0–255 的 u8 三元组，gpui 要 0–1 的 `Rgba`。
fn run_color(color: Option<folio_core::css::computed::Color>) -> Hsla {
    match color {
        Some(c) => Rgba {
            r: c.0 as f32 / 255.0,
            g: c.1 as f32 / 255.0,
            b: c.2 as f32 / 255.0,
            a: 1.0,
        }
        .into(),
        None => ink(),
    }
}

/// 视图现在拿的是什么内容。
pub enum Source {
    /// Increment A：一行硬编码文本。第一次上机靠它验证
    /// 「生命周期 + 渲染器 + 字体」三件事同时成立。
    Sample(SharedString),
    /// Increment B：一条真实文档路径，正文来自 folio-core 的 `Layout`。
    Document {
        title: SharedString,
        chapter: usize,
        page: Page,
    },
}

pub struct ReaderView {
    source: Source,
    prefs: Reader,
    /// 已滚过的像素（内容坐标，向下为正）。
    scroll_y: f32,
    /// 上一帧的内容总高与**有效**视口高（已减掉 `bottom_reserve`）。滚动事件发生时手上没有
    /// `Window`，夹范围只能用这两个记下来的值，下一帧渲染时再校正一次。
    content_height: f32,
    viewport_height: f32,
    /// 窗口底部被系统外壳占掉的高度（逻辑像素，目前就是 `shell` 的导航栏）。
    /// 正文的可见行与滚动上限都按「窗口高减掉它」这个有效视口算，
    /// 否则文档最后这么一截永远滚不进视野。
    bottom_reserve: f32,
}

/// Increment A 用的那行字。中英混排，顺带把 fallback 路径也验一遍。
const SAMPLE_LINE: &str = "Folio — 中文正文与 Latin 混排的一行";

impl ReaderView {
    /// 冷启动：找得到文档就走 Increment B，否则退回 Increment A 的那一行。
    pub fn boot(cx: &App) -> Self {
        let family = device_family(cx);
        for path in candidate_paths() {
            if path.is_file() {
                match Self::document_with(&path, &family) {
                    Ok(view) => {
                        log::info!("ReaderView: 打开 {path:?}");
                        return view;
                    }
                    Err(err) => log::error!("ReaderView: 打开 {path:?} 失败：{err:#}"),
                }
            }
        }
        log::info!("ReaderView: 没有可读文档，进入 Increment A（单行文本）");
        Self::sample_with(&family)
    }

    pub fn sample() -> Self {
        Self::sample_with(&Reader::default().font_family)
    }

    fn sample_with(family: &str) -> Self {
        Self {
            source: Source::Sample(SAMPLE_LINE.into()),
            prefs: reader_with_family(family),
            scroll_y: 0.0,
            content_height: 0.0,
            viewport_height: 0.0,
            bottom_reserve: 0.0,
        }
    }

    /// 用 folio-core 打开一个文档，排它的第一章。
    pub fn document(path: &Path) -> folio_core::Result<Self> {
        Self::document_with(path, &Reader::default().font_family)
    }

    fn document_with(path: &Path, family: &str) -> folio_core::Result<Self> {
        let prefs = reader_with_family(family);
        let (meta, flow) = open(path)?;
        let chapter = 0usize;
        let ch = flow
            .chapters
            .first()
            .ok_or_else(|| folio_core::Error::UnsupportedFormat(meta.title.clone()))?;
        let xhtml = chapter_xhtml(ch, meta.format);
        let page = Page::new(xhtml, &flow.css, prefs.clone());
        Ok(Self {
            source: Source::Document {
                title: meta.title.into(),
                chapter,
                page,
            },
            prefs,
            scroll_y: 0.0,
            content_height: 0.0,
            viewport_height: 0.0,
            bottom_reserve: 0.0,
        })
    }

    pub fn title(&self) -> SharedString {
        match &self.source {
            Source::Sample(_) => "folio-mobile".into(),
            Source::Document { title, .. } => title.clone(),
        }
    }

    /// 告知底部导航栏的高度（逻辑像素），正文据此缩短自己的有效视口。
    pub(crate) fn set_bottom_reserve(&mut self, height: f32) {
        self.bottom_reserve = height;
    }

    /// 平台层算好的滚动增量落到 `scroll_y`。
    ///
    /// 触摸向下滑（dy > 0）时内容跟着手指往下走 ⇒ 露出的行号变小，所以是减。
    fn on_scroll(&mut self, event: &ScrollWheelEvent) {
        let dy = match event.delta {
            ScrollDelta::Pixels(delta) => f32::from(delta.y),
            // 鼠标滚轮（外接键盘/调试时）按「行」给增量，按正文字号折算，别当 0 忽略。
            ScrollDelta::Lines(lines) => lines.y * self.prefs.font_size,
        };
        if dy == 0.0 {
            return;
        }
        let max = (self.content_height - self.viewport_height).max(0.0);
        self.scroll_y = (self.scroll_y - dy).clamp(0.0, max);
    }

    /// 把 `[scroll_y, scroll_y + 视口高]` 覆盖到的行盒摊成绘制用的 run 列表。
    ///
    /// 几何全部来自排版层，这里只加两个偏移：左边的 `margin_x`（版心在窗口里的位置）
    /// 和向上的 `scroll_y`。行内每个 run 用自己的宽度累加定 x；
    /// 基线一律 `line.y + line.baseline` —— 桌面端 `line_element` 先把 run 的
    /// line-height 盒顶摆到 `baseline - (盒高 + ascent − descent)/2`、
    /// 再由 gpui 把基线落回盒内同一个偏移，两头抵消，结果就是这一条线。
    /// 这里自己画字形，所以直接用它。
    ///
    /// 自由函数而不是方法：渲染时要一边可变借用 `self.source`（拿 `&mut Page` 排版）
    /// 一边读 `self.scroll_y`，写成 `&self` 方法会直接撞借用检查器。
    fn line_runs(page: &Page, layout: &Layout, scroll_y: f32, viewport: f32) -> Vec<GlyphRun> {
        let prefs = page.prefs();
        let top = scroll_y;
        let bottom = top + viewport.max(1.0);
        let family = SharedString::from(prefs.font_family.clone());
        let mut out = Vec::new();
        for (ix, line) in layout.lines.iter().enumerate() {
            if line.y + line.height < top || line.y > bottom {
                continue;
            }
            // 行文本住在所属块的那个归一化串里（`LineRun::norm` 是它的字节区间）。
            let Some(block) = layout.block_of_line(ix) else {
                continue;
            };
            let mut x = prefs.margin_x + line.x;
            let baseline = line.y + line.baseline - top;
            for fragment in &line.fragments {
                match fragment {
                    LineFragment::Text(run) => {
                        let text = block.text.get(run.norm.clone()).unwrap_or("");
                        if text.is_empty() {
                            x += run.width;
                            continue;
                        }
                        let font = mobile_font(&FontKey::clone(&run.style.font), &family);
                        out.push(
                            GlyphRun::new(
                                SharedString::new(text.to_string()),
                                font,
                                run.style.font_size,
                            )
                            .at(x, baseline)
                            .color(run_color(run.style.color)),
                        );
                        x += run.width;
                    }
                    // 图片：移动端还没有解码 + 纹理上传的通路（桌面端那套挂在 gpui-kit
                    // 的 img 元素上），先只留位不画，后面的文字位置才不会串。
                    LineFragment::Image(img) => x += img.width,
                }
            }
        }
        out
    }
}

impl Render for ReaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        // 有效视口 = 窗口高 − 底部导航栏高。可见行筛选、滚动上限、`on_scroll` 的夹范围
        // 全都从这里取，所以底部那一截内容仍然滚得进来，也不会滚出多余空白。
        let viewport_height = (f32::from(viewport.height) - self.bottom_reserve).max(1.0);
        // 内容高度 / 视口任一变化后重新夹一次滚动范围（转屏、换字号都在这一刻生效）。
        let max_scroll = (self.content_height - viewport_height).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
        self.viewport_height = viewport_height;

        let mut element = GlyphsElement::new().background(paper());
        // `&mut self.source` 只借用这一个字段，所以臂里写 `self.content_height`、
        // 读 `self.scroll_y` 都不冲突（前提是别把 `self` 整个传进函数）。
        match &mut self.source {
            Source::Sample(text) => {
                let family = SharedString::from(self.prefs.font_family.clone());
                let font = mobile_font(
                    &FontKey {
                        family: String::new(),
                        weight: 400,
                        italic: false,
                    },
                    &family,
                );
                // 单行：左边距 `margin_x`，基线给到 `margin_x + font_size` 处。
                element.push(
                    GlyphRun::new(text.clone(), font, self.prefs.font_size)
                        .at(
                            self.prefs.margin_x,
                            self.prefs.margin_x + self.prefs.font_size,
                        )
                        .color(ink()),
                );
                self.content_height = 0.0;
            }
            Source::Document { page, .. } => {
                page.ensure_layout(window, cx);
                self.content_height = page.layout().height;
                let runs = Self::line_runs(page, page.layout(), self.scroll_y, viewport_height);
                element.extend(runs);
            }
        }

        let listener = cx.listener(|this: &mut Self, event: &ScrollWheelEvent, _window, cx| {
            this.on_scroll(event);
            cx.notify();
        });
        div()
            .id("folio-reader")
            .size_full()
            .overflow_hidden()
            .on_scroll_wheel(listener)
            .child(element)
    }
}

/// 读者的偏好，只把正文家族换成这台设备真的装了的那个。
fn reader_with_family(family: &str) -> Reader {
    let mut prefs = Reader::default().clamped();
    prefs.font_family = family.to_string();
    prefs
}

/// 挑一个这台设备**真的能解析出来**的正文家族。
///
/// 必须挑，不能照桌面那样直接把偏好名字交给 GPUI：`TextSystem::resolve_font`
/// 在请求家族和它整串回退名都不存在时是 **panic**（`gpui-pre-0.3.8/src/text_system.rs:370`），
/// 而读者的默认偏好 `LXGW WenKai` 只在开发机上装着，那份回退表又全是桌面名字
/// （`.ZedSans` / Helvetica / Segoe UI / Noto Sans / DejaVu Sans / Arial…），
/// 结果就是文档刚打开就 SIGABRT。Android 平台的字族是从 `/system/fonts/` 预载进来的。
///
/// 手上只有 `all_font_names()` 这一个公开判据（真正的 `font_id` 是私有的），所以按名字比。
/// 一个都没命中时把设备报出来的名字整体写进日志 —— 候选表该照数据加长，而不是照猜。
pub(crate) fn device_family(cx: &App) -> String {
    const PREFERRED: &[&str] = &[
        "LXGW WenKai",      // 随包字体：push 进应用目录后应当优先于系统的
        "Noto Sans CJK SC", // /system/fonts/NotoSansCJK-Regular.ttc 的简体那一面
        "Source Han Sans SC",
        "Noto Sans SC",
        "Microsoft YaHei",
        "PingFang SC",
        "Heiti SC",
        "Droid Sans Fallback", // Android 上覆盖 CJK 的传统兜底家族
        "Roboto",              // 只剩拉丁，但至少不 panic
    ];
    let names = cx.text_system().all_font_names();
    let wanted = Reader::default().font_family;
    let find = |want: &str| {
        names
            .iter()
            .find(|name| name.eq_ignore_ascii_case(want))
            .cloned()
    };

    if let Some(hit) = find(&wanted).or_else(|| PREFERRED.iter().find_map(|want| find(want))) {
        log::info!(
            "device_family: 用 {hit:?}（设备注册了 {} 个家族）",
            names.len()
        );
        return hit;
    }
    log::error!(
        "候选家族一个都没命中；设备注册的 {n} 个名字：{names:?}",
        n = names.len()
    );
    // 兜底也必须是一个真实存在的名字，否则下一次 resolve 还是 panic。
    names
        .into_iter()
        .next()
        .unwrap_or_else(|| wanted.to_string())
}

/// 冷启动时依次试探的文档位置。
///
/// 首选**应用专属外部目录** `/sdcard/Android/data/<包名>/files/`：它的属主就是本应用，
/// 所以不需要任何存储权限，`adb push` 也写得进去。`/sdcard/Download/` 排在后面而不是
/// 前面 —— 那里的文件属主是媒体提供者（另一个 uid），scoped storage 下我们的进程读到
/// 的是 EACCES，放对了文件名也不会出现画面。`FOLIO_MOBILE_DOC` 留给调试时指任意路径。
fn candidate_paths() -> Vec<PathBuf> {
    const PACKAGE: &str = "dev.folio.reader";
    let mut out = Vec::new();
    if let Ok(path) = std::env::var("FOLIO_MOBILE_DOC") {
        out.push(PathBuf::from(path));
    }
    for dir in [
        format!("/sdcard/Android/data/{PACKAGE}/files"),
        format!("/storage/emulated/0/Android/data/{PACKAGE}/files"),
        format!("/data/user/0/{PACKAGE}/files"),
        "/sdcard/Download".to_string(),
    ] {
        for name in ["folio-sample.md", "folio-sample.txt", "folio-sample.epub"] {
            out.push(PathBuf::from(format!("{dir}/{name}")));
        }
    }
    out
}
