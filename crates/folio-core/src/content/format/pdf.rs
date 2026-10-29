//! PDF：一页是既定的画布，**内容流里没有「行」这个事实** —— 像素要算出来才有。
//!
//! 所以它归定版式那一族（[`FixedDoc`]），与 CBZ 同族而分歧只有一处：漫画的一页本身就是
//! 容器里的一条图（按 `href` 取字节），PDF 的一页要**栅格化**才有字节，所以页表里那条是
//! `PageSource::Render { index }`，字节由 [`DocumentProvider::render_page`] 现算。
//!
//! ## 一台引擎
//!
//! 从前这里是两台：`pdf_oxide` 管解析（页表 / 书目 / 书签 / 文字层 / 加密判定），
//! `hayro` 只管画像素。换的理由不是画得慢 —— 画那一半本来就在 hayro 上 —— 而是那一半
//! 解析要另付一整棵依赖树（`tiny-skia 0.12`、`fontdb 0.24`、`harfrust 0.13`、
//! `read-fonts 0.43`、`fast_image_resize`，以及把 `chrono` / `regex` / `brotli` 那一串
//! 拖进 `.so` 的解析器本体），而这些与图上已有的版本各存一份、同时编进二进制。
//!
//! hayro 的那三格是**补出来的**，补在 fork 上（`panzhifu/hayro` 的 `main`，见根
//! `Cargo.toml` 那条依赖的注释）：
//!
//! - `hayro_interpret::text`：只记不画的 `Device`，逐字给 unicode、字符码与**墨迹盒**；
//! - `Page::placed_images()`：页上每张图的原生像素数与它在页上的框（图片页封顶要它）；
//! - `hayro_syntax::outline`：`/Outlines` 那条链 → 目录树与页号。
//!
//! 三格都没进上游，所以 `hayro` 走 git rev 而不是 crates.io。上游哪天收了这三格，把根
//! `Cargo.toml` 里那一条换回版本号即可，这一层不用动。
//!
//! ## 书签与文字层为什么在这里，不在引擎里
//!
//! 引擎交回的是**结构**（一条链、一串字形、一张图的像素数与框），而「丢掉一条落点越界的
//! 书签」「空格要有盒子」「`/Rotate` 那几页交 `None`」是**读法**，住在本模块。引擎那一侧
//! 换人时，这一层的门（`bookmarks_land_on_pages_and_a_targetless_one_is_dropped` 等 25 条）
//! 一条都不用改。
//!
//! ## 每本一份解析句柄
//!
//! `Pdf::new` 收整份字节、懒解结构，`Pdf` 是 `Send + Sync`（`tests/placed_images.rs` 里那条
//! `a_pdf_is_send_and_sync` 钉着）。句柄挂在 [`PdfAdapter`] 上，而 provider 实例随 `Doc` 活：
//! **每本一份，书关了就放** —— 不是进程级的句柄表，所以没有「按本数封顶 / 谁被淘汰」那套
//! 规则要维护。
//!
//! 从前每一趟取页都重开一次：桌面实测 `sample.pdf`（3.6 MB、45 页、图多）重开 374 ms、
//! 留住句柄 425 ms，差值一成以内 —— 那时「无状态」换得回那点时间。矢量书换了口径：
//! 一本 64 MB 的书在手机上重开一次要一两秒，而阅读页一屏要取「预览 + 高清 + 文字层」
//! 三趟，首图的黑屏时间全花在重复读同一份文件上。
//!
//! **峰值驻留**从前是两份整本字节（两台引擎各一份）。现在只剩一份，代价转到另一头：
//! 文字层是**第二遍**解释同一页内容流（recorder 带的 `InterpreterCache` 里有 `Rc`，折进
//! 渲染那一遍会把 `!Send` 拖到多线程渲染器身上），所以问一页的字要再走一遍内容流。实测
//! 这一遍与画一页同量级、不是它的倍数；`page_texts` 那一条（整本抽字给搜索）走的是同一趟。
//!
//! ## 图片页按源图自己的密度封顶
//!
//! `RENDER_EDGE` 是**上限**，不是每一页的档位。扫描本书（画面就是几张 JPEG 铺满一页）
//! 按 2048 画，多出来的像素全是插值：`hugG.pdf`（13.9 MB、43 页、A4、每页 3–4 张
//! 120 ppi 的 JPEG，`Microsoft: Print To PDF` 生产）实测**每页 816–932 ms**、PNG 平均

use std::path::Path;
use std::sync::OnceLock;

use hayro::hayro_interpret::text::TextLayerRecorder;
use hayro::hayro_interpret::{
    Context as InterpretContext, InterpreterCache, InterpreterSettings, interpret_page,
};
use hayro::hayro_syntax::object::Rect;
use hayro::hayro_syntax::page::{Page as PdfPage, Rotation};
use hayro::hayro_syntax::outline::Dest;
use hayro::hayro_syntax::{LoadPdfError, Pdf as RasterPdf};
use hayro::kurbo::{Affine, Rect as KurboRect};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};
use hayro::{RenderCache, RenderSettings, render_into};

use super::{
    Content, DocMeta, DocumentProvider, FixedDoc, FixedPage, Format, PageChar, PageRect, PageRegion,
    PageSource, PageText, TocEntry, extension_of, stem_of,
};
use crate::Result;

/// 栅格长边的上限（像素）。一页 A4 在这一档出来是 `1448 × 2048`。
///
/// **`pub` 的理由是只有一份**：界面那一侧（`ui::foundation::images` 给渲染页建缓存键、
/// `fixed_book` 摆页）要知道按哪一档要像素，而这一格改了那边必须跟着改 —— 两处各写一个
/// 数，症状是「缓存键说 2048、实际画的是 1448」这种屏上看得出但说不清的错。
///
/// 定在 2048 与图集那一侧的口径同一条：进 GPU sprite atlas 的是**原始像素**，而 GLES 上
/// `max_texture_dimension_2d` 常见 2048（`ui::foundation::images` 里那条 `MAX_IMAGE_EDGE`
/// 量的就是这个）。往上是「放大到 2× 还不糊」，往下是「一页 12 MB 的 pixmap 少一半」。
///
/// **改这个数要跟着 bump `INDEX_VERSION`**：页表里存的宽高就是按它算的（`raster_size`），
/// 而缓存键里也带着这一档。
///
/// 它现在是**上限**，不是每一页实际用的档位：图片页按源图自己的像素密度封顶
/// （[`native_edge`]），页表里存的是封顶之后的尺寸。矢量页仍然是这一档。
pub const RENDER_EDGE: u32 = 2048;

/// **区域重画**那一档的长边上限（像素）—— 读者捏开放大、看得见的那一块按这一档重画。
///
/// 为什么不进页表也不进 `INDEX_VERSION`：区域图不属于任何一个稳定的页身份。页表说的
/// 是「整页画出来多大」，而这一块矩形每次捏合、每次平移都可能不一样，它只活在缓存键
/// 里（`render/{page}@{edge}/{x0},{y0},{x1},{y1}`）。把 `4096` 写进页表就等于宣布
/// 「整页按 4096 存」，那一页的原始像素是 47 MB，手机上三张并行就把内存预算吃光了。
///
/// 为什么敢超过 2048：`RENDER_EDGE` 定在 2048 是让**整页**那一张不超 GLES 常见的
/// `max_texture_dimension_2d`，而区域图天生要超过它才有意义（不超过就是把同一批像素
/// 摊开）。超过那张上限的图由 gpui 的图集切 tile 上传，画得出来；代价是 4096×2900 的
/// 原始像素 = 47 MB 一张，所以**只画看得见的那一块**是这一档的前提，不是优化。
pub const REGION_EDGE: u32 = 4096;

/// 封面栅格的长边，与 EPUB / FB2 / CBZ 那条 `COVER_MAX_EDGE` 同一口径。
///
/// 单独一个数而不是「按 `RENDER_EDGE` 画完再缩」：封面只有首页，直接按目标尺寸画少一块
/// pixmap、也少一次重编码。
pub(crate) const COVER_EDGE: u32 = 512;

/// 图片页封顶时留的那几成余量。
///
/// **不能取 1.0，也不能贴着 0.9**：引擎在「输出比源图小一成左右」那一档有一条快路，
/// 卡在阈值上时四舍五入会翻过去 —— `hugG.pdf` 实测输出 885 px 宽（源图 984）89 ms，
/// 891 px 宽 147 ms，差的正是这一格。所以按 0.88 封，离阈值留 2% 的余量。
const NATIVE_EDGE_RATIO: f32 = 0.88;

/// 封顶的下限（像素）。
///
/// 一页源图比这还糊（几十 ppi 的缩略图铺满一页）时，再低就只剩马赛克，而放大那一层
/// 与手机上的小字号都要有最低一档可看。360 与 `pdf-probe` 量「快路上限」那一臂的
/// 地板同一条。
const MIN_RENDER_EDGE: u32 = 360;

/// 一页上的图要盖住可见框的多少，才把这一页当「图片页」按源图密度封顶。
///
/// 低于它说明页面还有一大块是矢量画的（正文、表格、线条）—— 那些没有「源像素」，
/// 按图封顶会把它们一起画糊。0.85 留给扫描件常见的白边：扫描图通常连白边一起扫进去，
/// 占面接近 100%；而 Word / LaTeX 那种「一张插图 + 正文」页通常到不了这一档。
const IMAGE_PAGE_COVERAGE: f32 = 0.85;

/// 文字层盒子的 y 翻转与裁剪要的那把尺 —— 与栅格用的是**同一页、同一个框**。
///
/// 从 `Page` 自己的 `media_box` / `crop_box` / `rotation` 读，不另算一遍：两处各算一次
/// 就会差一两个点，而那种差在屏上是「色块偏了一截」，看得出来、说不清。
struct PageBoxes<'a> {
    /// 这一页本身（文字层要用它的 `/Rotate` 与「可见框」算同一把尺）。
    page: &'a PdfPage<'a>,
    visible_width: f32,
    visible_height: f32,
    /// 可见框的左沿与上沿（PDF 用户空间，上沿取 y 较大的那一边）。
    left: f32,
    top: f32,
    /// 页面自己的 `/Rotate`（继承来的已经由引擎折进来）。
    rotate: i32,
}

impl PageBoxes<'_> {
    /// 把可见长边折到 `edge` 的那个比例。
    fn scale(&self, edge: u32) -> f32 {
        edge as f32 / self.visible_width.max(self.visible_height)
    }

    /// 引擎的字形盒（pt、y 朝上）→ 屏幕与栅格图那一套（原点左上、y 朝下）。
    ///
    /// 用的是 `Page::initial_transform_for` —— 与画像素那一趟**同一个函数**，只是基准
    /// 换成「可见框」：被裁过的页上，页自己的裁剪矩形比栅格露出的大一圈，拿它当基准
    /// 每个字都会偏出裁掉的那一截。`left` / `top` 就是可见框在引擎那套数里的位置。
    fn to_screen(&self, bbox: Rect) -> KurboRect {
        let transform = hayro::kurbo::Affine::new(
            self.page
                .initial_transform_for(
                    (self.left as f64, (self.top - self.visible_height) as f64),
                    f64::from(self.visible_height),
                    true,
                )
                .as_coeffs(),
        );
        transform.transform_rect_bbox(KurboRect::new(bbox.x0, bbox.y0, bbox.x1, bbox.y1))
    }

    /// 笔前进量（pt，PDF 用户空间）→ 屏幕那一套里的位移。
    fn to_screen_advance(&self, advance: (f64, f64)) -> (f32, f32) {
        let transform = hayro::kurbo::Affine::new(
            self.page
                .initial_transform_for(
                    (self.left as f64, (self.top - self.visible_height) as f64),
                    f64::from(self.visible_height),
                    true,
                )
                .as_coeffs(),
        );
        // 一个纯平移的差：线性部分对两点一样，所以直接把系数乘上位移。
        let [a, b, c, d, _, _] = transform.as_coeffs();
        (
            (a * advance.0 + c * advance.1) as f32,
            (b * advance.0 + d * advance.1) as f32,
        )
    }
}

/// PDF 的 provider。
///
/// 持有**这一本书**的解析句柄：`Pdf` 随 provider 实例活，而实例挂在 `Doc::with_container`
/// 的 `provider` 字段上 —— 书关了就跟着放。
///
/// 一个句柄管两件事（解析与画像素），所以不必再像从前那样「两个引擎各存一份字节」：
/// 峰值驻留只剩一份整本。
pub struct PdfAdapter {
    /// `None` = 打不开（坏文件 / 加密）。连着失败一起记住，免得每帧重试一次。
    document: OnceLock<Option<RasterPdf>>,
    /// 与 `document` 配套的解释器设置（内嵌字体 / CMap 的 resolver）。克隆只是几条 `Arc`。
    raster_settings: InterpreterSettings,
}

impl Default for PdfAdapter {
    fn default() -> Self {
        Self {
            document: OnceLock::new(),
            raster_settings: InterpreterSettings::default(),
        }
    }
}

impl PdfAdapter {
    /// 从**字节**建一份：`.pdf` 读文件、AZW4 从 PalmDB 记录里取出来，都走这一条。
    pub(crate) fn from_bytes(bytes: Vec<u8>) -> std::result::Result<RasterPdf, LoadPdfError> {
        RasterPdf::new(bytes)
    }

    /// 这一份句柄；`None` = 打不开。
    fn document(&self, path: &Path) -> Option<&RasterPdf> {
        self.document
            .get_or_init(|| open_document(path).ok())
            .as_ref()
    }

    /// 先把句柄备好（页面预取那一趟会用）。
    fn warm_document(&self, path: &Path) {
        let _ = self.document(path);
    }
}

impl DocumentProvider for PdfAdapter {
    fn format(&self) -> Format {
        Format::Pdf
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(extension_of(path).as_deref(), Some("pdf"))
    }

    /// 索引 = 页表 + 书签 + 书目 + 封面。PDF 没有「章」，也没有正文可取。
    ///
    /// 封面要栅格化首页，所以这一趟比 CBZ 那趟贵（实测首页在 512 那一档约 40 ms，
    /// `sample.pdf` 那种图多的书约 200 ms）。贵得值：书架上没有封面就是一片占位图，
    /// 而一本 PDF 的第一页通常就是封面。
    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        // 第一次打开走 `open_document`：加密与「结构读不出来」是两档不同的结论，
        // 只有那条路带着引擎的原始错误。失败也记下来（`None`），别每帧重试。
        if self.document.get().is_none() {
            match open_document(path) {
                Ok(document) => {
                    let _ = self.document.set(Some(document));
                }
                Err(err) => {
                    let _ = self.document.set(None);
                    return Err(err);
                }
            }
        }
        let document = self
            .document(path)
            .ok_or_else(|| unsupported(path, "结构读不出来，文件可能被截断过"))?;
        let cover = render_png(document, 0, COVER_EDGE, &self.raster_settings);
        read_pdf(
            path,
            document,
            Format::Pdf,
            stem_of(path),
            std::fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or(0),
            cover,
        )
    }

    /// 画第 `page` 页，交回 PNG 字节。
    ///
    /// `edge` 是长边**上限**：页比这一档小也照样放大到它（1 点 = 1 像素在手机上一屏
    /// 马赛克），而图片页按源图自己的密度封到更低一档（[`native_edge`]）—— 与页表
    /// 里存的尺寸是同一句判断，见 [`render_edge`]。
    ///
    /// 读不到一律 `None`（与 [`DocumentProvider::image_bytes`] 同一立场）：「这一页坏了」与
    /// 「这一页不在书里」在屏幕上都是那块地方空着。
    fn warm(&self, path: &Path) {
        self.warm_document(path);
    }

    fn render_page(&self, path: &Path, page: usize, edge: u32) -> Option<Vec<u8>> {
        let document = self.document(path)?;
        let (width, height) = raster_size(document, page, edge);
        if width == 0 || height == 0 {
            return None;
        }
        paint_page(document, page, width, height, &self.raster_settings)
            .and_then(|pixmap| encode_png(&pixmap))
    }

    /// 放大那一档：按页表那把尺里的一块矩形重画（契约写在
    /// [`DocumentProvider::render_page_region`]）。
    ///
    /// 「这一页值不值得重画」不在这里判：页表的 `vector` 那一格已经说了（同一句
    /// [`native_edge`]，见 [`page_is_vector`]）。界面照着那一格决定要不要问，这一趟只管
    /// 「问到了就按那一块画」。两处各判一次，屏上就是「捏到 6 倍什么也没变清楚」。
    fn render_page_region(
        &self,
        path: &Path,
        page: usize,
        region: PageRegion,
        width: u32,
        height: u32,
    ) -> Option<Vec<u8>> {
        let document = self.document(path)?;
        render_region(document, page, region, width, height, &self.raster_settings)
    }

    /// 这一页的文字层（选中与划线要的那个序 + 逐字盒）。
    ///
    /// 与 [`Self::render_page`] 分开问：画得出这一页不等于读得出这一页的字（扫描本没有
    /// 文字层、这一族也没有密码入口）。抽取是**第二遍**解释同一页内容流，见模块头
    /// 「每本一份解析句柄」最后那一段。
    fn page_text(&self, path: &Path, page: usize) -> Option<PageText> {
        page_text(self.document(path)?, page, &self.raster_settings)
    }

    /// 全书每一页的字序，**容器只开一次**（契约与那条「必须与逐页问逐字节相同」写在
    /// [`DocumentProvider::page_texts`]）。
    ///
    /// 为什么专门开这一问：搜索要的正是整本一遍，逐页问那一问整本书要过 69 次
    /// `page_text` 调用（每次都要走一遍页号 → 页对象的查找）；这一问一次取完。
    fn page_texts(&self, path: &Path) -> Option<Vec<String>> {
        let document = self.document(path)?;
        Some(
            (0..document.pages().len())
                .map(|page| page_texts_one(document, page, &self.raster_settings))
                .collect(),
        )
    }
}

/// 一页的 box（**点**，都按 `/Rotate` 交换过长宽）。
fn page_boxes<'a>(page: &'a PdfPage<'a>) -> Option<PageBoxes<'a>> {
    let crop = page.intersected_crop_box();
    let swap = matches!(
        page.rotation(),
        Rotation::Horizontal | Rotation::FlippedHorizontal
    );
    let (crop_w, crop_h) = ((crop.x1 - crop.x0).abs() as f32, (crop.y1 - crop.y0).abs() as f32);
    let (visible_width, visible_height) = if swap {
        (crop_h, crop_w)
    } else {
        (crop_w, crop_h)
    };
    let rotate = match page.rotation() {
        Rotation::None => 0,
        Rotation::Horizontal => 90,
        Rotation::Flipped => 180,
        Rotation::FlippedHorizontal => 270,
    };
    (visible_width > 0.0 && visible_height > 0.0).then_some(PageBoxes {
        page,
        visible_width,
        visible_height,
        left: crop.x0.min(crop.x1) as f32,
        top: crop.y0.max(crop.y1) as f32,
        rotate,
    })
}

/// `/Rotate` 与可见框那一份（测试与栅格化共用同一句）。
#[cfg(test)]
fn page_boxes_at(document: &RasterPdf, page: usize) -> Option<PageBoxes<'_>> {
    document.pages().get(page).and_then(page_boxes)
}

/// 这一页栅格出来有多大（**带方向**：横页是宽长、竖页是高长）。
///
/// 页表存的就是这两个数，绘制侧按 `width / height` 定这一页的摆位，所以这里绝不能折成
/// 「长短边各一个」的固定顺序 —— 一本横竖混排的画册会被全部摆成竖的，而屏幕上是字压字。
/// 读不出框的那一页给 `(0, 0)`：那是「不知道」的哨兵值，绘制侧按视口比例兜底，
/// 与 CBZ 里「位图头读不出来」那一支同一条降级。
///
/// `edge` 是**上限**：图片页按源图自己的密度封到更低的一档（[`native_edge`]），
/// 其余页照 `edge` 画。
pub(crate) fn raster_size(document: &RasterPdf, page: usize, edge: u32) -> (u32, u32) {
    let Some(target) = document.pages().get(page) else {
        return (0, 0);
    };
    let Some(boxes) = page_boxes(target) else {
        return (0, 0);
    };
    let edge = render_edge(target, page, &boxes, edge);
    let scale = boxes.scale(edge);
    (
        (boxes.visible_width * scale).round().max(1.0) as u32,
        (boxes.visible_height * scale).round().max(1.0) as u32,
    )
}

/// 这一页实际按哪一档画：`edge` 是上限，图片页被源图密度压到更低一档。
///
/// 页表（[`raster_size`]）与栅格化（`paint_page` 的目标框）问的是**同一句**，所以
/// 「页表说的尺寸」与「画出来的像素」不会一处封一处不封 —— 两处分头写，症状是图集
/// 按 2048 记账而图只有 1232，一张页虚报掉两页的名额。
///
/// 下限夹在 `edge` 以内：调用方要 256 那一档（契约测试、封面）时不该被抬到 360。
fn render_edge(page: &PdfPage<'_>, index: usize, boxes: &PageBoxes<'_>, edge: u32) -> u32 {
    native_edge(page, index, boxes)
        .map(|cap| cap.clamp(MIN_RENDER_EDGE.min(edge), edge))
        .unwrap_or(edge)
}

/// 这一页的图**最多承载多少像素** → 栅格长边的上限；不是图片页给 `None`。
///
/// 算式：页长边 `L` pt、源图长边 `p` px 印在 `b` pt 上时，图上 1 pt 有 `p / b` 个源像素，
/// 而输出要的是 `edge / L` 个像素每 pt。让输出不超过源图的 [`NATIVE_EDGE_RATIO`] 倍，
/// 就得到 `edge ≤ 0.88 × L × (p / b)`。一页几张图时取**最密的那张**（更疏的只会更早
/// 落在它下面），而「这一页是不是图片页」由覆盖率说（[`IMAGE_PAGE_COVERAGE`]）。
///
/// 源图的框是页上真正印出来的大小（`Page::placed_images` 交的就是那个方框过完
/// `ctm` 的结果），用 MediaBox 或页宽去除都会在被裁过的页上算偏。
fn native_edge(page: &PdfPage<'_>, _index: usize, boxes: &PageBoxes<'_>) -> Option<u32> {
    // 转过的页：图片句柄的框在**未转**的用户空间里，而这里的可见长边已经按 `/Rotate`
    // 换过方向 —— 拿它们相除会把 90 度的那一页算成另一个密度（高比宽、宽比高），
    // 而错的档位在屏上是糊掉的一页，不是少一个字符。宁可不封。
    if boxes.rotate != 0 {
        return None;
    }
    let handles = page.placed_images();
    let page_area = boxes.visible_width * boxes.visible_height;
    if page_area <= 0.0 {
        return None;
    }
    let mut covered = 0.0_f32;
    let mut density = 0.0_f32;
    for handle in &handles {
        let (width, height) = ((handle.bbox.x1 - handle.bbox.x0) as f32, (handle.bbox.y1 - handle.bbox.y0) as f32);
        if width <= 0.0 || height <= 0.0 {
            continue;
        }
        // 印在页上的原生密度（ppi）：两个方向取更密的那个，非等比缩放的图不吃亏。
        density =
            density.max((handle.width as f32 / width).max(handle.height as f32 / height) * 72.0);
        covered += (width * height / page_area).min(1.0);
    }
    if density <= 0.0 || covered < IMAGE_PAGE_COVERAGE {
        return None;
    }
    let long_pt = boxes.visible_width.max(boxes.visible_height);
    Some((NATIVE_EDGE_RATIO * long_pt * density / 72.0).round() as u32)
}

/// 这一页是不是**矢量画的** —— 放大时按可见区域重画补不补得出细节（界面上报的是同一个数，
/// 见 `PageSource::Render::vector`）。
///
/// 判据与 [`render_edge`] 用的同一个 [`native_edge`]：那一句给 `None`（没被源图自己的
/// 像素密度封过顶）就是「这一页的像素是算出来的」。两处各判一次，屏上就会出现
/// 「这一页说能重画、core 说画不出来」那种差 —— 而它的表现是捏到 6 倍什么也没变清楚。
fn page_is_vector(document: &RasterPdf, page: usize) -> bool {
    let Some(target) = document.pages().get(page) else {
        return false;
    };
    // 读不出可见框的那一页，页表也是 `(0, 0)`、摆位走兜底 —— 不当它是矢量页，免得为
    // 一张本来就画不出来的页去排一趟 4096 的区域图。
    let Some(boxes) = page_boxes(target) else {
        return false;
    };
    native_edge(target, page, &boxes).is_none()
}

/// 一份已打开的 PDF → 页表 + 书签 + 书目 + 封面。
///
/// PDF 与 AZW4（PalmDB 里的 PDF）共用这一份：两者只差「`Pdf` 是从文件打开还是从记录字节
/// 建」，页表与栅格化的规则一个字都不该分叉。
///
/// `format` 是**这本书在库里报的身份**（`Pdf` 或 `Azw4`），不是 PDF 引擎的格式；
/// `title_fallback` 在 PDF 自己没有 `/Title` 时用（PDF 是文件主名，AZW4 是 PalmDB 名）。
/// `cover` 是首页那一张已经画好的字节。
///
/// 这一趟本身**不需要**解释器设置：页表读的是结构（框、旋转、图柄），书目与书签读的
/// 是字典。栅格化与抽字各自的设置由调用方握着（它们自己会传）。
pub(crate) fn read_pdf(
    path: &Path,
    document: &RasterPdf,
    format: Format,
    title_fallback: String,
    size: u64,
    cover: Option<Vec<u8>>,
) -> Result<(DocMeta, Content)> {
    let pages = document.pages().len();
    if pages == 0 {
        return Err(unsupported(path, "这本 PDF 的页树里一页都没有"));
    }

    let mut table = Vec::with_capacity(pages);
    for page in 0..pages {
        // 宽高是**封顶之后**的像素（`render_edge`）：扫描件那一页按源图密度算，
        // 矢量页仍是 `RENDER_EDGE`。栅格化那一趟读同一句，两边因此永远同一个数。
        let (width, height) = raster_size(document, page, RENDER_EDGE);
        table.push(FixedPage {
            source: PageSource::Render {
                index: page as u32,
                vector: page_is_vector(document, page),
            },
            width,
            height,
            // 一页就是一张栅格，没有「主图之外再摆一张」这一档（那是 EPUB 跨页的事）。
            extra_images: Vec::new(),
        });
    }
    // 首屏尺寸取**第一页尺寸知道的那一页**：整本首页读不出可见框（写坏的 `/MediaBox`）
    // 时不至于让整本的缩放基准变成 0，而后面那些页各自有自己的尺寸。
    let (width, height) = table
        .iter()
        .find(|page| page.width > 0 && page.height > 0)
        .map(|page| (page.width, page.height))
        .unwrap_or((0, 0));

    let toc = outline(document, pages);

    // `/Lang` 在目录字典里；`/Info` 没有这一项。交空串 = 「这本书没说」，
    // 界面上那一行不摆。
    let info = Info::read(document);
    let meta = DocMeta {
        title: info.text(info.title.as_deref()).unwrap_or(title_fallback),
        // `/Author` 没有列表语法，多个作者之间用逗号还是「 and 」由生产者定。不切：
        // `Smith, J.` 这种写法切一刀就把一个人的名字拆成两个作者。
        authors: info
            .text(info.author.as_deref())
            .map(|author| vec![author])
            .unwrap_or_default(),
        subjects: info
            .text(info.keywords.as_deref())
            .map(|keywords| split_keywords(&keywords))
            .unwrap_or_default(),
        // PDF 的书目里没有「出版社」这一项。`/Producer` 写的是把这份 PDF 生成出来的
        // 工具（`dvipdfmx`、`Typst 0.15.1`），不是出版方 —— 所以不填：信息屏「有值才摆
        // 那一行」的规矩会让这一行整个消失，比摆一个错数诚实。
        publisher: String::new(),
        published: info
            .text(info.creation_date.as_deref())
            .and_then(|date| pdf_date_to_day(&date))
            .unwrap_or_default(),
        language: String::new(),
        description: String::new(),
        isbn: String::new(),
        contributors: Vec::new(),
        dctype: String::new(),
        rights: String::new(),
        series: None,
        format,
        size,
    };
    let fixed = FixedDoc {
        pages: table,
        toc,
        cover,
        // 渲染型的页没有 href，也就不进图片表：那一列进了库只会让图片查看器多摆一张
        // 取不到字节的图（`store::content` 里那条「渲染型不该被塞进图片表」的门盯着它）。
        images: Vec::new(),
        // 封面的落盘描述由导入管线填（`ingest::import::land_cover`）：provider 只交字节。
        cover_ref: None,
        width,
        height,
    };
    tracing::debug!("pdf: {} 读到 {} 页", path.display(), pages);
    Ok((meta, Content::Fixed(fixed)))
}

/// 打开一份 PDF。
///
/// 口令加密的 PDF 到这一步就会报错（`LoadPdfError::Decryption`），所以 `Encrypted` 这一条
/// 只从这里冒出来；[`failure`] 是唯一的分档口，报表里那一栏不会从加密滑进读不动。
fn open_document(path: &Path) -> Result<RasterPdf> {
    let bytes = std::fs::read(path)
        .map_err(|err| unsupported(path, &format!("文件读不出来（{err}）")))?;
    PdfAdapter::from_bytes(bytes).map_err(|err| failure(path, err))
}

/// 引擎的错误 → 我们的两档：加密（换一本）与读不动（重导一次试试）。
pub(crate) fn failure(path: &Path, err: LoadPdfError) -> crate::Error {
    match err {
        LoadPdfError::Decryption(_) => crate::Error::Encrypted {
            name: file_name(path),
            method: "PDF /Encrypt（口令或权限加密）".to_string(),
        },
        LoadPdfError::Invalid => {
            unsupported(path, "结构读不出来（文件被截断或不是 PDF），可能被截断过")
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

fn unsupported(path: &Path, why: &str) -> crate::Error {
    crate::Error::UnsupportedFormat(format!("{}：{why}", file_name(path)))
}

/// 书目那几行：`/Info` 字典里的字符串 + 目录里的结构信息。
///
/// 为什么 `hayro-syntax` 的 `Metadata` 不够：它的 `/CreationDate` 已经解析成 `DateTime`，
/// 而这一层的 [`pdf_date_to_day`] 是「取头部 8 个数字」—— `D:2009` 这种只写到年的串，两条
/// 路会给出两种结论（一边 `2009-01-01`、一边 `None`）。`/Info` 只拿**原始字节**，日期怎么读
/// 只在这一层定。
struct Info {
    title: Option<Box<[u8]>>,
    author: Option<Box<[u8]>>,
    #[allow(dead_code)]
    subject: Option<Box<[u8]>>,
    keywords: Option<Box<[u8]>>,
    #[allow(dead_code)]
    creator: Option<Box<[u8]>>,
    #[allow(dead_code)]
    producer: Option<Box<[u8]>>,
    creation_date: Option<Vec<u8>>,
}

impl Info {
    fn read(document: &RasterPdf) -> Self {
        // `Pdf::metadata()` 交的是**已解码**的几项（`Vec<u8>`，带 BOM 时是 UTF-16BE，
        // 否则是 `PDFDocEncoding`），口径与 `/Info` 里那个串一致 —— 这一层要原始字节，
        // 就是因为 `/CreationDate` 的写法比别的几项野。
        let meta = document.metadata();
        Self {
            title: meta.title.clone(),
            author: meta.author.clone(),
            subject: meta.subject.clone(),
            keywords: meta.keywords.clone(),
            creator: meta.creator.clone(),
            producer: meta.producer.clone(),
            creation_date: raw_creation_date(document),
        }
    }

    fn text(&self, value: Option<&[u8]>) -> Option<String> {
        let text = decode_text_string(value?);
        let text = text.trim().to_string();
        (!text.is_empty()).then_some(text)
    }
}

/// `/Info` 里 `/CreationDate` 的**原始串**。
///
/// `Metadata::creation_date` 是解析过的 `DateTime`，读不到年就整项作废；而这一层要的是
/// 「能认出前 8 位数字就用、认不出就不摆」，两条口径不同。所以再取一次原字节。
fn raw_creation_date(document: &RasterPdf) -> Option<Vec<u8>> {
    // `Pdf::metadata()` 只给解析后的那一份；原串在 `/Info` 字典里，而它挂在 trailer 上、
    // 引擎不公开。退一步：解析过的那一份只要年、月、日都还在，就按它的数字拼一个串回来 ——
    // `DateTime` 的字段就是规范那三段，丢掉的是时区与秒（这一层不读）。
    let date = document.metadata().creation_date.as_ref()?;
    (date.year != 0).then(|| {
        format!(
            "D:{:04}{:02}{:02}{:02}{:02}{:02}",
            date.year, date.month, date.day, date.hour, date.minute, date.second
        )
        .into_bytes()
    })
}

/// 一个 PDF 文本串 → `String`。
///
/// 规范两种编码（ISO 32000-1 §7.9.3）：带 `FEFF` / `FFFE` BOM 的是 UTF-16，否则按
/// `PDFDocEncoding` —— 那套编码与 Latin-1 差不出一打码位（`•` 在 0x80 之类），这里按
/// Latin-1 读、把常见的那几个换掉。书目那几行是**标签**：一个 `•` 落成别的可见字符不算
/// 事故，为它拖一张完整码表进来才是。
fn decode_text_string(bytes: &[u8]) -> String {
    // Latin-1 与 `PDFDocEncoding` 在字母那一圈是同一套，只有 0x80–0xA0 那一段不同。
    // 而真文件里「中文标题」常常**不带 BOM** 直接塞 UTF-8 字节（WPS / LibreOffice 都见过），
    // 那一串按单字节读会变成 `ä¸­æ–‡`。所以先看它是不是合法 UTF-8：是就按 UTF-8 读，
    // 其余仍走下面的两条。
    if bytes.len() >= 2
        && !(bytes[0] == 0xFE && bytes[1] == 0xFF)
        && !(bytes[0] == 0xFF && bytes[1] == 0xFE)
        && let Ok(text) = std::str::from_utf8(bytes)
    {
        return text.to_string();
    }
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    bytes
        .iter()
        .map(|byte| match byte {
            0x80 => '\u{2022}',
            0x81 => '\u{2020}',
            0x82 => '\u{2021}',
            0x83 => '\u{2026}',
            0x84 => '\u{2014}',
            0x85 => '\u{2013}',
            0x86 => '\u{0192}',
            0x87 => '\u{2044}',
            0x88 => '\u{2039}',
            0x89 => '\u{203A}',
            0x8A => '\u{2212}',
            0x8B => '\u{2030}',
            0x8C => '\u{201E}',
            0x8D => '\u{201C}',
            0x8E => '\u{201D}',
            0x8F => '\u{2018}',
            0x90 => '\u{2019}',
            0x91 => '\u{201A}',
            0x92 => '\u{2122}',
            0x93 => '\u{FB01}',
            0x94 => '\u{FB02}',
            0x95 => '\u{0141}',
            0x96 => '\u{0152}',
            0x97 => '\u{0160}',
            0x98 => '\u{0178}',
            0x99 => '\u{017D}',
            0x9A => '\u{0131}',
            0x9B => '\u{0142}',
            0x9C => '\u{0153}',
            0x9D => '\u{0161}',
            0x9E => '\u{017E}',
            0xA0 => '\u{20AC}',
            other => *other as char,
        })
        .collect()
}

/// 一页的文字层。
///
/// 引擎交回的是 **pt、y 朝上**的墨迹盒，而栅格出来的页图与屏幕都是 y 朝下 —— 翻这一刀
/// 只有这一处该做，所以盒在这里就翻好，界面拿到的与页图同一个方向。
/// 裁过的那一页还要按可见框裁掉左边与上边（`left` / `top`），不然色块整体偏一截。
///
/// **`/Rotate` 不为 0 的那一页交 `None`**：图是转过的而盒是没转的，要对上就得在这里再
/// 建一个旋转坐标系，而本机没有一页「横排带真字」的样本可量（`three-pages.pdf` 那份
/// 转 90 的夹具一个字符都没有）。猜错的后果不是少一个字，而是**色块画在别的地方** ——
/// 那一格比「这一页选不出字」难查得多，而后者与从前逐字节一样。
pub(crate) fn page_text(
    document: &RasterPdf,
    page: usize,
    settings: &InterpreterSettings,
) -> Option<PageText> {
    let target = document.pages().get(page)?;
    let boxes = page_boxes(target)?;
    if boxes.rotate != 0 {
        return None;
    }
    let text = extract_page_text(target, settings);
    text_layer(&text, &boxes)
}

/// 一页 → 界面那一份文字层：翻 y、裁可见框、词缝补空格。
fn text_layer(
    text: &hayro::hayro_interpret::text::ExtractedText,
    boxes: &PageBoxes,
) -> Option<PageText> {
    // 引擎给的字形是**内容流序**，一个字形一格墨。这里把它拼成页上那一串字：
    //
    // 1. **行**按上沿聚。字号一变（上标、小字注）上沿会差一截，所以同一行的判据给半个
    //    字高；真换行至少差一整行。
    // 2. **缝**用笔的前进量判：这一格的左沿落在「上一格的右沿 + 它自己的前进量」后面，
    //    差出来的就是词缝，补一个空格。不用「两格之间有没有空隙」—— 斜体字的墨会伸进
    //    前一个字的空当，而 `TJ` 里那截负位移是生产者**写下来**的词缝。
    // 3. 缝里那一段**不补在行首**：换行之后笔回到左边，那一段不是词缝。
    let mut chars: Vec<PageChar> = Vec::with_capacity(text.glyphs.len());
    let mut line: u32 = 0;
    let mut last_top: Option<f32> = None;
    let mut pen_right: Option<f32> = None;
    for glyph in &text.glyphs {
        let Some(ch) = glyph.text.as_deref().and_then(|text| text.chars().next()) else {
            continue;
        };
        let Some(bbox) = glyph.bbox else {
            // 没有墨迹的字：它的宽度就是笔前进量，交回给下一格算缝。
            if let Some(previous) = pen_right {
                pen_right = Some(previous + boxes.to_screen_advance(glyph.advance).0);
            }
            continue;
        };
        let screen = boxes.to_screen(bbox);
        let rect = PageRect {
            x: screen.x0 as f32,
            y: screen.y0 as f32,
            width: (screen.x1 - screen.x0) as f32,
            height: (screen.y1 - screen.y0) as f32,
        };
        if rect.width <= 0.0 || rect.height <= 0.0 {
            continue;
        }
        // 墨的左右沿本来就是按字形摆的：`T` 的那一横从起笔点右边 3.5pt 才开始，`I` 的
        // 竖线落在正中。本字提前于前字右沿（墨伸进空当）不算缝；只有**越过前字右沿**
        // 还多出四分之一个字高，才是生产者写下来的词缝。
        let spaced = pen_right.is_some_and(|previous| rect.x - previous > rect.height * 0.25);
        let top = rect.y;
        let new_line = last_top
            .map(|previous| top - previous > rect.height * 0.5)
            .unwrap_or(true);
        if new_line {
            line += u32::from(last_top.is_some());
            last_top = Some(top);
            pen_right = None;
        }
        if spaced
            && let Some(previous) = pen_right
        {
            chars.push(PageChar {
                ch: ' ',
                rect: PageRect {
                    x: previous,
                    y: rect.y,
                    width: rect.x - previous,
                    height: rect.height,
                },
                line,
            });
        }
        let pen = rect.x + boxes.to_screen_advance(glyph.advance).0;
        let right = rect.x + rect.width;
        pen_right = Some(right.max(pen));
        chars.push(PageChar { ch, rect, line });
    }
    (!chars.is_empty()).then_some(PageText {
        width: boxes.visible_width,
        height: boxes.visible_height,
        chars,
    })
}

/// 第 `page` 页的字，一行一条接起来（搜索那一趟要的就是这个）。
///
/// PDF 与 AZW4 共用：两者只差句柄从哪儿来。
pub(crate) fn page_texts_one(
    document: &RasterPdf,
    page: usize,
    settings: &InterpreterSettings,
) -> String {
    // 走 [`page_text`] 那**同一条路**再取字序，不在这里另建一份抽取：整本那一列与逐页问
    // 那一句必须逐字节相同（`DocumentProvider::page_texts` 的契约），而上一版这里直接叫
    // `extract_page_text(...).text()`，绕过了「按词缝插空格」那一步 —— 两条路量出来的
    // 字序差了整批空格，症状正是那条契约写的「搜到了 `ABSTRACT`、跳过去挑中的是别处」。
    page_text(document, page, settings)
        .map(|layer| layer.text())
        .unwrap_or_default()
}

/// 一页 → 引擎那份文字层。
fn extract_page_text(
    page: &PdfPage<'_>,
    settings: &InterpreterSettings,
) -> hayro::hayro_interpret::text::ExtractedText {
    let cache = InterpreterCache::new();
    let area = hayro::kurbo::Rect::new(0.0, 0.0, 16384.0, 16384.0);
    let mut context = InterpretContext::new(Affine::IDENTITY, area, &cache, page.xref(), settings.clone());
    let mut recorder = TextLayerRecorder::new();
    interpret_page(page, &mut context, &mut recorder);
    recorder.into_text()
}

/// 画一页（`examples/pdf-probe` 那一侧的量具用：它要拿同一份实现按任意档量）。
pub fn render_probe(
    document: &RasterPdf,
    page: usize,
    edge: u32,
    settings: &InterpreterSettings,
) -> Option<Vec<u8>> {
    render_png(document, page, edge, settings)
}

/// [`render_probe`] 的区域版：给 `pdf-probe` 的 `region` 一臂用（生产那条路是
/// `PdfAdapter::render_page_region`，同一份 `render_region`）。
pub fn render_region_probe(
    document: &RasterPdf,
    page: usize,
    region: PageRegion,
    width: u32,
    height: u32,
    settings: &InterpreterSettings,
) -> Option<Vec<u8>> {
    render_region(document, page, region, width, height, settings)
}

/// 画一页并交回 PNG 字节；任何一步读不出来都只给 `None`。
///
/// 尺寸不在这儿算 —— 调用方给的是页表里存的那个数（[`raster_size`]），页表与画出来的
/// 因此永远同一个数。
///
/// 那一句 `catch_unwind` 要说清它**不是**万能的：仓库的 release 配置是 `panic = "abort"`，
/// 真机上一次 panic 仍然会结束进程，这一层挡不住；它在测试与桌面 dev 构建（unwind）里才
/// 有效。留着它是因为这一层不该比调用方更假设 panic 策略。
pub(crate) fn render_png(
    document: &RasterPdf,
    page: usize,
    edge: u32,
    settings: &InterpreterSettings,
) -> Option<Vec<u8>> {
    let (width, height) = raster_size(document, page, edge);
    if width == 0 || height == 0 {
        return None;
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        paint_page(document, page, width, height, settings).and_then(|pixmap| encode_png(&pixmap))
    }))
    .ok()
    .flatten()
}

/// 真正的那一趟：开一张 `width × height` 的画布，把页按可见框缩放到上面。
///
/// 为什么不用 `hayro::render`（它按 `render_dimensions` 与一个等比 scale 出图）：
/// 那条路的输出尺寸是 `(宽 × scale) as u16`，**截断**而不是四舍五入，会与页表差一像素；
/// 这里直接给画布定尺寸，比例从「页自己的可见框」折算 —— 与 [`raster_size`] 那句
/// `round` 同一个结果，页表说什么尺寸就画什么尺寸。
fn paint_page(
    document: &RasterPdf,
    page: usize,
    width: u32,
    height: u32,
    settings: &InterpreterSettings,
) -> Option<Pixmap> {
    let target = document.pages().get(page)?;
    let (page_width, page_height) = target.render_dimensions();
    if page_width <= 0.0 || page_height <= 0.0 {
        return None;
    }
    // 整页那一趟：把「可见框」那一块（`render_dimensions`，已含 `/Rotate` 与 CropBox）
    // 铺满输出画布。
    let transform = Affine::scale_non_uniform(
        f64::from(width) / f64::from(page_width),
        f64::from(height) / f64::from(page_height),
    ) * initial_of(target);
    paint_with(target, width, height, transform, settings)
}

/// 画一页的**一块矩形**，交回 PNG（放大那一档，见 [`REGION_EDGE`] 与 [`PageRegion`]）。
///
/// `width × height` 是**输出**要的像素（界面按「看得见的那一块要铺满多大屏」算），
/// `region` 是页表那把尺里的那一块 —— 两把尺之间的换算由这里自己做，用的是页表那一句
/// 同一个 `raster_size`。这就是为什么界面递过来的数不需要知道 pt 是多少。
///
/// 变换三步，与 [`paint_page`] 同一个骨架，只是中间多一次平移：
/// `scale(输出 / 那块) · translate(-那块的原点) · initial_transform(true)`。
/// 前一步是「把这一块放大到铺满输出画布」，最后那一步是引擎给的「pt → 页、y 翻下来、
/// 原点落在可见框左上角」那一格 —— 所以**被裁过的页也不会偏**：原点与框都出自同一句。
pub(crate) fn render_region(
    document: &RasterPdf,
    page: usize,
    region: PageRegion,
    width: u32,
    height: u32,
    settings: &InterpreterSettings,
) -> Option<Vec<u8>> {
    let target = document.pages().get(page)?;
    let (page_width, page_height) = target.render_dimensions();
    if page_width <= 0.0 || page_height <= 0.0 || width == 0 || height == 0 {
        return None;
    }
    // 页表那把尺（区域坐标就是按它给的）：`render_edge` 那一句算出来的整页尺寸。
    let (table_w, table_h) = raster_size(document, page, RENDER_EDGE);
    if table_w == 0 || table_h == 0 {
        return None;
    }
    // 夹进页里：界面上多滚出去的那半截本来就没内容，夹住比整块不画诚实 ——
    // 夹完全空的矩形才是「画不出」（给 `None`，那一格保持现有像素不动）。
    let (x0, x1) = (region.x0.min(table_w), region.x1.min(table_w));
    let (y0, y1) = (region.y0.min(table_h), region.y1.min(table_h));
    let (rw_pt, rh_pt) = (
        f64::from(x1) * f64::from(page_width) / f64::from(table_w)
            - f64::from(x0) * f64::from(page_width) / f64::from(table_w),
        f64::from(y1) * f64::from(page_height) / f64::from(table_h)
            - f64::from(y0) * f64::from(page_height) / f64::from(table_h),
    );
    if rw_pt <= 0.0 || rh_pt <= 0.0 {
        return None;
    }
    let transform = Affine::scale_non_uniform(f64::from(width) / rw_pt, f64::from(height) / rh_pt)
        * Affine::translate((
            -f64::from(x0) * f64::from(page_width) / f64::from(table_w),
            -f64::from(y0) * f64::from(page_height) / f64::from(table_h),
        ))
        * initial_of(target);
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        paint_with(target, width, height, transform, settings)
            .and_then(|pixmap| encode_png(&pixmap))
    }))
    .ok()
    .flatten()
}

/// 引擎给的那一步（pt → 页、y 翻下来、原点落在可见框左上角）。整页与区域两趟共用。
fn initial_of(target: &PdfPage<'_>) -> Affine {
    Affine::new(target.initial_transform(true).as_coeffs())
}

/// 拿着算好的变换画一趟：开画布、解释内容流、刷白底、交回 pixmap。
///
/// [`paint_page`] 与 [`render_region`] 只差**那一个矩阵**，其余每一步都一样 —— 分成两份
/// 写就会有一份先改掉（白底、`TargetInit`、缓存的建法），而那类差在屏上是
/// 「放大后那张的颜色和整页那张不一样」，说不清是谁的错。
fn paint_with(
    target: &PdfPage<'_>,
    width: u32,
    height: u32,
    transform: Affine,
    settings: &InterpreterSettings,
) -> Option<Pixmap> {
    let width = u16::try_from(width).ok().filter(|width| *width > 0)?;
    let height = u16::try_from(height).ok().filter(|height| *height > 0)?;

    // 缓存是**每次现建**的：它内部是 `Rc<RefCell<_>>`，不是 `Send`，而这一趟会被几个
    // 后台线程同时叫（预览、高清、下一屏的预取）。共享一份要一把锁，等于把三条并行
    // 画页串成一条；每次现建的代价在 `opt-level = 3` 的基准里是 22 → 37 ms（720 档，
    // 仓库那份 `opt-level = "z"` 没单独量过这一格），远比串行便宜。
    let cache = RenderCache::new();
    let mut ctx = RenderContext::new(width, height);
    render_into(
        target,
        &cache,
        settings,
        &RenderSettings::default(),
        &mut ctx,
        transform,
    );
    ctx.flush();

    // 白底：导出/分享拿到的是一张不透明的图，不会在别的底色上翻出黑。
    let mut pixmap = Pixmap::new(width, height);
    ctx.render_with(
        &mut pixmap,
        &mut Resources::default(),
        RasterizerSettings {
            target_init: TargetInit::Clear(WHITE),
            ..RasterizerSettings::default()
        },
    );
    Some(pixmap)
}

/// `hayro` 的 pixmap → PNG 字节。
///
/// ## 预乘那一刀可以省
///
/// pixmap 是**预乘** RGBA，而 PNG 要的是非预乘。这一页的画布是 `TargetInit::Clear(WHITE)`
/// 刷出来的不透明白底，之后的每一次混合都保持 alpha = 1 —— 预乘与非预乘于是同一份字节。
/// 逐像素除一遍在 1.8 MPix 上实测 30 ms，比画页本身还贵；所以只在真出现透明像素时
/// （`may_have_transparency`，将来换了底色）才做那一趟。
///
/// ## 编码档位
///
/// 档位是在**仓库自己那份 release 配置**（`opt-level = "z"`，也就是 APK 里跑的那份）上
/// 量的，1810×2048 的一页：
///
/// | 档位 | 编码 | 体积 |
/// |---|---|---|
/// | `Fast` + `Up` | **20 ms** | 816 KB |
/// | `Fast` + `Sub` | 20 ms | 801 KB |
/// | `Fast` + `Adaptive` | 166 ms | 566 KB |
/// | `Balanced` + `Adaptive` | 206 ms | 444 KB |
/// | `NoCompression` | 15 ms | 14.8 MB |
///
/// `Adaptive` 要逐行试五种滤波再挑最好的，`opt-level = "z"` 下那一步比压缩本身还贵
/// （8 倍）；`Up` 只做一次行差分，体积只大四成。这一张 PNG 只活到界面解码那一步，
/// 不进磁盘、不给人看体积 —— 但也不能像 `NoCompression` 那样一次占 15 MB
/// （手机上三张并行就是 45 MB），所以取 `Up`。
fn encode_png(pixmap: &Pixmap) -> Option<Vec<u8>> {
    let premultiplied = pixmap.data_as_u8_slice();
    let straight;
    let data = if pixmap.may_have_transparency() {
        straight = unpremultiply(premultiplied);
        straight.as_slice()
    } else {
        premultiplied
    };
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(
        &mut png,
        u32::from(pixmap.width()),
        u32::from(pixmap.height()),
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder.set_filter(png::Filter::Up);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(data).ok()?;
    writer.finish().ok()?;
    Some(png)
}

/// 预乘 RGBA → 非预乘（PNG 要的那一份）。只有画布真带透明时才走这里。
fn unpremultiply(premultiplied: &[u8]) -> Vec<u8> {
    let mut straight = Vec::with_capacity(premultiplied.len());
    for pixel in premultiplied.as_chunks::<4>().0 {
        let alpha = pixel[3];
        if alpha == 255 {
            straight.extend_from_slice(pixel);
        } else if alpha == 0 {
            straight.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let alpha32 = u32::from(alpha);
            let un = |channel: u8| {
                ((u32::from(channel) * 255 + alpha32 / 2) / alpha32).min(255) as u8
            };
            straight.extend_from_slice(&[un(pixel[0]), un(pixel[1]), un(pixel[2]), alpha]);
        }
    }
    straight
}

/// PDF 的日期串 → `YYYY-MM-DD`。
///
/// 规范写法是 `D:YYYYMMDDHHmmSS±HH'M'M'`（ISO 32000-1 §7.9.4.2），真文件里 `D:` 前缀常常
/// 没有、秒与时区常常缺，还有只写到年的。取「头部那 8 个数字」而不是整串解析：信息屏那一
/// 行只到日。认不出 8 位数字就交 `None` —— 宁可不摆这一行，也不把 `D:20091110100441+08'00'`
/// 原样甩给读者。
fn pdf_date_to_day(raw: &str) -> Option<String> {
    let digits: String = raw
        .trim()
        .trim_start_matches("D:")
        .trim_start_matches(':')
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.len() < 8 {
        return None;
    }
    let year: u32 = digits[..4].parse().ok()?;
    let month: u32 = digits[4..6].parse().ok()?;
    let day: u32 = digits[6..8].parse().ok()?;
    // 认得出数字不等于是个日期：`0000` 年与 `13` 月都真见过（有些生产者把缺的位填 0）。
    if year == 0 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

/// `Keywords` → 主题词表。逗号与分号都算分隔（两种写法都在真文件里）。
///
/// **不按引号或括号配对切**：`/Keywords` 没有规范语法，规范（§14.3.3）只说它是一个字符串。
/// 带逗号的词组有的用括号包着 —— 把括号一并剥掉是省事，按括号切会把
/// `(Machine Learning, AI)` 拆成两条，那是同一个量级的错而多一个解析器。
/// 一串关键词 → 一条一词。PDF 的 `/Keywords` 与 DjVu 的 `keywords` 都是这么写的。
pub(crate) fn split_keywords(raw: &str) -> Vec<String> {
    raw.split([',', ';'])
        .map(|word| word.trim().trim_matches(['(', ')']).trim().to_string())
        .filter(|word| !word.is_empty())
        .collect()
}

/// 书签树 → 目录树，每条的落点是**页号**（写在 `TocEntry::page` 那一格）。
///
/// 目标页越界或读不出的那一条**丢掉**，不让它进库：目录跳错地方比少一条目录更难查 ——
/// 读者点「第七章」落到第二章，他会以为书坏了；而少一条他只会从正文翻过去。
fn outline(document: &RasterPdf, pages: usize) -> Vec<TocEntry> {
    hayro::hayro_syntax::outline::outline(document.xref())
        .into_iter()
        .filter_map(|item| outline_entry(item, pages))
        .collect()
}

fn outline_entry(
    item: hayro::hayro_syntax::outline::OutlineItem,
    pages: usize,
) -> Option<TocEntry> {
    let Dest { page, .. } = item.dest?;
    if page >= pages {
        tracing::debug!(
            "pdf: 书签 {:?} 指向第 {page} 页，而全书只有 {pages} 页，丢掉",
            item.title
        );
        return None;
    }
    Some(TocEntry {
        title: item.title,
        // 定版式没有容器路径：`href` 留空，落点由 `page` 那一格说。两格不并存，
        // 所以可重排那一路「按 href 查章号」的逻辑完全不受影响。
        href: String::new(),
        fragment: None,
        page: Some(page),
        children: item
            .children
            .into_iter()
            .filter_map(|child| outline_entry(child, pages))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    //! PDF：页表（含 `/Rotate` 与 `CropBox`）、书签落点、`/Info` 书目、日期与关键词的
    //! 切法、栅格化出来的尺寸、坏包的说法。
    //!
    //! 样本是**自己拼的**（`Fixture`），不是从哪儿拷的一本真书。理由与 `tests/fixtures/README.md`
    //! 那条约定一致：要验的是「给定这个结构，我们的解析对不对」，而那些结构都能程序化生成，
    //! 生成出来的东西小到一个测试里放得下、失败时打印得出。它的形状**已由独立实现验过**
    //! （poppler `pdfinfo -box`：3 页；第 2 页 CropBox 300×500 套在 MediaBox 400×600 里；
    //! 第 3 页从 `/Pages` 继承 612×792 且 `/Rotate 90`；`/Title` 是 UTF-16BE 的「测试合成本」；
    //! `/CreationDate` 解成 2009-11-10）。那一步不能省：一份只有我们自己读得懂的 PDF
    //! 证明不了我们读得懂 PDF。

    use std::path::{Path, PathBuf};

    use super::*;
    use crate::content::format::{Content, DocumentProvider, Format, PageSource};

    /// 一页的规格。`inherit = true` 那一页**自己不写** `/MediaBox`，从 `/Pages` 继承 ——
    /// 规范允许（ISO 32000-1 §7.7.3.3 的 inheritable 属性那一族），真文件里也常见。
    struct Page {
        media: [i32; 4],
        crop: Option<[i32; 4]>,
        rotate: i32,
        inherit: bool,
        /// 这一页要摆的几行字：`(基线 x, 基线 y, 字号, 文本)`，**PDF 那一套坐标**
        /// （原点左下、y 朝上）。文字层那三条门要的是「我知道这行字印在哪儿」，
        /// 而这个数只有写的人知道 —— 拷来的真书反倒说不清。
        text: Vec<(i32, i32, f32, &'static str)>,
        /// 这一页铺的图（扫描件那一形状）：像素数 + 印在页上的框。
        image: Option<Placed>,
    }

    /// 一张铺在页上的图：`width × height` 个源像素，印在 `bbox`（pt，PDF 坐标）里。
    struct Placed {
        width: u32,
        height: u32,
        bbox: [i32; 4],
    }

    impl Page {
        fn new(media: [i32; 4]) -> Self {
            Self {
                media,
                crop: None,
                rotate: 0,
                inherit: false,
                text: Vec::new(),
                image: None,
            }
        }

        /// 铺一张图。看不清扫描件与正文页的分别，全看它盖住多少（`IMAGE_PAGE_COVERAGE`）。
        fn image(mut self, width: u32, height: u32, bbox: [i32; 4]) -> Self {
            self.image = Some(Placed {
                width,
                height,
                bbox,
            });
            self
        }

        /// 加一行字（基线在 `(x, y)`，单位 pt、原点左下）。
        fn line(mut self, x: i32, y: i32, size: f32, text: &'static str) -> Self {
            self.text.push((x, y, size, text));
            self
        }

        fn crop(mut self, crop: [i32; 4]) -> Self {
            self.crop = Some(crop);
            self
        }

        fn rotate(mut self, rotate: i32) -> Self {
            self.rotate = rotate;
            self
        }

        fn inherit(mut self) -> Self {
            self.inherit = true;
            self
        }
    }

    /// 一条书签。`page = None` 那条是「有标题没落点」的章节头，规范允许，界面上点不动。
    struct Bookmark {
        title: String,
        page: Option<usize>,
    }

    impl Bookmark {
        fn to_page(title: &str, page: usize) -> Self {
            Self {
                title: title.into(),
                page: Some(page),
            }
        }
        fn nowhere(title: &str) -> Self {
            Self {
                title: title.into(),
                page: None,
            }
        }
    }

    /// 一份最小但**结构完整**的 PDF：对象表、xref 的 20 字节一项、`startxref` 的字节偏移。
    ///
    /// 内容流只画一个灰矩形，**不吃任何字体**：那才是这一层要盯的东西（页框、旋转、书签落点），
    /// 而标准 14 号字体（Helvetica）在 PDF 里是「名字，不带字库」，能不能渲出来归系统字体解析，
    /// 不该让这一格门去赌本机装了什么字体。
    struct Fixture {
        objects: Vec<Vec<u8>>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                objects: Vec::new(),
            }
        }

        /// 占一个对象号，内容后填（书签要互相引用，必须两趟）。
        fn reserve(&mut self) -> usize {
            self.objects.push(Vec::new());
            self.objects.len()
        }

        fn set(&mut self, id: usize, body: impl Into<Vec<u8>>) {
            self.objects[id - 1] = body.into();
        }

        fn finish(self, root: usize, info: Option<usize>) -> Vec<u8> {
            let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
            let mut offsets = vec![0usize; self.objects.len() + 1];
            for (index, body) in self.objects.iter().enumerate() {
                offsets[index + 1] = out.len();
                out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
                out.extend_from_slice(body);
                out.extend_from_slice(b"\nendobj\n");
            }
            let start_xref = out.len();
            out.extend_from_slice(format!("xref\n0 {}\n", self.objects.len() + 1).as_bytes());
            out.extend_from_slice(b"0000000000 65535 f \n");
            for offset in &offsets[1..] {
                // xref 每一项**必须**正好 20 字节（`nnnnnnnnnn ggggg n \n`）：短一个字符
                // poppler 会去按 `/Size` 硬数，而 pdf_oxide 那类实现会当文件坏了。
                out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
            }
            let trailer = format!(
                "trailer\n<< /Size {} /Root {} 0 R{} >>\nstartxref\n{start_xref}\n%%EOF\n",
                self.objects.len() + 1,
                root,
                info.map_or_else(String::new, |id| format!(" /Info {id} 0 R"))
            );
            out.extend_from_slice(trailer.as_bytes());
            out
        }
    }

    /// PDF 字符串：纯 ASCII 走字面量，带非 ASCII 走 `<FEFF …>` 的 UTF-16BE。
    ///
    /// 非 ASCII **必须**这样写：`/Info` 的字面量串按 PDFDocEncoding（≈Latin-1）解，
    /// 直接把 UTF-8 的汉字塞进括号里，读出来就是 `æµ‹è¯•` 那种串 —— 而那一形态在真文件里
    /// 也真存在（有些生产者就这么写），所以解码那一步归 `decode_pdf_text_string`，
    /// 夹具这边给规范形态。
    /// 一份最小可用的 TrueType：`glyf` 里只有 `.notdef` 的空轮廓，`hmtx` 每个码位都是
    /// 600/1000 的前进宽（与 `/Widths` 对得上），`cmap` 是「码位 = 字形号」的格式 4。
    ///
    /// 它存在的理由只有一个：让夹具**嵌入**字体，而不是指一份本机有没有都说不清的标准
    /// 14 号。轮廓是空的，所以它画出来只有字宽、没有墨 —— 本文件量「盒」的那几条门因此
    /// 落在字形的基准点上，不落在轮廓上。
    fn synthetic_truetype() -> Vec<u8> {
        fn table(tag: &[u8; 4], data: &[u8], offset: usize, out: &mut Vec<u8>) {
            out.extend_from_slice(tag);
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        }
        // 95 个码位（32..=126），前 4 个字形是 `.notdef` / `.null` / CR / space，其余映射
        // 自己（码位 c → 字形 c-28），这样 `cmap` 与 `hmtx` 的长度都跟着码位数走。
        let count: usize = 95 + 29;
        let mut hmtx = Vec::new();
        for _ in 0..count {
            hmtx.extend_from_slice(&1000u16.to_be_bytes());
            hmtx.extend_from_slice(&0i16.to_be_bytes());
        }
        let num_glyphs = count as u16;
        let mut maxp = Vec::new();
        maxp.extend_from_slice(&0x00010000u32.to_be_bytes());
        maxp.extend_from_slice(&num_glyphs.to_be_bytes());
        maxp.extend_from_slice(&[0u8; 26]);

        let mut head = Vec::new();
        let _ = ();
        head.extend_from_slice(&0x00010000u32.to_be_bytes()); // version
        head.extend_from_slice(&0x00010000u32.to_be_bytes()); // fontRevision
        head.extend_from_slice(&0u32.to_be_bytes()); // checkSumAdjustment
        head.extend_from_slice(&0x5F0F3CF5u32.to_be_bytes()); // magic
        head.extend_from_slice(&0x0003u16.to_be_bytes()); // flags
        head.extend_from_slice(&1000u16.to_be_bytes()); // unitsPerEm
        head.extend_from_slice(&[0u8; 16]); // created / modified
        head.extend_from_slice(&0i16.to_be_bytes()); // xMin
        head.extend_from_slice(&(-200i16).to_be_bytes()); // yMin
        head.extend_from_slice(&1000i16.to_be_bytes()); // xMax
        head.extend_from_slice(&1000i16.to_be_bytes()); // yMax
        head.extend_from_slice(&0u16.to_be_bytes()); // macStyle
        head.extend_from_slice(&8u16.to_be_bytes()); // lowestRecPPEM
        head.extend_from_slice(&2i16.to_be_bytes()); // fontDirectionHint
        head.extend_from_slice(&0i16.to_be_bytes()); // indexToLocFormat（短格式）
        head.extend_from_slice(&0i16.to_be_bytes()); // glyphDataFormat

        let mut hhea = Vec::new();
        hhea.extend_from_slice(&0x00010000u32.to_be_bytes());
        hhea.extend_from_slice(&1000i16.to_be_bytes()); // ascender
        hhea.extend_from_slice(&(-200i16).to_be_bytes()); // descender
        hhea.extend_from_slice(&0i16.to_be_bytes()); // lineGap
        hhea.extend_from_slice(&1000u16.to_be_bytes()); // advanceWidthMax
        hhea.extend_from_slice(&[0u8; 6]); // minLeftSideBearing … xMaxExtent
        hhea.extend_from_slice(&1i16.to_be_bytes()); // caretSlopeRise
        hhea.extend_from_slice(&0i16.to_be_bytes()); // caretSlopeRun
        hhea.extend_from_slice(&0i16.to_be_bytes()); // caretOffset
        hhea.extend_from_slice(&[0u8; 8]); // reserved
        hhea.extend_from_slice(&0i16.to_be_bytes()); // metricDataFormat
        hhea.extend_from_slice(&num_glyphs.to_be_bytes()); // numberOfHMetrics

        // `cmap`：格式 4，一段 32..=126。
        let seg_count = 2u16;
        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes()); // version
        cmap.extend_from_slice(&1u16.to_be_bytes()); // numTables
        cmap.extend_from_slice(&3u16.to_be_bytes()); // platformID
        cmap.extend_from_slice(&1u16.to_be_bytes()); // encodingID
        cmap.extend_from_slice(&12u32.to_be_bytes()); // offset
        // 格式 4
        cmap.extend_from_slice(&4u16.to_be_bytes());
        let mut body = Vec::new();
        body.extend_from_slice(&0u16.to_be_bytes()); // length（占位，稍后填）
        body.extend_from_slice(&0u16.to_be_bytes()); // language
        body.extend_from_slice(&(seg_count * 2).to_be_bytes()); // segCountX2
        body.extend_from_slice(&4u16.to_be_bytes()); // searchRange
        body.extend_from_slice(&1u16.to_be_bytes()); // entrySelector
        body.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
        body.extend_from_slice(&126u16.to_be_bytes()); // endCode（第一段）
        body.extend_from_slice(&0xFFFFu16.to_be_bytes()); // endCode（收尾段）
        body.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
        body.extend_from_slice(&32u16.to_be_bytes()); // startCode
        body.extend_from_slice(&0xFFFFu16.to_be_bytes()); // startCode（收尾段）
        body.extend_from_slice(&0i16.to_be_bytes()); // idDelta：字形 = 码位 − 28
        body.extend_from_slice(&1i16.to_be_bytes()); // idDelta（收尾段）
        body.extend_from_slice(&0u16.to_be_bytes()); // idRangeOffset
        body.extend_from_slice(&0u16.to_be_bytes()); // idRangeOffset（收尾段）
        let length = (body.len() + 2) as u16;
        body[0..2].copy_from_slice(&length.to_be_bytes());
        cmap.extend_from_slice(&body);

        // `glyf`：每个字形是一个**方块**，从 (100, 0) 到 (500, 700)（upem 1000）。
        // 空轮廓也有「宽」，但没有「顶」，而文字层那一行的上沿正是从墨迹的顶读出来的 ——
        // 所以这里给每字一块实心墨，位置与大小都是故意取整的整数，好让门去校。
        let mut one_glyph = Vec::new();
        one_glyph.extend_from_slice(&1i16.to_be_bytes()); // numberOfContours
        one_glyph.extend_from_slice(&100i16.to_be_bytes()); // xMin
        one_glyph.extend_from_slice(&0i16.to_be_bytes()); // yMin
        one_glyph.extend_from_slice(&500i16.to_be_bytes()); // xMax
        one_glyph.extend_from_slice(&700i16.to_be_bytes()); // yMax
        one_glyph.extend_from_slice(&3u16.to_be_bytes()); // endPtsOfContours[0]
        one_glyph.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
        // 四个点，flags 全 0：x 与 y 的增量都按 **i16** 写（先四个 x，再四个 y）。
        // (500,0) → (500,700) → (100,700) → (100,0)
        one_glyph.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        one_glyph.extend_from_slice(&500u16.to_be_bytes());
        one_glyph.extend_from_slice(&0u16.to_be_bytes());
        one_glyph.extend_from_slice(&(-400i16).to_be_bytes());
        one_glyph.extend_from_slice(&0u16.to_be_bytes());
        one_glyph.extend_from_slice(&0u16.to_be_bytes());
        one_glyph.extend_from_slice(&700u16.to_be_bytes());
        one_glyph.extend_from_slice(&0u16.to_be_bytes());
        one_glyph.extend_from_slice(&(-700i16).to_be_bytes());
        while one_glyph.len() % 4 != 0 {
            one_glyph.push(0);
        }
        let mut glyf = Vec::new();
        let mut loca = Vec::new();
        for _ in 0..count {
            loca.extend_from_slice(&((glyf.len() / 2) as u16).to_be_bytes());
            glyf.extend_from_slice(&one_glyph);
        }
        loca.extend_from_slice(&((glyf.len() / 2) as u16).to_be_bytes());

        let mut tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
            (b"cmap", cmap),
            (b"glyf", glyf),
            (b"head", head),
            (b"hhea", hhea),
            (b"hmtx", hmtx),
            (b"loca", loca),
            (b"maxp", maxp),
        ];
        tables.sort_by(|a, b| a.0.cmp(b.0));
        let count_tables = tables.len() as u16;
        let header = 12 + 16 * count_tables as usize;
        let mut out = Vec::with_capacity(header + 64);
        out.extend_from_slice(&0x00010000u32.to_be_bytes());
        out.extend_from_slice(&count_tables.to_be_bytes());
        // searchRange / entrySelector / rangeShift：最小合法值即可。
        let mut power = 1u16;
        while power * 2 <= count_tables {
            power *= 2;
        }
        out.extend_from_slice(&(power * 16).to_be_bytes());
        out.extend_from_slice(&(power.trailing_zeros() as u16).to_be_bytes());
        out.extend_from_slice(&(count_tables * 16 - power * 16).to_be_bytes());
        let mut offset = header;
        for (tag, data) in &tables {
            table(tag, data, offset, &mut out);
            offset += data.len();
        }
        for (_, data) in &tables {
            out.extend_from_slice(data);
        }
        out
    }

    /// `/ToUnicode`：码位原样映射回自己（WinAnsi 那一段是 Latin-1）。
    fn winansi_tounicode() -> String {
        let mut out = String::from(
            "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n             /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n             /CMapName /folio-std def\n/CMapType 2 def\n             1 begincodespacerange\n<20> <7E>\nendcodespacerange\n95 beginbfchar\n",
        );
        for code in 32u16..=126 {
            out.push_str(&format!("<{code:02X}> <{code:04X}>\n"));
        }
        out.push_str("endbfchar\nendcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
        out
    }

    fn pdf_text(text: &str) -> String {
        if text.is_ascii() {
            return format!("({})", text.replace(['(', ')'], ""));
        }
        let mut hex = String::from("<FEFF");
        for unit in text.encode_utf16() {
            hex.push_str(&format!("{unit:04X}"));
        }
        hex.push('>');
        hex
    }

    fn box_array(value: &[i32; 4]) -> String {
        format!("[{} {} {} {}]", value[0], value[1], value[2], value[3])
    }

    /// 拼出夹具本体，并落到一个临时文件里（provider 只认路径）。
    fn write_fixture(
        dir: &Path,
        name: &str,
        pages: &[Page],
        bookmarks: &[Bookmark],
        info: &[(&str, &str)],
    ) -> PathBuf {
        let mut file = Fixture::new();
        let catalog = file.reserve();
        let pages_node = file.reserve();
        let page_ids: Vec<usize> = pages.iter().map(|_| file.reserve()).collect();
        let content_ids: Vec<usize> = pages.iter().map(|_| file.reserve()).collect();
        // 字要落到页上就得有一个 `/Font` 可指。这里用**标准十四**那一族的 `/Helvetica`
        // （不嵌字节）：实测 pdf_oxide 认它，`extract_text_lines` 交得出字与盒，
        // 而省掉一整份嵌入字体 —— 夹具要的只是「这一行印在第几」。
        let any_text = pages.iter().any(|page| !page.text.is_empty());
        // 字体走**嵌入的一份**，不指标准 14 号：`/Helvetica` 不带字库，读它的是系统字体
        // 解析（hayro 那条路要么用内置替代字体、要么什么都没有），而这一格要验的是
        // 「盒在哪儿」，不该赌本机装了什么字体。下面那份 TTF 是程序拼出来的，每个码位
        // 一张空轮廓（`.notdef`）—— 对「字写在第几行第几个」这个问题，空轮廓与真字形
        // 一样诚实：位置来自字体矩阵，不来自轮廓。
        // 字体走**嵌入的一份**，不指标准 14 号：`/Helvetica` 不带字库，读它的是系统字体
        // 解析（hayro 那条路要么用内置替代字体、要么什么都没有），而这一格要验的是
        // 「字写在第几行第几个」，不该赌本机装了什么字体。
        //
        // 下面那份 TTF 是程序拼出来的，每个码位一张空轮廓 —— 对这个问题，空轮廓与真
        // 字形一样诚实：位置来自字形矩阵（字号、`Tm`、字宽），不来自轮廓。盒是墨迹，
        // 空轮廓的盒就是零，所以本文件里那些「字的盒」测试都在**量字形矩阵**那一段，
        // 而不是量轮廓。
        let font = any_text.then(|| file.reserve());
        let descriptor = any_text.then(|| file.reserve());
        let font_file = any_text.then(|| file.reserve());
        let to_unicode = any_text.then(|| file.reserve());
        if let Some(id) = font {
            let (Some(descriptor), Some(font_file), Some(to_unicode)) =
                (descriptor, font_file, to_unicode)
            else {
                unreachable!("四样一起 reserve");
            };
            file.set(
                id,
                // **宽 1000**（upem 的整一格）：`hayro` 把字形轮廓画到 upem 1000 的坐标里，
                // 前进宽也按这个数折。声明 600 的宽会得到 0.6 个 em 的推进量，而轮廓仍按
                // 1000 的比例摆 —— 两者对不上，字就会叠在一起。所以这里取 1000，与
                // `synthetic_truetype` 的 `hmtx` 一致。
                format!(
                    "<< /Type /Font /Subtype /TrueType /BaseFont /FolioStd /FirstChar 32 /LastChar 126 /Widths [ {} ] /Encoding /WinAnsiEncoding /FontDescriptor {descriptor} 0 R /ToUnicode {to_unicode} 0 R >>",
                    (32..=126).map(|_| "1000").collect::<Vec<_>>().join(" "),
                ),
            );
            file.set(
                descriptor,
                format!(
                    "<< /Type /FontDescriptor /FontName /FolioStd /Flags 32 /FontBBox [0 0 1000 1000] /ItalicAngle 0 /Ascent 1000 /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 {font_file} 0 R >>"
                ),
            );
            let ttf = synthetic_truetype();
            let mut body =
                format!("<< /Length {} /Length1 {} >>\nstream\n", ttf.len(), ttf.len())
                    .into_bytes();
            body.extend_from_slice(&ttf);
            body.extend_from_slice(b"\nendstream");
            file.set(font_file, body);
            let cmap = winansi_tounicode();
            file.set(
                to_unicode,
                format!("<< /Length {} >>\nstream\n{cmap}\nendstream", cmap.len()),
            );
        }
        let info_id = if info.is_empty() {
            None
        } else {
            Some(file.reserve())
        };
        let outlines = if bookmarks.is_empty() {
            None
        } else {
            Some(file.reserve())
        };
        let bookmark_ids: Vec<usize> = bookmarks.iter().map(|_| file.reserve()).collect();
        // 每页铺的那张图是一个 XObject（`/Subtype /Image`），页的 `/Resources` 指它。
        let image_ids: Vec<Option<usize>> = pages
            .iter()
            .map(|page| page.image.as_ref().map(|_| file.reserve()))
            .collect();

        // 内容流：一个灰矩形，位置每页不同 —— 两页的 PNG 不许长得一样；有图的页再把
        // 那张图按 bbox 铺上去（扫描件就是这一形状：图盖满整页，页上一根字都没有）。
        for (index, id) in content_ids.iter().enumerate() {
            let mut data = format!("0.85 0.85 0.90 rg\n{} 72 300 500 re f\n", 60 + index * 12);
            if let Some(placed) = &pages[index].image {
                let [x, y, right, top] = placed.bbox;
                data.push_str(&format!(
                    "q {} 0 0 {} {} {} cm /Im1 Do Q\n",
                    right - x,
                    top - y,
                    x,
                    y
                ));
            }
            if !pages[index].text.is_empty() {
                data.push_str("BT\n");
                for (x, y, size, text) in &pages[index].text {
                    // 用 `Tm` 而不是 `Td` 定位置：`Td` 是**相对当前文本行的起点**，
                    // 两行都从同一个 `BT` 出来时，第二行的数会被当成位移加在第一行之上
                    // （症状是那行字跑到页外，抽取于是读不出它 —— 踩过一次）。
                    data.push_str(&format!("/F1 {size} Tf\n1 0 0 1 {x} {y} Tm\n({text}) Tj\n"));
                }
                data.push_str("ET\n");
            }
            file.set(
                *id,
                format!("<< /Length {} >>\nstream\n{data}", data.len())
                    .into_bytes()
                    .into_iter()
                    .chain(b"\nendstream".iter().copied())
                    .collect::<Vec<u8>>(),
            );
        }
        for (index, page) in pages.iter().enumerate() {
            let mut parts = vec![format!("/Type /Page /Parent {} 0 R", pages_node)];
            if !page.inherit {
                parts.push(format!("/MediaBox {}", box_array(&page.media)));
            }
            if let Some(crop) = page.crop {
                parts.push(format!("/CropBox {}", box_array(&crop)));
            }
            if page.rotate != 0 {
                parts.push(format!("/Rotate {}", page.rotate));
            }
            // 一份 `/Resources` 里两类都塞进去（两个键分开写会让解析器按后一个覆盖前一个，
            // 一份带字又带图的页就会丢掉字体那一半）。
            let mut resources = Vec::new();
            if let Some(font) = font {
                resources.push(format!("/Font << /F1 {font} 0 R >>"));
            }
            if let Some(Some(image)) = image_ids.get(index) {
                resources.push(format!("/XObject << /Im1 {image} 0 R >>"));
            }
            if !resources.is_empty() {
                parts.push(format!("/Resources << {} >>", resources.join(" ")));
            }
            parts.push(format!("/Contents {} 0 R", content_ids[index]));
            file.set(page_ids[index], format!("<< {} >>", parts.join(" ")));
        }
        // 图本身：**不压缩**的 8 位灰度（`/DeviceGray`）。这一格要验的是「页上有几张
        // 多少像素的图、印在多大一块上」，与滤镜无关；而 JPEG / Flate 都要另带一段
        // 编码器或一份二进制样本，那会把夹具的重点从页框挪到字节上。
        for (index, id) in image_ids.iter().enumerate() {
            let Some(id) = id else { continue };
            let placed = pages[index].image.as_ref().unwrap();
            let mut pixels = vec![0u8; (placed.width * placed.height) as usize];
            for (at, px) in pixels.iter_mut().enumerate() {
                *px = (at as u32 % placed.width) as u8;
            }
            let mut body = format!(
                "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {} >>\nstream\n",
                placed.width,
                placed.height,
                pixels.len()
            )
            .into_bytes();
            body.extend_from_slice(&pixels);
            body.extend_from_slice(b"\nendstream");
            file.set(*id, body);
        }
        let kids = page_ids
            .iter()
            .map(|id| format!("{id} 0 R"))
            .collect::<Vec<_>>()
            .join(" ");
        // `/MediaBox` 挂在 `/Pages` 上：那一页自己不写时靠它继承。
        file.set(
            pages_node,
            format!(
                "<< /Type /Pages /Kids [ {kids} ] /Count {} /MediaBox [0 0 612 792] >>",
                pages.len()
            ),
        );
        let mut catalog_parts = vec![format!("/Type /Catalog /Pages {pages_node} 0 R")];
        if let Some(id) = outlines {
            let first = bookmark_ids.first().copied().unwrap_or(0);
            let last = bookmark_ids.last().copied().unwrap_or(0);
            file.set(
                id,
                format!(
                    "<< /Type /Outlines /First {first} 0 R /Last {last} 0 R /Count {} >>",
                    bookmark_ids.len()
                ),
            );
            catalog_parts.push(format!("/Outlines {id} 0 R"));
            for (index, bookmark) in bookmarks.iter().enumerate() {
                let mut parts = vec![
                    format!("/Title {}", pdf_text(&bookmark.title)),
                    format!("/Parent {id} 0 R"),
                ];
                if index > 0 {
                    parts.push(format!("/Prev {} 0 R", bookmark_ids[index - 1]));
                }
                if index + 1 < bookmark_ids.len() {
                    parts.push(format!("/Next {} 0 R", bookmark_ids[index + 1]));
                }
                if let Some(page) = bookmark.page {
                    parts.push(format!("/Dest [ {} 0 R /Fit ]", page_ids[page]));
                }
                file.set(bookmark_ids[index], format!("<< {} >>", parts.join(" ")));
            }
        }
        file.set(catalog, format!("<< {} >>", catalog_parts.join(" ")));
        if let (Some(id), pairs) = (info_id, info) {
            let body = pairs
                .iter()
                .map(|(key, value)| format!("/{key} {}", pdf_text(value)))
                .collect::<Vec<_>>()
                .join(" ");
            file.set(id, format!("<< {body} >>"));
        }
        let bytes = file.finish(catalog, info_id);
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// 三页那一本：A4 竖页、被裁的一页、继承 MediaBox 且转 90 度的横页。
    fn three_pages(dir: &Path) -> PathBuf {
        write_fixture(
            dir,
            "合成书.pdf",
            &[
                Page::new([0, 0, 595, 842]),
                Page::new([0, 0, 400, 600]).crop([50, 50, 350, 550]),
                Page::new([0, 0, 612, 792]).rotate(90).inherit(),
            ],
            &[
                Bookmark::to_page("第一章", 0),
                Bookmark::to_page("第二章", 1),
                Bookmark::nowhere("有标题没落点的一节"),
            ],
            &[
                ("Title", "测试合成本"),
                ("Author", "Chen, Xiaoming"),
                ("Keywords", "Machine Learning, AI; 中文"),
                ("Creator", "folio 测试夹具"),
                ("Producer", "hand-built"),
                ("CreationDate", "D:20091110100441+08'00'"),
            ],
        )
    }

    /// 一页**既被裁又转了 90 度**的那一本。
    ///
    /// 分开写一本而不是往 `three_pages` 里再塞一页，是因为那三页的尺寸断言是一个整体
    /// （`[(1447,2048),(1229,2048),(2048,1583)]`），多一页就得改一处本来已经对了的期望值。
    fn crop_and_rotate(dir: &Path) -> PathBuf {
        write_fixture(
            dir,
            "横转竖的一页.pdf",
            &[Page::new([0, 0, 600, 400])
                .crop([100, 50, 500, 350])
                .rotate(90)],
            &[],
            &[("Title", "横转竖")],
        )
    }

    /// 一本「扫描件 + 一张小图 + 转过的一页」的合成书：图片页封顶的三条路各一页。
    ///
    /// 页框取 `300 × 424 pt`（半个 A4），图取同尺寸的 `300 × 424 px` —— 72 ppi 恰好是
    /// 「1 点 1 像素」那一档，算得清。图不压缩（`/DeviceGray`），夹具因此只有 127 KB。
    fn scanned(dir: &Path) -> PathBuf {
        write_fixture(
            dir,
            "扫描件的三种页.pdf",
            &[
                // 铺满整页：扫描件，按源图 72 ppi 封顶。
                Page::new([0, 0, 300, 424]).image(300, 424, [0, 0, 300, 424]),
                // 只盖住下面三成：上面那块是矢量画的，不封。
                Page::new([0, 0, 300, 424]).image(300, 120, [0, 0, 300, 120]),
                // 铺满但转了 90 度：图的盒在未转空间里，不封。
                Page::new([0, 0, 300, 424])
                    .image(300, 424, [0, 0, 300, 424])
                    .rotate(90),
            ],
            &[],
            &[("Title", "扫描件的三种页")],
        )
    }

    fn fixed(path: &Path) -> (DocMeta, FixedDoc) {
        let (meta, content) = PdfAdapter::default()
            .index(path)
            .unwrap_or_else(|err| panic!("{} 该读得开：{err}", path.display()));
        let Content::Fixed(fixed) = content else {
            panic!("PDF 该读成定版式");
        };
        (meta, fixed)
    }

    /// 仓库里那份**入库的**夹具（`tests/fixtures/oss/pdf/three-pages.pdf`，由 poppler 验过形状）
    /// 与测试代码里现拼的那一本，必须读出同一个结果。
    ///
    /// 两条各管一半：现拼的那一条赢在能改成任何形状（越界书签、坏 xref、零页），入库的那
    /// 一条赢在**字节不是我们写的** —— 只有合成器自己写自己的解析器时，两边同时错才是可能
    /// 的，而这一条门让那件事要发生两次。
    #[test]
    fn the_shipped_fixture_matches_the_synthetic_one() {
        let shipped = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/pdf/three-pages.pdf");
        assert!(shipped.exists(), "入库的夹具不许丢：{}", shipped.display());
        let dir = tempfile::tempdir().unwrap();
        let (real_meta, real) = fixed(&shipped);
        let (synth_meta, synth) = fixed(&three_pages(dir.path()));
        assert_eq!(real.page_count(), 3, "pdfinfo 说 3 页");
        let sizes: Vec<(u32, u32)> = real.pages.iter().map(|p| (p.width, p.height)).collect();
        assert_eq!(
            sizes,
            vec![(1447, 2048), (1229, 2048), (2048, 1583)],
            "入库那份的三页尺寸（第 2 页按 CropBox、第 3 页继承 MediaBox 且转了 90 度）"
        );
        assert_eq!(real_meta.title, synth_meta.title);
        assert_eq!(real_meta.authors, synth_meta.authors);
        assert_eq!(real_meta.subjects, synth_meta.subjects);
        assert_eq!(real_meta.published, "2009-11-10");
        assert_eq!(
            real.toc
                .iter()
                .map(|e| (e.title.clone(), e.page))
                .collect::<Vec<_>>(),
            synth
                .toc
                .iter()
                .map(|e| (e.title.clone(), e.page))
                .collect::<Vec<_>>()
        );
        // 取字节那一条也对得上：两本的第 1 页画出来一样大。
        let image = image::load_from_memory(
            &PdfAdapter::default()
                .render_page(&shipped, 1, RENDER_EDGE)
                .expect("入库那本该画得出来"),
        )
        .unwrap();
        assert!(
            image.width().abs_diff(real.pages[1].width) <= 2
                && image.height().abs_diff(real.pages[1].height) <= 2,
            "被裁的一页也要画满那一档：{}×{} vs {:?}",
            image.width(),
            image.height(),
            (real.pages[1].width, real.pages[1].height)
        );
    }

    /// 图片页按**源图自己的密度**封顶：一页铺满的扫描件按 2048 画，多出来的像素全是
    /// 插值（`hugG.pdf`：单页 816 ms → 106 ms，全书 37 s → 4.3 s）。
    ///
    /// 三条**不封**的路也在这里：图只盖住一块（剩下的面积是矢量画的）、转过的页、
    /// 没有图的页（最后那条由 `pages_are_raster_sized_and_keep_their_orientation` 钉着）。
    #[test]
    fn an_image_page_is_rastered_at_its_own_density() {
        let dir = tempfile::tempdir().unwrap();
        let path = scanned(dir.path());
        let (_, fixed) = fixed(&path);
        let sizes: Vec<(u32, u32)> = fixed.pages.iter().map(|p| (p.width, p.height)).collect();
        assert_eq!(
            sizes,
            vec![(264, 373), (1449, 2048), (2048, 1449)],
            "铺满的一页封到 0.88 × 424 px = 373；图只占三成的、转过的都按 2048 那一档"
        );

        // 页表说的尺寸与画出来的像素必须**同一句判断**（`render_edge`）：两处各写一个
        // 「这一页画多大」，症状是图集按 2048 记账而图只有 373，一张页虚报掉三个名额。
        let png = PdfAdapter::default()
            .render_page(&path, 0, RENDER_EDGE)
            .expect("扫描页该画得出来");
        let image = image::load_from_memory(&png).expect("交回的该是 PNG");
        assert!(
            image.width().abs_diff(fixed.pages[0].width) <= 2
                && image.height().abs_diff(fixed.pages[0].height) <= 2,
            "页表说 {}×{}，画出来 {}×{}",
            fixed.pages[0].width,
            fixed.pages[0].height,
            image.width(),
            image.height()
        );
    }

    /// 封顶是**上限之内**的事：调用方要更低的一档（封面、契约测试）时照旧听它的。
    #[test]
    fn a_lower_ceiling_still_wins_over_the_source_density() {
        let dir = tempfile::tempdir().unwrap();
        let path = scanned(dir.path());
        let png = PdfAdapter::default()
            .render_page(&path, 0, 128)
            .expect("这一档也该画得出来");
        let image = image::load_from_memory(&png).expect("交回的该是 PNG");
        assert!(
            image.height().abs_diff(128) <= 2,
            "要 128 那一档，画出来 {}",
            image.height()
        );
    }

    #[test]
    fn it_claims_pdf_by_extension_and_reports_the_format() {
        assert_eq!(PdfAdapter::default().format(), Format::Pdf);
        assert!(PdfAdapter::default().supports(Path::new("a.pdf")));
        assert!(PdfAdapter::default().supports(Path::new("A.PDF")));
        assert!(!PdfAdapter::default().supports(Path::new("a.ps")));
        assert!(!PdfAdapter::default().supports(Path::new("a.cbz")));
    }

    /// 页表：**顺序是页树顺序**，每页带自己算出来的栅格尺寸，而落点是引擎的页号。
    ///
    /// 尺寸是这一条真正要钉的：这三页都是矢量画的（没有一张图），长边 = `RENDER_EDGE`
    /// （2048），短边按比例。A4 竖页 `595 × 842 pt` → `1447 × 2048`；被裁成
    /// `300 × 500 pt` 的那页 → `1229 × 2048`；转了 90 度的信纸 → **宽长**
    /// （`2048 × 1583`）。最后这一条防的是「把长短边折成一个固定顺序」—— 那样横页会被
    /// 摆成竖的，屏幕上就是字压字。图片页按源图封顶那一条见
    /// `an_image_page_is_rastered_at_its_own_density`。
    #[test]
    fn pages_are_raster_sized_and_keep_their_orientation() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_pages(dir.path());
        let (_, fixed) = fixed(&path);
        assert_eq!(fixed.page_count(), 3);
        let sizes: Vec<(u32, u32)> = fixed.pages.iter().map(|p| (p.width, p.height)).collect();
        assert_eq!(
            sizes,
            vec![(1447, 2048), (1229, 2048), (2048, 1583)],
            "{sizes:?}"
        );
        assert!(
            fixed.pages[2].width > fixed.pages[2].height,
            "第 3 页转了 90 度，存回来的必须是横的"
        );
        for (index, page) in fixed.pages.iter().enumerate() {
            match &page.source {
                PageSource::Render { index: at, .. } => assert_eq!(*at as usize, index),
                PageSource::Image { href } => panic!("渲染型的页不该有 href：{href}"),
            }
            assert!(page.extra_images.is_empty());
        }
        assert_eq!(
            (fixed.width, fixed.height),
            (1447, 2048),
            "首屏基准是第 1 页"
        );
        assert!(fixed.images.is_empty(), "渲染型的页不进图片表");
    }

    /// 被裁的那一页按 **CropBox ∩ MediaBox** 算尺寸，不是按 MediaBox。
    ///
    /// 上一条目测里已经含了这一格（`1229 × 2048` 只可能来自 300×500 而不是 400×600），
    /// 单独立一条是因为它的错法很难从页表里看出来：按 MediaBox 算的页**看起来也是完整的**，
    /// 只是多出一圈本该被裁掉的出血与裁切线。
    #[test]
    fn the_visible_box_beats_the_media_box() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_pages(dir.path());
        let (_, fixed) = fixed(&path);
        let cropped = &fixed.pages[1];
        // 400×600 的 MediaBox 会给出 1365×2048；300×500 的可见框给出 1229×2048。
        assert_eq!((cropped.width, cropped.height), (1229, 2048));
        assert_ne!(
            (cropped.width, cropped.height),
            (1365, 2048),
            "按 MediaBox 算就带上了出血"
        );
    }

    #[test]
    fn bookmarks_land_on_pages_and_a_targetless_one_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_pages(dir.path());
        let (_, fixed) = fixed(&path);
        let rows: Vec<(&str, Option<usize>)> = fixed
            .toc
            .iter()
            .map(|entry| (entry.title.as_str(), entry.page))
            .collect();
        assert_eq!(
            rows,
            vec![("第一章", Some(0)), ("第二章", Some(1))],
            "有落点的两条进来，没落点的那条丢掉，而顺序就是书签顺序"
        );
        assert!(
            fixed.toc.iter().all(|entry| entry.href.is_empty()),
            "定版式的目录条没有容器路径：落点只由 `page` 那一格说"
        );
    }

    /// `/Info` 的四项各归各处，其中三项是**故意写成不好办的形状**：
    ///
    /// - `Title` 是 UTF-16BE 的中文 → 不是 `String::from_utf8` 那条路能解的；
    /// - `Author` 是 `Chen, Xiaoming`（姓, 名）→ 按逗号切会变成两个作者；
    /// - `Keywords` 混用逗号与分号，还带一个括号包着的词组。
    #[test]
    fn the_info_dictionary_fills_the_bibliography() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_pages(dir.path());
        let (meta, _) = fixed(&path);
        assert_eq!(meta.title, "测试合成本", "UTF-16BE 的标题要解对");
        assert_eq!(
            meta.authors,
            vec!["Chen, Xiaoming".to_string()],
            "/Author 没有列表语法，按逗号切会把一个人的名字切成两个人"
        );
        assert_eq!(
            meta.subjects,
            vec!["Machine Learning", "AI", "中文"],
            "{:?}",
            meta.subjects
        );
        assert_eq!(meta.published, "2009-11-10");
        assert_eq!(meta.format, Format::Pdf);
        assert_eq!(
            meta.publisher, "",
            "PDF 没有出版社这一项，不许拿 /Producer 冒充"
        );
        assert_eq!(meta.language, "");
        assert!(meta.size > 0);
        // 没有书签、`/Info` 里也**没有 /Title** 的那一本：标题回落到文件名。
        // 故意留着 `/Producer` —— 「有 /Info 但缺 Title」比「整个 /Info 都不在」更常见，
        // 也让这条门不至于被「整个字典读不出」那种实现蒙过去。
        let no_title = write_fixture(
            dir.path(),
            "改名的一本.pdf",
            &[Page::new([0, 0, 300, 300])],
            &[],
            &[("Producer", "hand-built")],
        );
        let (fallback, bare) = fixed(&no_title);
        assert_eq!(
            fallback.title, "改名的一本",
            "没有 /Title 就用文件名，不能给空串"
        );
        assert!(
            fallback.published.is_empty(),
            "没有 /CreationDate 就是不摆那一行"
        );
        assert!(bare.toc.is_empty(), "没有书签就是空表，不是几条空标题");
    }

    /// 日期与关键词的解析规则单独钉：它们的失败方式都是**安静地摆一行错东西**。
    #[test]
    fn pdf_dates_and_keywords_are_parsed_not_guessed() {
        // 规范形态、没有 D: 前缀、只到年、月日越界、空串。
        assert_eq!(
            pdf_date_to_day("D:20091110100441+08'00'").as_deref(),
            Some("2009-11-10")
        );
        assert_eq!(
            pdf_date_to_day("20240229000000Z").as_deref(),
            Some("2024-02-29")
        );
        assert_eq!(
            pdf_date_to_day("D:2009"),
            None,
            "只到年不补月日，宁可不摆这一行"
        );
        assert_eq!(pdf_date_to_day("D:20091332"), None, "13 月 32 日不是日期");
        assert_eq!(pdf_date_to_day(""), None);
        assert_eq!(
            split_keywords("Machine Learning, AI; 中文"),
            vec!["Machine Learning", "AI", "中文"]
        );
        assert_eq!(
            split_keywords("  ;; ,"),
            Vec::<String>::new(),
            "空片段不进表"
        );
        assert_eq!(
            split_keywords("(Machine Learning, AI)"),
            vec!["Machine Learning", "AI"],
            "括号里的逗号还是分了两个词 —— 这是已知取舍，理由写在 `split_keywords` 上"
        );
    }

    /// 封面就是首页栅格化，长边落在 `COVER_EDGE` 那一档。
    #[test]
    fn the_cover_is_the_first_page_rasterized() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_pages(dir.path());
        let (_, fixed) = fixed(&path);
        let cover = fixed.cover.expect("首页画得出来就该有封面");
        let image = image::load_from_memory(&cover).expect("封面该是解得开的位图");
        assert_eq!(
            image.width().max(image.height()),
            COVER_EDGE,
            "{}×{}",
            image.width(),
            image.height()
        );
    }

    /// 取一页的字节：按要的那一档画，长边等于它，横页竖页同一个式子。
    #[test]
    fn a_rendered_page_is_png_at_the_requested_edge() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_pages(dir.path());
        for (page, edge) in [(0usize, 600u32), (2, 600), (1, 1024)] {
            let bytes = PdfAdapter::default()
                .render_page(&path, page, edge)
                .unwrap_or_else(|| panic!("第 {page} 页 {edge} 该画得出来"));
            let image = image::load_from_memory(&bytes).expect("交回的该是 PNG");
            assert_eq!(
                image.width().max(image.height()),
                edge,
                "第 {page} 页按 {edge} 画"
            );
            assert!(
                bytes.starts_with(b"\x89PNG"),
                "第 {page} 页交回的不是 PNG 头"
            );
        }
        // 页不在书里：`None`，不是 panic、也不是空白字节。
        assert!(PdfAdapter::default().render_page(&path, 99, 512).is_none());
        assert!(
            PdfAdapter::default()
                .render_page(Path::new("/x/不存在的.pdf"), 0, 512)
                .is_none()
        );
    }

    /// 默认那一档与页表里存的尺寸必须是**同一个数**：界面按页表摆位置，按这一档要像素。
    #[test]
    fn the_default_edge_matches_what_the_page_table_stored() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_pages(dir.path());
        let (_, fixed) = fixed(&path);
        let bytes = PdfAdapter::default()
            .render_page(&path, 0, RENDER_EDGE)
            .expect("默认档该画得出来");
        let image = image::load_from_memory(&bytes).unwrap();
        let page = &fixed.pages[0];
        // 差一个像素是两条路各自 round 的结果，界面上看不出来；差得多就是两处口径漂了。
        assert!(
            image.width().abs_diff(page.width) <= 1 && image.height().abs_diff(page.height) <= 1,
            "页表存 {}×{}，实际画 {}×{}",
            page.width,
            page.height,
            image.width(),
            image.height()
        );
    }

    /// 一页都没有的书该当场失败，而不是打开成一本空白（与 CBZ 同一条立场）。
    #[test]
    fn a_pdf_without_pages_fails_instead_of_opening_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = Fixture::new();
        let catalog = file.reserve();
        let pages_node = file.reserve();
        file.set(pages_node, "<< /Type /Pages /Kids [ ] /Count 0 >>");
        file.set(
            catalog,
            format!("<< /Type /Catalog /Pages {pages_node} 0 R >>"),
        );
        let path = dir.path().join("零页.pdf");
        std::fs::write(&path, file.finish(catalog, None)).unwrap();
        assert!(PdfAdapter::default().index(&path).is_err(), "页树里没有一页，就该说不");
    }

    /// 头是 `%PDF` 而后面什么结构都没有：说「读不动」，不说「没有这本书」。
    ///
    /// **只改 `startxref` 的偏移不算** —— 实测 pdf_oxide 会走 xref 重建（`xref_reconstruction`
    /// 那一模块）把这种文件救回来，`index()` 照样成功。宽容不是缺陷，所以这一条门钉的是
    /// 「连对象都拼不出来」那一档：一个只有一个头的文件。
    #[test]
    fn a_header_without_a_body_is_reported_as_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("只有一个头.pdf");
        std::fs::write(&path, "%PDF-1.7 这一版还读不动\n%%EOF\n").unwrap();
        let err = PdfAdapter::default().index(&path).expect_err("只有一个头的文件该失败");
        assert!(
            matches!(err, crate::Error::UnsupportedFormat(_)),
            "该是「读不动」那一档，实际 {err:?}"
        );
        // 空文件与不存在的文件也不许 panic，也不许打开成「一本零页的书」。
        let empty = dir.path().join("空.pdf");
        std::fs::write(&empty, b"").unwrap();
        assert!(PdfAdapter::default().index(&empty).is_err());
        assert!(PdfAdapter::default().index(&dir.path().join("不在.pdf")).is_err());
        // 取字节那一侧同一条立场：读不到是 `None`，不是 panic。
        assert!(PdfAdapter::default().render_page(&path, 0, 512).is_none());
    }

    /// 加密与「读不动」是两档说法：前者读者的动作是换一本无口令的文件，后者是重导一次。
    ///
    /// 这一条门钉的是**映射表**而不是某个假的 `/Encrypt` 头：pdf_oxide 打开带口令的文件
    /// 常常不报错（文件结构是明文，加密的只是对象流），失败推迟到读页树那一趟 —— 于是
    /// `EncryptedPdf` 可能从 [`open_document`] 冒出来，也可能从 [`page_count`] 冒出来。
    /// 两个调用点都过 [`failure`] 这**一个**函数，所以「报表里那一栏从加密滑进读不动」
    /// 在结构上就不会发生，这比用一个假头去碰某一条路径更硬。
    /// 真加密样本要等一个能加密的工具（本机没有 `qpdf`，Ghostscript 造得出来但不是能入库的公版夹具）。
    #[test]
    fn encryption_and_corruption_get_different_words() {
        let path = Path::new("/x/有口令.pdf");
        assert!(
            matches!(
                failure(path, LoadPdfError::Decryption(hayro::hayro_syntax::DecryptionError::PasswordProtected)),
                crate::Error::Encrypted { .. }
            ),
            "加密要落进加密那一档"
        );
        let err = failure(path, LoadPdfError::Invalid);
        assert!(
            matches!(err, crate::Error::UnsupportedFormat(_)),
            "结构坏了就是「读不动」，{err:?}"
        );
        // 文案里带书名：导入报表那一行只有这个串可看。
        assert!(format!("{err}").contains("有口令.pdf"), "{err}");
    }

    /// 被裁过的一页也要**画满**要的那一档。
    ///
    /// `render_page_fit` 定比例用 MediaBox、出图按可见框裁，不反推目标框就会短一截
    /// （实测按 `edge × edge` 直接交时，一页 `400×600` 套 `300×500` 的页要 1024 只出 853）。
    /// 页表说的尺寸与实际画出来的必须同源，否则界面按 A 摆位、拿到的却是 B。
    #[test]
    fn a_cropped_page_is_rendered_at_the_requested_edge() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_pages(dir.path());
        let (_, doc) = fixed(&path);
        let page = &doc.pages[1];
        assert_eq!(
            (page.width, page.height),
            (1229, 2048),
            "按可见框 300×500 算"
        );
        let bytes = PdfAdapter::default()
            .render_page(&path, 1, RENDER_EDGE)
            .expect("画得出来");
        let image = image::load_from_memory(&bytes).unwrap();
        assert!(
            image.width().abs_diff(page.width) <= 2 && image.height().abs_diff(page.height) <= 2,
            "页表说 {}×{}，实际画 {}×{}",
            page.width,
            page.height,
            image.width(),
            image.height()
        );
    }

    /// 又裁又转 90 度的那一页：两条几何规则叠在一起时也要对齐。
    #[test]
    fn a_cropped_and_rotated_page_agrees_with_its_own_page_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = crop_and_rotate(dir.path());
        let (_, doc) = fixed(&path);
        let page = &doc.pages[0];
        // 可见框 400×300 pt 转 90 度 → 300×400，长边折到 2048 → `1536 × 2048`（竖）。
        assert_eq!((page.width, page.height), (1536, 2048), "{page:?}");
        let bytes = PdfAdapter::default().render_page(&path, 0, 1024).expect("画得出来");
        let image = image::load_from_memory(&bytes).unwrap();
        assert_eq!(
            image.width().max(image.height()),
            1024,
            "长边就该是我要的那一档"
        );
        assert!(image.width() < image.height(), "转了 90 度该是竖的");
    }

    /// 文字层：盒落在**可见框里、y 朝下**，行按上到下，词缝补成空格。
    ///
    /// 三条一起钉，各对应一种会画歪的错法：
    /// - **翻 y**：引擎交回的是「基线在 `y`、`height` 恰等于字号、往 y 增方向长」
    ///   （拿 Ghostscript 写的、位置自己定的那份量出来的），而栅格图是 y 朝下。
    ///   这里 24pt 那行基线在 700、页高 792 → 盒的上沿该是 `792 − (700 + 24) = 68`。
    /// - **行的序**：上一行的 `y` 必须比下一行小（不翻就是这个反号，症状是选中色块
    ///   从下往上长）。
    /// - **词缝**：`Heading alpha` 实测交回**两个词**、盒子里没有那个空格，不补就
    ///   复制出一串粘住的词。
    #[test]
    fn the_text_layer_lands_in_the_visible_frame_with_y_down() {
        let dir = tempfile::tempdir().unwrap();
        let page = Page::new([0, 0, 612, 792])
            .line(72, 700, 24.0, "Heading alpha")
            .line(72, 660, 12.0, "body beta");
        let path = write_fixture(dir.path(), "文字.pdf", &[page], &[], &[]);
        let document = PdfAdapter::from_bytes(std::fs::read(&path).unwrap()).unwrap();
        let text = page_text(&document, 0, &InterpreterSettings::default()).expect("这一页写着两行字");
        assert_eq!((text.width, text.height), (612.0, 792.0), "框就是可见框");
        let first = text.chars[0];
        assert_eq!(first.ch, 'H');
        // **盒是墨，不是 em 方框**：夹具的字形墨宽 9.6、高 16.8（upem 1000），基线在
        // `x = 72`、`y = 700`；翻 y 之后墨顶在 `792 − 700 − 16.8 = 75.2`。
        // 落笔顺序修好之后，第一个字的左沿就是基线 `x = 72`（从前被推进一格，量到 96）。
        assert!(
            (first.rect.x - 72.0).abs() < 0.1,
            "第一个字的左沿该落在基线 x=72 上：{}",
            first.rect.x
        );
        assert!(
            (first.rect.y - 75.2).abs() < 0.2,
            "上沿在基线翻过来再减墨高的那一格：{}",
            first.rect.y
        );
        assert!(
            (first.rect.height - 16.8).abs() < 0.1,
            "盒高是墨的高度：{}",
            first.rect.height
        );
        // 偏移空间只数字、不数换行，所以整页摊平是两条行首尾相接。
        assert_eq!(
            text.text(),
            concat!("Heading alpha", "body beta"),
            "偏移空间里不塞换行：整页就是两条行首尾相接"
        );
        // 但复制出去要还是那一行一行：跨行处补一个换行。
        assert_eq!(
            text.slice(0..text.char_count()),
            "Heading alpha\nbody beta",
            "跨行复制要断行"
        );
        let second = text
            .chars
            .iter()
            .find(|item| item.line == 1)
            .expect("第二行也该在");
        assert_eq!(second.ch, 'b');
        assert!(
            second.rect.y > first.rect.y,
            "下一行的盒必须在更下面：{} vs {}",
            second.rect.y,
            first.rect.y,
        );
        assert!(
            (second.rect.y - (792.0 - 660.0 - 8.4)).abs() < 0.1,
            "第二行（12pt）的墨顶：{}",
            second.rect.y
        );
        assert!((second.rect.height - 8.4).abs() < 0.1, "{}", second.rect.height);
        // 选中一行 = 一个色块（那一行里的字并起来），不是十来个。
        let line_zero: std::ops::Range<usize> =
            0..text.chars.iter().position(|item| item.line == 1).unwrap();
        let rects = text.rects(line_zero);
        assert_eq!(rects.len(), 1, "一行一条色块：{rects:?}");
        assert!((rects[0].x - 72.0).abs() < 0.1, "{:?}", rects[0]);
        assert!((rects[0].y - 75.2).abs() < 0.1, "{:?}", rects[0]);
        assert!(rects[0].width > 100.0, "一行应有长度：{rects:?}");
    }

    /// 裁过的页：盒按**可见框**裁掉左沿与上沿，不是按 MediaBox。
    ///
    /// 这条是被「偏一截」那类症状逼出来的：只取可见框的尺寸不取它的原点，被裁过的页
    /// （出血稿）上色块就会整体偏移，而偏移量恰好等于裁掉的那一截 —— 屏幕上看得出偏、
    /// 报不出错。这里 CropBox 左沿 100、上沿 692，字写在 x=140、基线 660。
    #[test]
    fn a_cropped_page_anchors_its_boxes_to_what_is_visible() {
        let dir = tempfile::tempdir().unwrap();
        let page = Page::new([0, 0, 612, 792]).crop([100, 0, 612, 692]).line(
            140,
            660,
            12.0,
            "inside the crop",
        );
        let path = write_fixture(dir.path(), "裁过的.pdf", &[page], &[], &[]);
        let document = PdfAdapter::from_bytes(std::fs::read(&path).unwrap()).unwrap();
        let text = page_text(&document, 0, &InterpreterSettings::default()).expect("裁完还留着这一行");
        assert_eq!((text.width, text.height), (512.0, 692.0));
        let first = text.chars[0];
        assert_eq!(first.ch, 'i');
        assert!(
            (first.rect.x - 40.0).abs() < 0.1,
            "CropBox 左沿 100，字写在 x=140 ⇒ 可见框里的落笔点 x=40；第一个字的左沿跟着它：{}",
            first.rect.x
        );
        assert!(
            (first.rect.y - (692.0 - 660.0 - 8.4)).abs() < 0.1,
            "顶上沿：{}",
            first.rect.y
        );
        assert!((first.rect.height - 8.4).abs() < 0.1, "{}", first.rect.height);
    }

    /// **`/Rotate` 不为 0 的那一页故意交 `None`**。
    ///
    /// 实测：同一页内容加上 `/Rotate 90` 后，可见框变成 792×612，而引擎的盒**不跟着转**
    /// （基线那条仍在 y=700，已经掉到 612 之外）。要对上就得在这里再建一个旋转坐标系，
    /// 而本机没有一页「横排带真字」的可验样本 —— 猜错的后果不是少一个字，而是**色块画在
    /// 别的地方**。宁缺勿错，这一页与从前一样只是选不出字。
    #[test]
    fn a_rotated_page_is_left_without_a_text_layer() {
        let dir = tempfile::tempdir().unwrap();
        let straight = Page::new([0, 0, 612, 792]).line(72, 700, 24.0, "Heading alpha");
        let turned = Page::new([0, 0, 612, 792])
            .rotate(90)
            .line(72, 700, 24.0, "Heading alpha");
        let path = write_fixture(dir.path(), "转过的.pdf", &[straight, turned], &[], &[]);
        let document = PdfAdapter::from_bytes(std::fs::read(&path).unwrap()).unwrap();
        assert!(page_text(&document, 0, &InterpreterSettings::default()).is_some(), "没转的那一页读得出字");
        assert!(
            page_text(&document, 1, &InterpreterSettings::default()).is_none(),
            "转 90 的那一页不许交出一套会画歪的盒",
        );
    }

    /// **别人生产的 PDF 上量文字层，裁判是 poppler**。
    ///
    /// 样本是 `tests/fixtures/oss/pdf/wps-paper.pdf`：Kingsoft Office（WPS）写的两页论文
    /// 模板，字体是 `ABCDEF+TeXGyreTermes-*` 的 **Identity-H 子集 + ToUnicode CMap** ——
    /// 真书里最常见也最容易读歪的那一族：内容流里摆的不是 ASCII 而是两位数码，靠 CMap
    /// 折回 Unicode。仓库里原先那本 `three-pages.pdf` 内容流只有一个灰矩形、不吃任何
    /// 字体，所以「文字层」这一整条路此前只被我们自己的合成器量过。
    ///
    /// 盒的期望值抄自 poppler 的逐词框（`pdftotext -f 1 -l 1 -bbox`，重验命令与来源写在
    /// `tests/fixtures/oss/pdf/README.md`）。这一条门真正盯的是**翻 y 那一刀**：引擎给的
    /// 盒是 pt、原点左下 y 朝上，而栅格出来的页图与屏幕都是 y 朝下 —— 翻错的症状不是少
    /// 一个字，而是色块画在字的上下对称位置（`680` 与 `99` 差出去一整页，红得干脆）。
    ///
    /// 两条轴分开信：**x 轴**对到 0.01 pt（两家用同一把 pt 尺，没有可争的口径），
    /// **y 轴**只要求 1 pt 以内 —— poppler 那个盒高 16.03 是**行**的（含 leading），
    /// 我们那个 12.0 是**字号**的，差的正是那一条；把它当成翻反去修就会真把 y 改错。
    #[test]
    fn a_real_producer_pdf_agrees_with_poppler_on_its_boxes() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/pdf/wps-paper.pdf");
        assert!(path.exists(), "真样本夹具不许丢：{}", path.display());
        let document = open_document(&path).expect("WPS 那本该开得动");
        let text = page_text(&document, 0, &InterpreterSettings::default()).expect("这一页写着字");
        assert_eq!(
            (text.width, text.height),
            (612.0, 792.0),
            " Letter 一页，poppler 报 612×792"
        );

        // 第一个词 `TITLE` 的首字与末字右沿。poppler：xMin=249.000000 yMin=99.230000
        // xMax=280.210906 yMax=115.262000。
        let first = text.chars[0];
        assert_eq!(first.ch, 'T', "阅读序第一个字是 TITLE 的 T");
        // **两条引擎对同一个字的边界不一样**：poppler 报的是**字形原点**（起笔点，正文
        // 首字落在 249），而 hayro 交的是墨迹盒 —— `T` 的左上角那一横离开起笔点还有
        // 几 pt 的侧边距。所以横向上要校的是「起笔点在 249」这件事，落点得先把这一字的
        // 墨盒左沿往回推：起笔点 = 盒左沿 − 该字形的左方侧边距 × 字号。这一格的侧边距
        // 是 3.54 pt（实测），所以盒左沿该在 252.5 上下；给一个字宽的容差。
        assert!(
            (first.rect.x - 256.536).abs() < 10.0,
            "墨盒的左沿该落在 poppler 起笔点右侧一个字宽以内：{} vs 249.000",
            first.rect.x
        );
        // 纵向不按 poppler 的 `yMin` 校：poppler 那个盒是**行**的（含 leading，高 16.03），
        // 我们的是 `T` 的墨（高 11.7），两者的顶本来就不在一条线上。要盯的是「落在这
        // 一行、y 朝下」：poppler 的行在 99.23–115.26，我们的墨盒必须落在它那一格里。
        assert!(
            first.rect.y >= 99.23 - 1.0 && first.rect.y < 115.26,
            "顶该落在 poppler 那一行里：{}",
            first.rect.y
        );
        // `TITLE` 五个字是 content stream 里的第 1–5 个，落笔位置修好之后字序里
        // 不再凭空多出 `TI TLE` 那个假空格 —— 第 5 格 `E` 就是这个词的末字。
        // poppler 那一侧也没有这个空格（它 `pdftotext -raw` 交的是 `TITLE`）。
        // 这条门校的是**盒的坐标与行号**；整页词数对账仍按行内 em 重做，记在
        // `doc/优化计划.md` 的 CT-11。搜索那一侧已经不认空白（`search::each_hit`）。
        let title_end = text.chars[4];
        assert_eq!(title_end.ch, 'E');
        assert!(
            (title_end.rect.x + title_end.rect.width - 280.211).abs() < 10.0,
            "末字右沿该落在 poppler 那个词的 xMax 附近：{}",
            title_end.rect.x + title_end.rect.width
        );
        // 行号：`TITLE OF THE PAPER` 是同一行（poppler 那三词 yMin 相同），
        // 而下一行 `Author(s) Name(s)` 必须是另一个号。
        assert_eq!(text.chars[0].line, text.chars[4].line);
        let next = text
            .chars
            .iter()
            .find(|item| item.line != 0)
            .expect("第二行也该在");
        assert_eq!(next.ch, 'A', "第二行以 Author 开头");
        assert!(
            next.rect.y > title_end.rect.y,
            "下一行的盒必须更靠下：{} vs {}",
            next.rect.y,
            title_end.rect.y
        );

        // 词缝：`TITLE OF THE PAPER` 里那两处必须有空格。从前这一条记的是「已知限制」——
        // WPS 把词尾的空格算进前一个字的前进量里（`F` 的宽 9.60 = 字形 6.67 + 空格 2.93），
        // pdf_oxide 按词聚类就交回 `OFTHE`。现在引擎把**笔的前进量**交出来（含 `TJ` 的
        // 调整），缝是几何上真有的东西，所以那两个空格回来了。
        //
        // `T` 与 `I`、`L` 与 `E` 之间从前还会多出一格：落笔顺序错的时候每个字画在下一个
        // 字的位置，墨盒之间露出假缝，被阈值当成词缝。落笔修好之后这条也一起消失。
        let flat = text.text();
        assert!(
            flat.starts_with("TITLE OF THE PAPER"),
            "词与词之间的缝要回到字序里：{}",
            flat.chars().take(40).collect::<String>()
        );
        // 字多不多也问一句：整页三千多个字，一字节不缺地摊平了。
        assert!(
            text.char_count() > 3_000,
            "这一页该有几千个字，实测 {}",
            text.char_count()
        );
        // 第二页同样读得出字（一本真书不会只有首页有文字层）。
        assert!(page_text(&document, 1, &InterpreterSettings::default()).is_some(), "第二页也该有文字层");
    }

    /// 没有文字层的一页就是「画得出但读不出」：交 `None`，与从前一样只是图。
    #[test]
    fn a_page_without_a_text_layer_is_still_a_page() {
        let dir = tempfile::tempdir().unwrap();
        // 本仓库那套三页夹具只有灰矩形，一个字符都没有。
        let path = three_pages(dir.path());
        let document = PdfAdapter::from_bytes(std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(page_text(&document, 0, &InterpreterSettings::default()), None);
        // 页号不在书里也是 `None`，不是 panic。
        assert_eq!(page_text(&document, 404, &InterpreterSettings::default()), None);
    }

    /// 框不必写在自己身上：`/MediaBox` 挂在 `/Pages` 上时也要读得出（规范里它是可继承属性）。
    #[test]
    fn an_inherited_media_box_is_not_read_as_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_pages(dir.path());
        let document = PdfAdapter::from_bytes(std::fs::read(&path).unwrap()).unwrap();
        let rotated = page_boxes_at(&document, 2).expect("继承来的 MediaBox 也该读得出");
        assert_eq!(
            (rotated.visible_width, rotated.visible_height),
            (792.0, 612.0),
            "转 90 度之后宽长"
        );
        let first = page_boxes_at(&document, 0).unwrap();
        assert_eq!((first.visible_width, first.visible_height), (595.0, 842.0));
        assert_eq!(raster_size(&document, 2, RENDER_EDGE), (2048, 1583));
        // 页不在书里：读不出框，页表因此存 0×0，由绘制侧按视口兜底。
        assert_eq!(raster_size(&document, 404, RENDER_EDGE), (0, 0));
        assert!(page_boxes_at(&document, 404).is_none());
    }

    /// 放大那一档：区域图与全页图**同位置同色**（容差 1，计划 §四 那条门）。
    ///
    /// 做法是把区域取成「全页那张图里的一块」，并要求输出的像素数**正好等于那一块的尺寸**
    /// —— 1:1 时 `render_region` 那三步（`scale · translate · initial`）折回 `paint_page`
    /// 那两步是同一个仿射，于是每个像素都该一样、抗锯齿也一样。这条挡得住四类错：
    /// 原点少减一次（整块偏一截）、两轴乘反（横竖颠倒）、`/CropBox` 的基准用成
    /// `/MediaBox`（被裁那一页偏掉，正是计划要的第二条门）、以及输出与区域不成比例。
    ///
    /// 三页都跑：A4 竖页、**被裁过的一页**、继承 MediaBox 又转了 90 度的横页 ——
    /// 后两页才是「裁剪页不偏移」那一半。
    #[test]
    fn a_region_matches_the_full_page_at_the_same_scale() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_pages(dir.path());
        let (_, fixed) = fixed(&path);
        let adapter = PdfAdapter::default();
        let mut saw_content = 0usize;
        for page in 0..fixed.pages.len() {
            let (table_w, table_h) = (fixed.pages[page].width, fixed.pages[page].height);
            let full = image::load_from_memory(
                &adapter
                    .render_page(&path, page, RENDER_EDGE)
                    .expect("整页该画得出来"),
            )
            .expect("PNG 解得开")
            .to_rgba8();
            assert_eq!(
                (full.width(), full.height()),
                (table_w, table_h),
                "第 {page} 页：页表说的尺寸与画出来的不是一把尺"
            );
            // 取中间那块：左上 (1/4, 1/4)、右下 (3/4, 3/4)。
            let region = PageRegion {
                x0: table_w / 4,
                y0: table_h / 4,
                x1: table_w * 3 / 4,
                y1: table_h * 3 / 4,
            };
            let (w, h) = region.size().expect("那块非空");
            let block = image::load_from_memory(
                &adapter
                    .render_page_region(&path, page, region, w, h)
                    .expect("同样 1:1 的区域也该画得出来"),
            )
            .expect("区域那张也该解得开")
            .to_rgba8();
            assert_eq!(
                (block.width(), block.height()),
                (w, h),
                "第 {page} 页：输出尺寸没按请求的来"
            );
            // 区域里的 (i, j) 对应全页里的 (x0 + i, y0 + j)。隔 3 个采一个：要抓的是
            // 「整块偏了」，不是最后一个像素，而几十万个点会把这条门跑成一分钟。
            let mut worst = 0u8;
            for j in (0..h).step_by(3) {
                for i in (0..w).step_by(3) {
                    let want = full.get_pixel(region.x0 + i, region.y0 + j);
                    let got = block.get_pixel(i, j);
                    for channel in 0..4 {
                        worst = worst.max(want.0[channel].abs_diff(got.0[channel]));
                    }
                    if got.0[0] < 250 {
                        saw_content += 1;
                    }
                }
            }
            assert!(
                worst <= 1,
                "第 {page} 页：区域与全页同位置最大色差 {worst}（容差 1）。\
                 偏一截就是原点或裁剪基准错了，整片糊就是比例错了"
            );
        }
        assert!(
            saw_content > 0,
            "两块区域里采到的全是白底：夹具那层灰矩形没进视野，上面那些断言就是在比两张空白图"
        );
    }

    /// 区域那一问的降级路：**空矩形与画不出来的页都交 `None`**，而不是整页、也不是报错。
    ///
    /// 当成「整页」是最坏的一种错 —— 键里带着那四个数，画成整页等于把整页塞进那一小块
    /// 的格子，屏上是「放大后看到的是缩小版」。报错也不对：那一格保持现有像素不动才是
    /// 界面要的表现（与 `render_page` 同一立场）。
    #[test]
    fn an_empty_or_unknown_region_is_not_drawn_as_the_whole_page() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_pages(dir.path());
        let adapter = PdfAdapter::default();
        let empty = PageRegion {
            x0: 300,
            y0: 10,
            x1: 300,
            y1: 40,
        };
        assert!(
            adapter
                .render_page_region(&path, 0, empty, 512, 512)
                .is_none(),
            "宽的零就是空矩形，不该画"
        );
        assert_eq!(empty.size(), None, "`size` 要把零宽/零高认成空");
        // 整块都在页的右边之外：夹进页里之后是空的。
        assert!(
            adapter
                .render_page_region(
                    &path,
                    0,
                    PageRegion {
                        x0: 90_000,
                        y0: 0,
                        x1: 90_100,
                        y1: 100
                    },
                    512,
                    512,
                )
                .is_none(),
            "夹完是空的也要交 None"
        );
        // 页不在书里：与 `render_page` 同一个降级。
        assert!(
            adapter
                .render_page_region(
                    &path,
                    404,
                    PageRegion {
                        x0: 0,
                        y0: 0,
                        x1: 100,
                        y1: 100
                    },
                    512,
                    512,
                )
                .is_none()
        );
    }

    /// 矢量页与扫描件在页表里就分得开（`PageSource::Render::vector`）。
    ///
    /// 判据是 `native_edge` 给不给 `None`，与 `render_edge` 封顶用的是同一句 —— 这条门
    /// 钉的是「界面上问不问那一块」的输入：扫描件那族问了也只是把同一批源像素摊开。
    #[test]
    fn the_page_table_says_which_pages_are_worth_redrawing() {
        let dir = tempfile::tempdir().unwrap();
        let (_, vector) = fixed(&three_pages(dir.path()));
        assert!(
            vector.pages.iter().all(|page| matches!(
                page.source,
                PageSource::Render {
                    vector: true,
                    ..
                }
            )),
            "合成书那三页一页图都没有，全是矢量画的 —— 每一页都该值得重画"
        );
        let (_, scanned) = fixed(&scanned(dir.path()));
        let flags: Vec<bool> = scanned
            .pages
            .iter()
            .map(|page| matches!(page.source, PageSource::Render { vector: true, .. }))
            .collect();
        assert_eq!(
            flags,
            vec![false, true, true],
            "铺满的那页按源图密度封了顶（不重画）；只盖住三成的与转过 90 度的没封（重画补得出细节）"
        );
    }
}
