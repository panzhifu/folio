//! 阅读视图：把会话的状态摆成元素——逐行画字形、把图片绝对定位、接住滚轮。
//!
//! 这里是**纯渲染**：排版不在这里（在 [`crate::ui::features::reader::document`]），
//! 状态与命令不在这里（在 [`crate::ui::features::reader::session`]）。`render` 只按当前
//! 模式挑出要摆的那几「片」，把排版行摊成 [`Scene`]，再挂上滚轮监听。
//!
//! 三种模式只差 [`Viewport`] 的那三个数，切片由核心分片器给出：
//!
//! - **连续滚动**：一片装完全部，位置是 `scroll_y` 这个像素数。手机上默认这一档。
//! - **分页**：一片 = 一个视口高、一页一栏，位置是页号。
//! - **双页对开**：一片 = 一栏、一页两栏，位置还是页号（一号就是「一张纸」）。
//!
//! 宽度装不下两栏时双页自动退回分页（判据在 [`chrome::PageMode::effective`]，按宽度而不是
//! 按 target），所以横屏的平板真能双页，竖屏手机不会长出两条窄柱子。
//!
//! 滚动**不自己做惯量**：`gpui-mobile` 的平台层已经把触摸翻译成了 `ScrollWheel` 事件
//! （`gpui-mobile/src/android/window.rs:1316` 起，用 `momentum::VelocityTracker` 估速度、
//! 用 `MomentumScroller` 在松手后每帧派发一次衰减增量），视图只要把事件里的 dy 累加到
//! `scroll_y`。在这里再套一个 `MomentumScroller` 会把同一份惯量算两遍，滚起来直接翻倍。

use folio_core::layout::FontKey;
use folio_core::layout::fragment::column_offset;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement, Render,
    ScrollWheelEvent, SharedString, Styled, TestSupportExt as _, Window, div, img, px,
};

use super::book_fonts::BookFonts;
use super::chrome;
use super::document::{Document, Viewport};
use super::paint::{GlyphRun, GlyphsElement};
use super::scene::{Scene, VisibleImage};
use super::session::{ReaderSession, Source};

impl Render for ReaderSession {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_prefs(cx);
        let viewport_size = window.viewport_size();
        // 有效视口 = 窗口高 − 外壳占的高 − 页眉 − 页脚。可见行筛选、滚动上限、
        // `on_scroll` 的夹范围全都从这里取，所以底部那一截内容仍然滚得进来，
        // 也不会滚出多余空白。浮层（顶栏/底栏/面板）压在正文**之上**，不参与这个数：
        // 点一下屏幕字不该跳走。
        let viewport_height = (f32::from(viewport_size.height)
            - self.reserve.height
            - self.header_reserve
            - self.footer_reserve)
            .max(1.0);
        let gap = chrome::spread_gap(self.prefs.margin_x);
        // 退档判据与排版必须量同一个版心宽，所以用 `Document` 的那个式子算，不在这里另定一套。
        let usable = Document::content_width(window, &self.prefs, self.reserve.width);
        let mode = self.mode.effective(usable, gap);
        let columns = mode.columns();

        let foreground = cx.theme().foreground;
        let family = self.family.clone();
        // 等宽偏好整帧取一次：兜底那一行与正文的 run 都要它。
        let mono = SharedString::from(self.prefs.mono_font_family.clone());
        // `FontKey → Font` 的解析每帧对每条可见 run 都要一次，交给会话持有的缓存
        // （避免逐 run 扫设备字体表）。body / mono / faces / 设备表变了由会话清或重建。
        let font_cache = &mut self.font_cache;
        let mut element = GlyphsElement::new().background(cx.theme().background);
        let mut column = div().size_full().relative();
        // `&mut self.source` 只借用这一个字段，所以臂里写 `self.content_height`、
        // 读 `self.scroll_y` 都不冲突（前提是别把 `self` 整个传进函数）。
        match &mut self.source {
            Source::Sample(text) => {
                let font = font_cache.resolve(
                    &FontKey {
                        family: String::new(),
                        families: Vec::new(),
                        weight: 400,
                        italic: false,
                    },
                    &family,
                    &mono,
                    // 兜底那一行没有书，也就没有内嵌字体。空表不分配，等于「没有这个功能」。
                    &BookFonts::empty(),
                );
                // 单行：左边距 `margin_x`，基线给到页眉下面 `margin_x + font_size` 处。
                element.push(
                    GlyphRun::new(text.clone(), font, self.prefs.font_size)
                        .at(
                            self.prefs.margin_x,
                            self.header_reserve + self.prefs.margin_x + self.prefs.font_size,
                        )
                        .color(foreground),
                );
                self.content_height = 0.0;
                self.laid_out.viewport = viewport_height;
            }
            Source::Book(book) => {
                let column_width = book.document_mut().ensure_viewport(
                    window,
                    cx,
                    Viewport {
                        reserve: self.reserve.width,
                        columns,
                        // 连续滚动 = 一片高 0：核心那边把 0 当「不分片」的哨兵值。
                        height: if mode.is_continuous() {
                            0.0
                        } else {
                            viewport_height
                        },
                        gap,
                    },
                );
                let pages = if mode.is_continuous() {
                    1
                } else {
                    book.document().page_count()
                };
                self.laid_out = super::session::LaidOut {
                    mode,
                    columns,
                    pages,
                    viewport: viewport_height,
                };

                // 上次读到第几个字 → 该摆在哪一像素 / 哪一页。只有排版做完了才映射得出，
                // 所以这一步在排版之后而不是构造时。
                if let Some(locator) = self.pending_restore.take() {
                    let line = book.document().line_at_offset(locator.offset);
                    if mode.is_continuous() {
                        self.scroll_y = book.document().y_at_offset(locator.offset);
                    } else {
                        self.page = book
                            .document()
                            .page_of_line(line)
                            .min(pages.saturating_sub(1));
                    }
                }
                // 内容高度 / 视口任一变化后重新夹一次落点（转屏、换字号都在这一刻生效）。
                if mode.is_continuous() {
                    self.content_height = book.document().height();
                    let max_scroll = (self.content_height - viewport_height).max(0.0);
                    self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
                } else {
                    self.page = self.page.min(pages.saturating_sub(1));
                }

                if mode.is_continuous() {
                    // 连续滚动：整章一条，靠 `scroll_y` 挑出可见行。
                    let scene = Scene::build(
                        book.document(),
                        font_cache,
                        &family,
                        0..book.document().line_count(),
                        self.scroll_y..(self.scroll_y + viewport_height),
                        self.scroll_y,
                        self.prefs.margin_x,
                        self.header_reserve,
                        foreground,
                    );
                    element.extend(scene.runs);
                    for image in scene.images {
                        column = column.child(image_child(image));
                    }
                } else {
                    // 分页 / 双页：一页取几片、每片一栏，摆在各自的栏位上。
                    let slices = book.document().slices();
                    for index in 0..columns {
                        let Some(slice) = slices.get(self.page * columns + index) else {
                            continue;
                        };
                        if slice.empty {
                            continue;
                        }
                        let x =
                            self.prefs.margin_x + column_offset(column_width, gap, slice.column);
                        let scene = Scene::build(
                            book.document(),
                            font_cache,
                            &family,
                            slice.lines.clone(),
                            slice.origin_y..(slice.origin_y + slice.height + 1.0),
                            slice.origin_y,
                            x,
                            self.header_reserve,
                            foreground,
                        );
                        element.extend(scene.runs);
                        for image in scene.images {
                            column = column.child(image_child(image));
                        }
                    }
                }
            }
        }
        column = column.child(element);

        let listener = cx.listener(|this: &mut Self, event: &ScrollWheelEvent, _window, cx| {
            this.on_scroll(event);
            cx.notify();
        });
        div()
            .id("folio-reader")
            // 宿主的测试要按 id 把滚轮事件投到这里。非 `test-support` 构建里这一层是透明的
            // （`Observed` 原样转交内部元素），所以真机上不多花一个节点。
            .test_support()
            .size_full()
            .overflow_hidden()
            .on_scroll_wheel(listener)
            .child(column)
    }
}

/// 一片里的一张图：绝对摆位交给 gpui 的 `img`。
fn image_child(image: VisibleImage) -> AnyElement {
    div()
        .absolute()
        .left(px(image.x))
        .top(px(image.top))
        .w(px(image.width))
        .h(px(image.height))
        .child(img(image.image).size_full())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    //! M1–M4 的验收门，现在挂在**统一的那一份**视图/会话上。
    //!
    //! 这批用例原来在 `desktop.rs`（虚拟列表那一套）里，`mobile.rs` 那边一条都没有 —— 于是
    //! 「手机上正文渲染成立」这件事靠的是真机截图，而桌面上那条回路红过也没人知道。
    //! 两端一份实现之后，这些断言测的就是真机上跑的那条代码路径。
    //!
    //! 用生产类型 `ReaderSession`，不在测试里另搭近似视图；滚动走真实滚轮事件，
    //! 因为直接写 `scroll_y` 会绕过要测的那段行为。
    //! 导入点名而不 `use super::*`：`gpui_kit` 根把 `gpui` 整个转了出来，里面有一个
    //! 叫 `test` 的属性宏，glob 会让下面的 `#[test]` 递归展开成它。

    use std::rc::Rc;
    use std::sync::Arc;

    use folio_core::document::Document;
    use folio_core::store::Store;
    use folio_core::{Chapter, Config, Content, DocMeta, FlowDoc, Format, Locator, Reader};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Entity, ScrollDelta, TestAppContext, point, px, size,
    };

    use crate::ui::features::reader::chrome::PageMode;
    use crate::ui::features::reader::session::ReaderSession;
    use crate::ui::shared::config_model::ConfigHolder;

    /// 一段足够长的中文，保证它会折行 —— 不折行的话「缩进改变了布局」这种断言无从成立。
    const PARA: &str = "于是我们奋力向前，逆水行舟，被不断地向后推，推入过去。";

    fn flow() -> FlowDoc {
        // `Chapter::source` 的契约是**引擎就绪的 XHTML**（纯文本的包装在 provider 里做完）。
        // 这里直接进排版层，所以自己按契约包好 —— 塞纯文本进来是测试自己违约，不是引擎该兜的。
        let source = (0..80)
            .map(|i| format!("<p>{PARA}（第 {i} 段）</p>"))
            .collect::<String>();
        FlowDoc {
            chapters: vec![Chapter {
                title: "第一章".into(),
                source,
                spine_href: String::new(),
            }],
            ..Default::default()
        }
    }

    fn meta(format: &str) -> DocMeta {
        DocMeta {
            title: "测试书".into(),
            authors: Vec::new(),
            format: Format::parse(format).expect("格式名要认得"),
            size: 0,
        }
    }

    fn prefs(size: f32) -> Reader {
        Reader {
            font_size: size,
            ..Reader::default()
        }
    }

    /// 开一扇窗、装好配置、把会话摆上去。`width` 决定这一帧能不能长出双页 ——
    /// 退档判据按宽度而不是按 target，所以窗口尺寸就是测试里唯一要控制的变量。
    fn open(
        cx: &mut TestAppContext,
        flow: &FlowDoc,
        format: &str,
        prefs: Reader,
        locator: Locator,
        width: f32,
    ) -> (Entity<ReaderSession>, AnyWindowHandle) {
        open_with_store(cx, flow, format, prefs, locator, width, None, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn open_with_store(
        cx: &mut TestAppContext,
        flow: &FlowDoc,
        format: &str,
        prefs: Reader,
        locator: Locator,
        width: f32,
        store: Option<Rc<Store>>,
        book_id: Option<i64>,
    ) -> (Entity<ReaderSession>, AnyWindowHandle) {
        let meta = meta(format);
        // 不开文件地造一本书：`from_content` 把章节收进统一文档接口，没有容器
        // （图片与内嵌字体都不来，与「容器丢了」同一个表现）。
        let doc: Arc<dyn Document> = Arc::from(folio_core::document::from_content(
            meta,
            Content::Reflow(flow.clone()),
        ));
        let mut view = None;
        let handle = cx.open_window(size(px(width), px(520.)), |window, cx| {
            // 偏好必须**装进配置**而不是只交给构造函数：会话每帧从 `ConfigHolder`
            // 同步一次偏好（读者在设置页改字号就是这条路），装默认配置就是把这里传的
            // `prefs` 当场冲掉 —— 而画面照样出得来，只有几何不对，最难查的那种红。
            let config = Config {
                reader: prefs.clone(),
                ..Default::default()
            };
            ConfigHolder::install(config, cx);
            let built = ReaderSession::with_document(doc, prefs, store, book_id, locator, cx);
            let created = cx.new(|_| built);
            view = Some(created.clone());
            Root::new(created, window, cx)
        });
        let view = view.expect("ReaderSession 应该被创建");
        settle(cx, handle.into());
        (view, handle.into())
    }

    /// 两轮「跑空事件 → 渲染一帧」。一轮不够：首帧只是把恢复请求挂上，
    /// 而 `run_until_parked` 只跑事件不画帧。框架自己的 `settle()` 也是这个形状。
    fn settle(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("渲染应成功");
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("第二帧应成功");
        cx.run_until_parked();
    }

    /// 真实滚轮事件投进正文。gpui 的约定是「向下滚为负」，所以往下滚给负的 dy。
    fn scroll_down(cx: &mut TestAppContext, handle: AnyWindowHandle, pixels: f32) {
        cx.update_window(handle, |_, window, cx| {
            window.scroll(
                "folio-reader",
                ScrollDelta::Pixels(point(px(0.0), px(-pixels))),
                cx,
            );
        })
        .expect("滚动应送达阅读视图");
        cx.run_until_parked();
    }

    fn progress_of(view: &Entity<ReaderSession>, cx: &mut TestAppContext) -> f32 {
        view.read_with(cx, |v, _| v.progress())
    }

    fn offset_of(view: &Entity<ReaderSession>, cx: &mut TestAppContext) -> usize {
        view.read_with(cx, |v, _| v.current_offset().unwrap_or_default())
    }

    /// 合成书：仓库里那本三章的 `minimal-zh.epub`。
    fn epub_flow() -> FlowDoc {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/minimal-zh.epub");
        let doc = crate::ui::features::reader::document::open(&path).expect("合成 EPUB 应能打开");
        doc.flow().expect("合成 EPUB 应读成可重排").clone()
    }

    #[gpui_kit::test]
    fn wheel_scrolling_moves_the_reading_position(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);

        assert_eq!(
            progress_of(&view, cx),
            0.0,
            "刚打开时应在章首，不该已有位移"
        );

        scroll_down(cx, handle, 900.0);
        let progress = progress_of(&view, cx);
        assert!(
            progress > 0.05,
            "真实滚轮事件应产生可测位移，实际比例 {progress:.3}"
        );
    }

    #[gpui_kit::test]
    fn locator_matches_the_scrolled_ratio(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let total = flow.chapters[0].char_len();
        assert!(total > 1000, "语料要够长才滚得动，实际 {total} 字");

        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
        assert_eq!(offset_of(&view, cx), 0, "未滚动时偏移应为 0");

        scroll_down(cx, handle, 900.0);
        let progress = progress_of(&view, cx);
        let offset = offset_of(&view, cx);

        assert!(offset > 0, "滚动后偏移应前进");
        assert!(offset < total, "偏移不应越出章尾");
        // 正文全是等高段落，所以「滚过的比例」与「读到的字数比例」该吻合。
        // 留一档余量：首行缩进与段距让行顶与字数不是严格线性。
        let ratio = offset as f32 / total as f32;
        assert!(
            (ratio - progress).abs() < 0.1,
            "偏移↔比例 换算不一致：offset {offset} / {total} = {ratio:.3}，progress = {progress:.3}"
        );
    }

    /// 「重启回到原处」里属于我们自己的那一半：位置必须能无损穿过 SQLite 回来，
    /// 连**当时那一档摆法**一起回来（进度存的是字偏移，模式只是它的读数方式）。
    #[gpui_kit::test]
    fn position_survives_a_restart_through_the_store(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);

        scroll_down(cx, handle, 900.0);
        let locator = Locator {
            chapter: 0,
            offset: offset_of(&view, cx),
        };
        assert!(locator.offset > 0, "滚动后应能算出字符偏移");

        let store = Rc::new(Store::in_memory().expect("内存库"));
        let id = store
            .add_book(std::path::Path::new("memory://fixture"), &meta("markdown"))
            .expect("入库");
        store.save_flow(id, &flow).expect("章节入库");
        store
            .set_position(id, PageMode::Continuous.storage_name(), &locator)
            .expect("写入进度");

        let (mode, restored) = store.position(id).expect("读回进度").expect("进度应存在");
        assert_eq!(mode, "continuous");
        assert_eq!(restored, locator, "进度穿过 SQLite 之后必须逐字符一致");

        // 用读回来的进度重开：恢复请求应在排版做完那一帧被消费掉，偏移落回同一个字。
        let (reopened, _) = open_with_store(
            cx,
            &flow,
            "markdown",
            prefs(16.0),
            restored,
            460.0,
            Some(store),
            Some(id),
        );
        assert!(
            !reopened.read_with(cx, |v, _| v.has_pending_restore()),
            "解析完成后恢复请求应被消费掉，而不是永远挂着"
        );
        assert_eq!(offset_of(&reopened, cx), restored.offset);
        assert!(progress_of(&reopened, cx) > 0.0, "重开之后正文真的滚出去了");
    }

    /// M2 的验收门：中文排版基础三件事必须由排版引擎产出。上一版在桌面上这些是
    /// `TextView` 画的，测到的是框架的行为；现在两端都是自绘，测的是引擎自己的行盒。
    #[gpui_kit::test]
    fn the_engine_indents_centers_and_spaces_lines(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = epub_flow();
        assert_eq!(flow.chapters.len(), 3, "spine 三章");

        let (view, _handle) = open(
            cx,
            &flow,
            "epub",
            Reader {
                font_size: 20.0,
                line_height: 1.8,
                ..Reader::default()
            },
            Locator::start(),
            460.0,
        );
        let lines = view.read_with(cx, |v, _| v.lines());
        let texts = view.read_with(cx, |v, _| v.line_texts());
        assert!(!lines.is_empty(), "引擎应产出行盒");

        // 第一章首个行盒是标题：UA 的 h1 = 2em 之外，书自己的 CSS 写了 1.8em 且居中。
        let title = &lines[0];
        assert_eq!(texts[0], "第一章 蛇结");
        assert!(
            (title.dominant_font_size() - 36.0).abs() < 0.5,
            "h1 = 1.8em × 20px，实际 {}",
            title.dominant_font_size()
        );
        assert!(
            title.x > 20.0,
            "居中标题应离开版心左缘，实际 x = {}",
            title.x
        );

        let body = lines
            .iter()
            .find(|l| l.dominant_font_size() == 20.0 && l.x > 0.0)
            .expect("应有一个首行被缩进的正文行");
        assert!(
            (body.x - 40.0).abs() < 1.0,
            "2em 缩进在 20px 字号下应是 40px，实际 {}",
            body.x
        );
        assert!(
            title.height > body.height * 1.5,
            "行盒高应随字号走，标题 {} vs 正文 {}",
            title.height,
            body.height
        );
        assert!(
            (body.height - 36.0).abs() < 0.5,
            "正文行高 = 20 × 1.8，实际 {}",
            body.height
        );

        // 长段落必须真的折了行：没有任何一行容得下整段原文，才算折过。
        let longest_paragraph = flow.chapters[0]
            .source
            .split('<')
            .map(|s| s.chars().filter(|c| !c.is_whitespace()).count())
            .max()
            .unwrap_or(0);
        let longest_line = texts.iter().map(|t| t.chars().count()).max().unwrap_or(0);
        assert!(
            longest_line < longest_paragraph && longest_line > 4,
            "段落应被折行：最长段 {longest_paragraph} 字，最长行只有 {longest_line} 字"
        );
        for pair in lines.windows(2) {
            assert!(
                pair[1].y >= pair[0].y + pair[0].height - 0.5,
                "行盒不能重叠：{} + {} > {}",
                pair[0].y,
                pair[0].height,
                pair[1].y
            );
        }
    }

    /// M3 的验收：一个段落里的局部字重、斜体与颜色必须各自成立。
    /// 这一条测的是**真实字体栈**上的多 run 度量与摆放，mock 测不到。
    #[gpui_kit::test]
    fn inline_styles_survive_into_runs(cx: &mut TestAppContext) {
        use folio_core::css::computed::Color;

        cx.update(gpui_kit::init);
        let flow = epub_flow();
        let (view, _handle) = open(
            cx,
            &flow,
            "epub",
            Reader::default(),
            Locator::start(),
            460.0,
        );
        let lines = view.read_with(cx, |v, _| v.lines());

        let rich = lines
            .iter()
            .find(|l| l.text_runs().count() >= 4)
            .expect("带 em/strong/span 的那一行应被切成多个 run");
        assert!(
            rich.text_runs().any(|r| r.style.font.weight >= 700),
            "strong 应带粗体"
        );
        assert!(rich.text_runs().any(|r| r.style.font.italic), "em 应带斜体");
        assert!(
            rich.text_runs()
                .any(|r| r.style.color == Some(Color(0xC0, 0x39, 0x2B))),
            "span.red 的局部颜色要落到 run 上"
        );
        assert!(
            rich.text_runs().all(|r| r.width > 0.0),
            "每个 run 都要有实际绘制宽度"
        );
        let sum: f32 = rich.text_runs().map(|r| r.width).sum();
        assert!(
            (sum - rich.width).abs() < 0.5,
            "行宽应是各 run 宽度之和：{sum} vs {}",
            rich.width
        );
    }

    /// M3-E3 验收：EPUB 章里的内嵌图要经排版引擎变成图片片段，`src` 按本章目录解析到
    /// 容器路径，且可见文本里不留哨兵字符。字节按需现取的这一层由 `BookImages` 管，
    /// 这里钉的是排版侧：解析成容器路径、带出固有尺寸。
    #[gpui_kit::test]
    fn images_flow_through_the_engine_as_fragments(cx: &mut TestAppContext) {
        use folio_core::format::ImageRes;

        cx.update(gpui_kit::init);
        let flow = FlowDoc {
            chapters: vec![Chapter {
                title: "图".into(),
                source: "<p>前<img src=\"../Images/pic.png\" alt=\"图\">后</p>".into(),
                spine_href: "OEBPS/Text/chap.xhtml".into(),
            }],
            images: vec![ImageRes {
                href: "OEBPS/Images/pic.png".into(),
                media_type: "image/png".into(),
                width: 40,
                height: 20,
            }],
            ..Default::default()
        };
        let (view, _handle) = open(
            cx,
            &flow,
            "epub",
            Reader::default(),
            Locator::start(),
            460.0,
        );
        let lines = view.read_with(cx, |v, _| v.lines());
        let texts = view.read_with(cx, |v, _| v.line_texts());

        assert_eq!(texts[0], "前后", "行内图不产出字符，哨兵要被去掉");
        let img = lines[0]
            .images()
            .next()
            .expect("../Images/pic.png 应解析成 OEBPS/Images/pic.png 并命中");
        assert_eq!(img.href, "OEBPS/Images/pic.png");
        assert_eq!((img.width, img.height), (40.0, 20.0));
    }

    /// M4 的门：坐标 → 行 → 落点 → 再反解回坐标，必须回到同一个字。
    /// 这一条与「进度落在第几个字上」整套设计同源，所以它红了就是锚点坐标系红了。
    #[gpui_kit::test]
    fn the_engine_progress_is_a_character_offset_that_comes_back(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = epub_flow();
        // 字号故意给到 60：这本合成书的第一章很短，默认字号下整章只有几行、比窗口还短，
        // 那一档下没有可滚的范围，测试就成了测空集。
        let prefs = prefs(60.0);
        let (view, handle) = open(cx, &flow, "epub", prefs.clone(), Locator::start(), 460.0);
        let lines = view.read_with(cx, |v, _| v.lines().len());
        assert!(lines > 4, "大字号下第一章该排出多行，实际 {lines}");
        // 跳转目标取「本章中点」。写死一个坐标的话，CI 上字体不同 → 行高不同 →
        // 可能落在第一行里，那条测试就变成测空集了。
        let total = view.read_with(cx, |v, _| v.text_length());
        assert!(total > 40, "第一章该有几十个字可跳，实际 {total}");
        let target = total / 2;
        // 章首那一行的坐标不是 0：pretty-print 过的 XHTML 里 `<body>` 与 `<p>` 之间的
        // 换行本身是文本节点，也进坐标系。
        let start = offset_of(&view, cx);
        assert!(start < 8, "刚打开该在章首附近，实际坐标 {start}");

        view.update(cx, |v, cx| v.jump_to_offset(target, cx));
        settle(cx, handle);

        let saved = offset_of(&view, cx);
        assert!(
            saved > start,
            "跳到中点坐标 {target} 之后章内偏移该往前走，实际 {start} → {saved}"
        );
        assert!(
            saved <= target,
            "落点该是覆盖 {target} 那一行的行首，不该越过它：{saved}"
        );
        assert!(
            target - saved < target / 4,
            "落点离目标 {target} 太远：{saved}"
        );
        let progress = progress_of(&view, cx);
        assert!(
            progress > 0.0 && progress < 1.0,
            "滚动比例该在 (0,1) 之间，实际 {progress}"
        );

        // 用同一个坐标重开：落回的行反解出来的偏移必须还是它。
        let (reopened, _) = open(
            cx,
            &flow,
            "epub",
            prefs,
            Locator {
                chapter: 0,
                offset: saved,
            },
            460.0,
        );
        assert!(
            !reopened.read_with(cx, |v, _| v.has_pending_restore()),
            "恢复请求该在排版做完的那一帧被消费掉"
        );
        assert_eq!(
            offset_of(&reopened, cx),
            saved,
            "坐标 → 行顶 → 再反解回坐标，必须回到同一个字"
        );
        assert!(
            progress_of(&reopened, cx) > 0.0,
            "重开之后正文真的滚出去了，不是只记了个数"
        );
    }

    /// 双页成立的那两条判据：一页确实是**两栏**，而且两栏首尾相接 —— 右栏的第一行
    /// 紧跟左栏的最后一行，读者按「先左后右」读不会跳行。
    #[gpui_kit::test]
    fn a_spread_is_two_contiguous_columns_of_one_sheet(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 1200.0);
        assert_eq!(
            view.read_with(cx, |v, _| v.mode()),
            PageMode::Continuous,
            "默认该是连续滚动，不是替读者决定"
        );

        view.update(cx, |v, cx| v.set_mode(PageMode::Spread, cx));
        settle(cx, handle);
        assert_eq!(
            view.read_with(cx, |v, _| v.mode()),
            PageMode::Spread,
            "1200 宽的窗口该放得下两栏"
        );
        let pages = view.read_with(cx, |v, _| v.page_count());
        assert!(pages > 2, "长一章双页也该有多页，实际 {pages}");

        let left = view
            .read_with(cx, |v, _| v.sheet_range(0, 0))
            .expect("第 0 页左栏应有内容");
        let right = view
            .read_with(cx, |v, _| v.sheet_range(0, 1))
            .expect("第 0 页右栏应有内容");
        assert!(!left.is_empty() && !right.is_empty(), "两栏都要有行");
        assert_eq!(
            left.end, right.start,
            "右栏该从左栏断的那一行接着排，不能重叠也不能留缝"
        );

        // 一页一号：翻一页 = 翻一张纸（两栏一起走），偏移该往前走并且不超过两栏的行数。
        let before = offset_of(&view, cx);
        view.update(cx, |v, cx| v.scroll_page(1, cx));
        settle(cx, handle);
        assert_eq!(view.read_with(cx, |v, _| v.page()), 1);
        let after = offset_of(&view, cx);
        assert!(after > before, "翻一张纸该往前走，实际 {before} → {after}");
    }

    /// 窄窗口点「双页」不该得到两条窄柱子：按宽度退回分页。
    /// 这条判据按宽度而不是按 target，所以横屏平板能真双页、竖屏手机自动单页。
    #[gpui_kit::test]
    fn a_narrow_window_falls_back_from_spread_to_one_column(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
        view.update(cx, |v, cx| v.set_mode(PageMode::Spread, cx));
        settle(cx, handle);

        assert_eq!(
            view.read_with(cx, |v, _| v.mode()),
            PageMode::Paged,
            "460 宽放不下两栏，该退回分页"
        );
        // 读者**选**的仍然是双页：按钮的选中态读它，不然一退档那格就自己弹回去了。
        assert_eq!(view.read_with(cx, |v, _| v.wanted_mode()), PageMode::Spread);
        // 只有一栏，所以第 1 栏不存在。
        assert!(
            view.read_with(cx, |v, _| v.sheet_range(0, 1)).is_none(),
            "退档之后不该还剩一条空栏"
        );
    }

    /// 换摆法不换位置：连续滚到某个字，切双页（宽窗）再切回来，还在同一个字上。
    /// 进度是「第几个字」，模式只是它的读数方式 —— 这条要是红了，读者的位置就会在
    /// 点一下「双页」时飘掉。
    #[gpui_kit::test]
    fn switching_modes_keeps_the_same_character(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 1200.0);
        scroll_down(cx, handle, 1200.0);
        let wanted = offset_of(&view, cx);
        assert!(wanted > 0, "先滚到一个字上，实际 {wanted}");

        view.update(cx, |v, cx| v.set_mode(PageMode::Spread, cx));
        settle(cx, handle);
        assert_eq!(view.read_with(cx, |v, _| v.mode()), PageMode::Spread);
        let in_spread = offset_of(&view, cx);
        // 落点是「覆盖那个字的那张纸的开头」，所以它只能等于或早于原位置，
        // 且不能早过一页之内的距离。
        assert!(
            in_spread <= wanted,
            "落位不该越过原来那一行：{wanted} → {in_spread}"
        );

        view.update(cx, |v, cx| v.set_mode(PageMode::Continuous, cx));
        settle(cx, handle);
        assert_eq!(
            offset_of(&view, cx),
            in_spread,
            "切回连续滚动该落在双页那一屏的开头那个字上"
        );
    }
}
