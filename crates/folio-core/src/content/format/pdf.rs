//! PDF：一页是既定的画布，**内容流里没有「行」这个事实** —— 像素要算出来才有。
//!
//! 所以它归定版式那一族（[`FixedDoc`]），与 CBZ 同族而分歧只有一处：漫画的一页本身就是
//! 容器里的一条图（按 `href` 取字节），PDF 的一页要**栅格化**才有字节，所以页表里那条是
//! `PageSource::Render { index }`，字节由 [`DocumentProvider::render_page`] 现算。
//!
//! 引擎是 `pdf_oxide`。它把整个 `rendering` 模块放在一条 feature 后面（0.3.78 的
//! `default` 只有 `icc` 与 `legacy-crypto`，不开就根本不存在画页的函数），选它与那条
//! feature 的实测代价写在根 `Cargo.toml` 上。本模块只碰三件事：读页表与书目
//! （`document`）、读书签（`outline`）、画一页（`rendering`）。
//!
//! **文字层不在这里**：`extract_words` 那一族实测存在（带 pt 的盒子，原点左下、y 朝上，
//! 而渲染出的位图原点在左上 —— 接进去要翻 y）。把它接进选中与批注是下一刀，届时的落点是
//! [`FixedPage`] 的页号加页内盒，不用改库里的磁盘形状。
//!
//! ## 每次取页重开一次文档
//!
//! `PdfDocument::open` 把整本读进内存再懒解结构，所以「重开」不是免费的。实测两档
//! （release、本机、长边 2048）：`简历.pdf` 重开一趟再画第 0 页 61 ms，留住句柄画第二趟
//! 56 ms；`sample.pdf`（3.6 MB、45 页、图多）重开 374 ms，留住句柄 425 ms。差值在一成
//! 以内，而代价只是把文件字节再读一遍 —— 换回来的是**没有全局状态**：一份进程级的句柄表
//! 要么按本数封顶（谁被淘汰要说得清），要么一直涨。与 CBZ / EPUB「按 href 现取、每次重开
//! 容器」那条立场一致（`ui::foundation::images::document_source`）。
//!
//! 要记的另一笔是**峰值驻留**：画一页时内存里同时有整本字节、一块 pixmap、一段 PNG。
//! `sample.pdf` 连画三页测得峰值 276 MB（桌面）。图多的书在手机上先撞这一格，处置是把
//! `RENDER_EDGE` 往下调（一个常数、跟着 `INDEX_VERSION` 走），而不是再加一层缓存。
//!
//! ## 图片页按源图自己的密度封顶
//!
//! `RENDER_EDGE` 是**上限**，不是每一页的档位。扫描本书（画面就是几张 JPEG 铺满一页）
//! 按 2048 画，多出来的像素全是插值：`hugG.pdf`（13.9 MB、43 页、A4、每页 3–4 张
//! 120 ppi 的 JPEG，`Microsoft: Print To PDF` 生产）实测**每页 816–932 ms**、PNG 平均
//! 3.6 MB、解成位图 11.3 MB。按源图封顶（这一本 1232）之后**每页中位 106 ms**（含每次
//! 重开文档；留住句柄那一趟 93 ms），全书 37 s → 4.3 s，单页位图 4.1 MB —— 图集那
//! 24 MB 的封顶从「两页」变成「五页」。
//!
//! 档位在下标那一趟就算好（`page_image_handles` 只读结构不解码）并落进页表，所以读的
//! 每一页不必重新问一次；界面那一侧按页表的长边现画（`reader::model::fixed_book`），
//! 不是按常数。入库那一趟因此从 83 ms 到 110 ms —— 买的正是上面那几个数。
//!
//! **宁慢勿错的三条**：转过的页（图的盒在未转空间里，长边却按转过的方向换过）、
//! 图盖不满页的（剩下的面积是矢量画的）、图柄读不出的 —— 都按上限画，不封顶。
//! 判断写在 [`native_edge`]。

use std::path::Path;

use pdf_oxide::document::PdfDocument;
use pdf_oxide::object::Object;
use pdf_oxide::optional_content::decode_pdf_text_string;
use pdf_oxide::outline::{Destination, OutlineItem};
use pdf_oxide::rendering::{ImageFormat, RenderOptions, render_page_fit};

use super::{
    Content, DocMeta, DocumentProvider, FixedDoc, FixedPage, Format, PageChar, PageRect,
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

/// 封面栅格的长边，与 EPUB / FB2 / CBZ 那条 `COVER_MAX_EDGE` 同一口径。
///
/// 单独一个数而不是「按 `RENDER_EDGE` 画完再缩」：封面只有首页，直接按目标尺寸画少一块
/// pixmap、也少一次重编码。
const COVER_EDGE: u32 = 512;

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

pub struct PdfAdapter;

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
        let document = open_document(path)?;
        read_pdf(
            path,
            &document,
            Format::Pdf,
            stem_of(path),
            std::fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or(0),
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
    fn render_page(&self, path: &Path, page: usize, edge: u32) -> Option<Vec<u8>> {
        PdfDocument::open(path)
            .ok()
            .and_then(|document| render_png(&document, page, edge))
    }

    /// 这一页的文字层（选中与划线要的那个序 + 逐字盒）。
    ///
    /// 与 [`Self::render_page`] 分开问：画得出这一页不等于读得出这一页的字（扫描本没有
    /// 文字层、这一族也没有密码入口）。每次现开一份 `PdfDocument` 与栅格化同一趟代价
    /// （见模块头「每次取页重开一次文档」那笔账），一次取字实测 0.3–10 ms。
    fn page_text(&self, path: &Path, page: usize) -> Option<PageText> {
        PdfDocument::open(path)
            .ok()
            .and_then(|document| page_text(&document, page))
    }

    /// 全书每一页的字序，**容器只开一次**（契约与那条「必须与逐页问逐字节相同」写在
    /// [`DocumentProvider::page_texts`]）。
    ///
    /// 为什么专门开这一问：逐页问那一问每页重开一次 `PdfDocument`（这一族没有全局句柄），
    /// 实测一份 3.6 MB、45 页的书重开一趟 374 ms —— 搜索要的正是整本一遍，69 页按那条路
    /// 走是二十几秒，读者每敲一个字付一次。开一次再逐页抽，抽取本身实测每页 0.3–10 ms。
    fn page_texts(&self, path: &Path) -> Option<Vec<String>> {
        let document = PdfDocument::open(path).ok()?;
        let pages = page_count(path, &document).ok()?;
        Some(
            (0..pages)
                .map(|page| {
                    page_text(&document, page)
                        .map(|text| text.text())
                        .unwrap_or_default()
                })
                .collect(),
        )
    }
}

/// 一份已打开的 PDF → 这一页的文字层。PDF 与 AZW4 共用（与 [`read_pdf`]、[`render_png`]
/// 同一条分工：只差文档从哪儿来）。
///
/// 引擎交回的是 **pt、y 朝上**的盒（拿 Ghostscript 写的、位置自己定的那份量出来的：
/// 基线在 `bbox.y`，`height` 恰等于字号，往 y 增方向长），而栅格出来的页图与屏幕都是
/// y 朝下 —— 翻这一刀只有这一处该做，所以盒在这里就翻好，界面拿到的与页图同一个方向。
/// 裁过的那一页还要按可见框裁掉左边与上边（`left` / `top`），不然色块整体偏一截。
///
/// **`/Rotate` 不为 0 的那一页交 `None`**：图是转过的而盒是没转的，要对上就得在这里再
/// 建一个旋转坐标系，而本机没有一页「横排带真字」的样本可量（`three-pages.pdf` 那份
/// 转 90 的夹具一个字符都没有）。猜错的后果不是少一个字，而是**色块画在别的地方** ——
/// 那一格比「这一页选不出字」难查得多，而后者与从前逐字节一样。
pub(crate) fn page_text(document: &PdfDocument, page: usize) -> Option<PageText> {
    let boxes = page_boxes(document, page)?;
    if boxes.rotate != 0 {
        return None;
    }
    let lines = document.extract_text_lines(page).ok()?;
    let mut chars: Vec<PageChar> = Vec::new();
    for (line, entry) in lines.iter().enumerate() {
        let line = line as u32;
        let mut right: Option<f32> = None;
        for piece in &entry.words {
            let (Some(first), Some(last)) = (piece.chars.first(), piece.chars.last()) else {
                continue;
            };
            // **词与词之间那个缝要补成一个空格**：引擎是按字间距聚类的，所以「两个词」这个
            // 事实本身就意味着中间空了一截 —— 盒子里没有那个空格，直接摊平复制出来就是
            // 一串粘在一起的词（实测 `Heading alpha` 交回 `Heading` + `alpha` 两个词）。
            // 补的口径只有一条：后一个词的左沿在前一个词的右沿之后。中文那一族整行常常是
            // 一个词（见 `PageText::snap` 那条实测），缝不存在，于是一个多余的空格也不掺。
            //
            // 为什么**不**按词边界无条件补：一份 Word / WPS 排出来的书里，同一个词常常
            // 被拆成几条样式片段（前半粗体、后半斜体），它们左右沿正好接上，按词边界补
            // 就补出一个 `Hel lo`。缝是几何事实，比聚类给的词编号可信。
            if first.bbox.x > right.unwrap_or(first.bbox.x) {
                let previous = right.unwrap_or(first.bbox.x);
                chars.push(PageChar {
                    ch: ' ',
                    rect: PageRect {
                        x: previous - boxes.left,
                        y: boxes.top - (first.bbox.y + first.bbox.height),
                        width: first.bbox.x - previous,
                        height: first.bbox.height,
                    },
                    line,
                });
            }
            for item in &piece.chars {
                chars.push(PageChar {
                    ch: item.char,
                    rect: page_rect(
                        item.bbox.x,
                        item.bbox.y,
                        item.bbox.width,
                        item.bbox.height,
                        &boxes,
                    ),
                    line,
                });
            }
            right = Some(last.bbox.x + last.bbox.width);
        }
    }
    (!chars.is_empty()).then_some(PageText {
        width: boxes.visible_width,
        height: boxes.visible_height,
        chars,
    })
}

/// 引擎的一个盒（**pt，基线在 `y`、往 y 增方向长**）→ 可见框里的那个盒
/// （**原点左上、y 朝下**，并按可见框裁掉左沿与上沿）。
///
/// 翻与裁都只有这一处做，理由见 [`super::PageRect`]。分成一条函数是因为那两格
/// （裁过的页、翻反的 y）各自偏多少都要能单独钉一条门 —— 混在摊平那段循环里，
/// 就只能整条路径一起验，而那条路径上还挂着「空格补不补」这件别的事。
fn page_rect(x: f32, y: f32, width: f32, height: f32, boxes: &PageBoxes) -> PageRect {
    PageRect {
        x: x - boxes.left,
        y: boxes.top - (y + height),
        width,
        height,
    }
}

/// 一份已打开的 PDF → 页表 + 书签 + 书目 + 封面。
///
/// PDF 与 AZW4（PalmDB 里的 PDF）共用这一份：两者只差「`PdfDocument` 是从文件打开
/// 还是从记录字节 `from_bytes`」，页表与栅格化的规则一个字都不该分叉。
///
/// `format` 是**这本书在库里报的身份**（`Pdf` 或 `Azw4`），不是 PDF 引擎的格式；
/// `title_fallback` 在 PDF 自己没有 `/Title` 时用（PDF 是文件主名，AZW4 是 PalmDB 名）。
pub(crate) fn read_pdf(
    path: &Path,
    document: &PdfDocument,
    format: Format,
    title_fallback: String,
    size: u64,
) -> Result<(DocMeta, Content)> {
    let pages = page_count(path, document)?;
    if pages == 0 {
        return Err(unsupported(path, "这本 PDF 的页树里一页都没有"));
    }

    let mut table = Vec::with_capacity(pages);
    for page in 0..pages {
        // 宽高是**封顶之后**的像素（`render_edge`）：扫描件那一页按源图密度算，
        // 矢量页仍是 `RENDER_EDGE`。栅格化那一趟读同一句，两边因此永远同一个数。
        let (width, height) = raster_size(document, page, RENDER_EDGE);
        table.push(FixedPage {
            source: PageSource::Render { index: page as u32 },
            width,
            height,
            // 一页就是一张栅格，没有「主图之外再摆一张」这一档（那是 EPUB 跨页的事）。
            extra_images: Vec::new(),
        });
    }
    // 首屏尺寸取**第一页尺寸知道的那一页**：整本首页读不出可见盒（写坏的 `/MediaBox`）
    // 时不至于让整本的缩放基准变成 0，而后面那些页各自有自己的尺寸。
    let (width, height) = table
        .iter()
        .find(|page| page.width > 0 && page.height > 0)
        .map(|page| (page.width, page.height))
        .unwrap_or((0, 0));

    let toc = outline(document, pages);
    let cover = render_png(document, 0, COVER_EDGE);

    let meta = DocMeta {
        title: info_string(document, "Title").unwrap_or(title_fallback),
        // `/Author` 没有列表语法，多个作者之间用逗号还是「 and 」由生产者定。不切：
        // `Smith, J.` 这种写法切一刀就把一个人的名字拆成两个作者。
        authors: info_string(document, "Author")
            .map(|author| vec![author])
            .unwrap_or_default(),
        subjects: info_string(document, "Keywords")
            .map(|keywords| split_keywords(&keywords))
            .unwrap_or_default(),
        // PDF 的书目里没有「出版社」这一项。`/Producer` 写的是把这份 PDF 生成出来的
        // 工具（`dvipdfmx`、`Typst 0.15.1`），不是出版方 —— 所以不填：信息屏「有值才摆
        // 那一行」的规矩会让这一行整个消失，比摆一个错数诚实。
        publisher: String::new(),
        published: info_string(document, "CreationDate")
            .and_then(|date| pdf_date_to_day(&date))
            .unwrap_or_default(),
        // 语言：`/Info` 没有这一项，`/Lang` 在目录字典里而 pdf_oxide 不读它。
        // 交空串 = 「这本书没说」，界面上那一行不摆。
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
/// 口令加密的 PDF 到这一步常常**还能打开**（结构是明文，对象流才是加密的），失败往往
/// 推迟到读页树那一趟，所以 `EncryptedPdf` 这一条在 [`open_document`] 与
/// [`page_count`] 两处都判 —— 少判一处，读者的报表上就是「读不动」而不是「换一本」。
fn open_document(path: &Path) -> Result<PdfDocument> {
    PdfDocument::open(path).map_err(|err| failure(path, err))
}

fn page_count(path: &Path, document: &PdfDocument) -> Result<usize> {
    document.page_count().map_err(|err| failure(path, err))
}

/// `pdf_oxide` 的错误 → 我们的两档：加密（换一本）与读不动（重导一次试试）。
pub(crate) fn failure(path: &Path, err: pdf_oxide::Error) -> crate::Error {
    match err {
        pdf_oxide::Error::EncryptedPdf => crate::Error::Encrypted {
            name: file_name(path),
            method: "PDF /Encrypt（口令或权限加密）".to_string(),
        },
        other => unsupported(path, &format!("结构读不出来（{other}），文件可能被截断过")),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

fn unsupported(path: &Path, why: &str) -> crate::Error {
    crate::Error::UnsupportedFormat(format!("{}：{why}", file_name(path)))
}

/// 画一页并交回 PNG 字节；任何一步读不出来都只给 `None`。
pub(crate) fn render_png(document: &PdfDocument, page: usize, edge: u32) -> Option<Vec<u8>> {
    let mut options = RenderOptions::default();
    options.format = ImageFormat::Png;
    let (fit_w, fit_h) = fit_box(document, page, edge);
    render_page_fit(document, page, fit_w, fit_h, &options)
        .ok()
        .map(|image| image.data)
}

/// 一页的两个框（**点**，都按 `/Rotate` 交换过长宽）：可见框 + MediaBox。
///
/// 为什么要两个框一起取：`render_page_fit` 定比例用的是 **MediaBox**，而它输出的那张
/// 图是按 **CropBox ∩ MediaBox** 裁出来的（`rendering::page_render_box`）。被裁过的页
/// （出血稿）于是会短一截 —— 实测一页 `MediaBox 400×600` 套 `CropBox 300×500`、要 1024
/// 那一档时，画出来是 `512×853` 而不是 `629×1024`。按可见框反推目标框（[`fit_box`]）
/// 才对得上，而页表里存的尺寸也按可见框算（[`raster_size`]）：读者看到的就那么大。
fn page_boxes(document: &PdfDocument, page: usize) -> Option<PageBoxes> {
    let (x0, y0, x1, y1) = document.get_page_visible_box(page).ok()?;
    let (mx0, my0, mx1, my1) = document.get_page_media_box(page).ok()?;
    let swap = matches!(
        document
            .get_page_rotation(page)
            .unwrap_or(0)
            .rem_euclid(360),
        90 | 270
    );
    let extent = |ax: f32, ay: f32, bx: f32, by: f32| {
        let (w, h) = ((bx - ax).abs(), (by - ay).abs());
        if swap { (h, w) } else { (w, h) }
    };
    let (visible_width, visible_height) = extent(x0, y0, x1, y1);
    let (media_width, media_height) = extent(mx0, my0, mx1, my1);
    // 可见框的左沿与上沿（上沿是 y 朝上的那一侧，即 `y0`/`y1` 里较大的那个）。
    // 文字层的盒要按它裁掉偏移：被裁过的页只取尺寸不取原点，色块就会整体偏一截，
    // 而那种偏在屏上看得出、说不清。转过的页另说（见 `page_text` 里 `rotate` 那一格）。
    let left = x0.min(x1);
    let top = y0.max(y1);
    (visible_width > 0.0 && visible_height > 0.0 && media_width > 0.0 && media_height > 0.0)
        .then_some(PageBoxes {
            visible_width,
            visible_height,
            media_width,
            media_height,
            left,
            top,
            rotate: document
                .get_page_rotation(page)
                .unwrap_or(0)
                .rem_euclid(360),
        })
}

struct PageBoxes {
    visible_width: f32,
    visible_height: f32,
    media_width: f32,
    media_height: f32,
    /// 可见框的左沿（未转空间，pt）与上沿（y 朝上的那一侧）。文字层的盒按它裁与翻。
    left: f32,
    top: f32,
    /// 页面自己的 `/Rotate`（继承来的已经折进来）。
    rotate: i32,
}

impl PageBoxes {
    /// 把可见长边折到 `edge` 的那个比例。
    fn scale(&self, edge: u32) -> f32 {
        edge as f32 / self.visible_width.max(self.visible_height)
    }
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
fn raster_size(document: &PdfDocument, page: usize, edge: u32) -> (u32, u32) {
    let Some(boxes) = page_boxes(document, page) else {
        return (0, 0);
    };
    let edge = render_edge(document, page, &boxes, edge);
    let scale = boxes.scale(edge);
    (
        (boxes.visible_width * scale).round().max(1.0) as u32,
        (boxes.visible_height * scale).round().max(1.0) as u32,
    )
}

/// 这一页实际按哪一档画：`edge` 是上限，图片页被源图密度压到更低一档。
///
/// 页表（[`raster_size`]）与栅格化（[`fit_box`]）问的是**同一句**，所以「页表说的尺寸」
/// 与「画出来的像素」不会一处封一处不封 —— 两处分头写，症状是图集按 2048 记账而图只有
/// 1232，一张页虚报掉两页的名额。
///
/// 下限夹在 `edge` 以内：调用方要 256 那一档（契约测试、封面）时不该被抬到 360。
fn render_edge(document: &PdfDocument, page: usize, boxes: &PageBoxes, edge: u32) -> u32 {
    native_edge(document, page, boxes)
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
/// 与 [`fit_box`] 那条「按可见框反推」同一立场：源图的 bbox 是页上真正印出来的大小，
/// 用 MediaBox 或页宽去除都会在被裁过的页上算偏。
fn native_edge(document: &PdfDocument, page: usize, boxes: &PageBoxes) -> Option<u32> {
    // 转过的页：`page_image_handles` 的 bbox 在**未转**的用户空间里，而这里的可见长边
    // 已经按 `/Rotate` 换过方向 —— 拿它们相除会把 90 度的那一页算成另一个密度
    // （高比宽、宽比高），而错的档位在屏上是糊掉的一页，不是少一个字符。宁可不封。
    if boxes.rotate != 0 {
        return None;
    }
    let handles = document.page_image_handles(page).ok()?;
    let page_area = boxes.visible_width * boxes.visible_height;
    if page_area <= 0.0 {
        return None;
    }
    let mut covered = 0.0_f32;
    let mut density = 0.0_f32;
    for handle in &handles {
        let (width, height) = (handle.bbox.width.abs(), handle.bbox.height.abs());
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

/// 喂给 `render_page_fit` 的目标框：**按 MediaBox 写、但比例按可见框反推**。
///
/// 那条函数内部取 `min(框宽 / MediaBox 宽, 框高 / MediaBox 高)` 当比例，而输出按可见框
/// 裁，所以要让可见长边正好等于 `edge`，框就得填成 `MediaBox × (edge / 可见长边)`。
/// MediaBox 与可见框重合的常规页（几乎每一本公版书）算回来就是 `edge × edge` 上下，
/// 与从前直接交 `edge × edge` 一模一样 —— 这一格真正服务的是被裁过的那一类。
///
/// `edge` 在这里再过一次 [`render_edge`]：图片页按源图密度封顶之后，目标框也按那一档反推
/// （否则 1232 的图配 2048 的框，引擎按 2048 的比例缩放，出来仍是 1447 宽）。
fn fit_box(document: &PdfDocument, page: usize, edge: u32) -> (u32, u32) {
    let Some(boxes) = page_boxes(document, page) else {
        return (edge, edge);
    };
    let edge = render_edge(document, page, &boxes, edge);
    let scale = boxes.scale(edge);
    (
        (boxes.media_width * scale).round().max(1.0) as u32,
        (boxes.media_height * scale).round().max(1.0) as u32,
    )
}

/// `/Info` 字典里一个字符串字段。
///
/// 自己走 `trailer → /Info → 键` 这一趟，而不是用 pdf_oxide 挂在 `DocumentEditor` 上的
/// `title()` / `author()`：开一个编辑器要先扫一遍全部对象（它的 `find_max_object_id`），
/// 只为读四行书目付不起那一趟。`PdfDocument` 自己只公开了 `Producer` 与 `Creator`
/// 两项，其余照它内部那个私有实现（`document_info_string`）同样的走法解。
fn info_string(document: &PdfDocument, key: &str) -> Option<String> {
    let trailer = document.trailer();
    let info = resolve(document, trailer.as_dict()?.get("Info")?);
    let raw = info.as_dict()?.get(key)?.clone();
    let value = resolve(document, &raw);
    let text = decode_pdf_text_string(value.as_string()?)
        .trim()
        .to_string();
    (!text.is_empty()).then_some(text)
}

/// 间接引用就地解一次；不是引用就原样交回（`/Info` 与它的值两种写法都合法）。
fn resolve(document: &PdfDocument, object: &Object) -> Object {
    match object.as_reference() {
        Some(reference) => document.load_object(reference).unwrap_or(Object::Null),
        None => object.clone(),
    }
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
fn outline(document: &PdfDocument, pages: usize) -> Vec<TocEntry> {
    let items = match document.get_outline() {
        Ok(Some(items)) => items,
        Ok(None) => return Vec::new(),
        Err(err) => {
            tracing::debug!("pdf: 书签读不出来：{err}");
            return Vec::new();
        }
    };
    items
        .into_iter()
        .filter_map(|item| outline_entry(document, item, pages))
        .collect()
}

fn outline_entry(document: &PdfDocument, item: OutlineItem, pages: usize) -> Option<TocEntry> {
    let page = match item.dest? {
        // pdf_oxide 已经把 `/Dest` 与 `/A{GoTo}` 解成 0 基页号了（内部走过
        // `find_page_index`），所以这里不需要自己按页树换算一次。
        Destination::PageIndex(index) => index,
        Destination::Named(name) => document.resolve_named_destination(&name).ok().flatten()?,
    };
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
            .filter_map(|child| outline_entry(document, child, pages))
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
        let font = any_text.then(|| file.reserve());
        if let Some(id) = font {
            file.set(id, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
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
        let (meta, content) = PdfAdapter
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
            &PdfAdapter
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
        let png = PdfAdapter
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
        let png = PdfAdapter
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
        assert_eq!(PdfAdapter.format(), Format::Pdf);
        assert!(PdfAdapter.supports(Path::new("a.pdf")));
        assert!(PdfAdapter.supports(Path::new("A.PDF")));
        assert!(!PdfAdapter.supports(Path::new("a.ps")));
        assert!(!PdfAdapter.supports(Path::new("a.cbz")));
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
                PageSource::Render { index: at } => assert_eq!(*at as usize, index),
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
            let bytes = PdfAdapter
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
        assert!(PdfAdapter.render_page(&path, 99, 512).is_none());
        assert!(
            PdfAdapter
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
        let bytes = PdfAdapter
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
        assert!(PdfAdapter.index(&path).is_err(), "页树里没有一页，就该说不");
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
        let err = PdfAdapter.index(&path).expect_err("只有一个头的文件该失败");
        assert!(
            matches!(err, crate::Error::UnsupportedFormat(_)),
            "该是「读不动」那一档，实际 {err:?}"
        );
        // 空文件与不存在的文件也不许 panic，也不许打开成「一本零页的书」。
        let empty = dir.path().join("空.pdf");
        std::fs::write(&empty, b"").unwrap();
        assert!(PdfAdapter.index(&empty).is_err());
        assert!(PdfAdapter.index(&dir.path().join("不在.pdf")).is_err());
        // 取字节那一侧同一条立场：读不到是 `None`，不是 panic。
        assert!(PdfAdapter.render_page(&path, 0, 512).is_none());
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
                failure(path, pdf_oxide::Error::EncryptedPdf),
                crate::Error::Encrypted { .. }
            ),
            "加密要落进加密那一档"
        );
        let err = failure(path, pdf_oxide::Error::InvalidXref);
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
        let bytes = PdfAdapter
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
        let bytes = PdfAdapter.render_page(&path, 0, 1024).expect("画得出来");
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
        let document = PdfDocument::open(&path).unwrap();
        let text = page_text(&document, 0).expect("这一页写着两行字");
        assert_eq!((text.width, text.height), (612.0, 792.0), "框就是可见框");
        let first = text.chars[0];
        assert_eq!(first.ch, 'H');
        assert_eq!(
            (first.rect.x, first.rect.y, first.rect.height),
            (72.0, 68.0, 24.0)
        );
        assert!(first.rect.width > 0.0, "字的宽度由字体度量给");
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
        assert_eq!(
            (second.rect.y, second.rect.height),
            (792.0 - (660.0 + 12.0), 12.0)
        );
        // 选中一行 = 一个色块（那一行里的字并起来），不是十来个。
        let line_zero: std::ops::Range<usize> =
            0..text.chars.iter().position(|item| item.line == 1).unwrap();
        let rects = text.rects(line_zero);
        assert_eq!(rects.len(), 1, "一行一条色块：{rects:?}");
        assert_eq!(rects[0].x, 72.0);
        assert_eq!(rects[0].y, 68.0);
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
        let document = PdfDocument::open(&path).unwrap();
        let text = page_text(&document, 0).expect("裁完还留着这一行");
        assert_eq!((text.width, text.height), (512.0, 692.0));
        let first = text.chars[0];
        assert_eq!(first.ch, 'i');
        assert_eq!(
            (first.rect.x, first.rect.y, first.rect.height),
            (40.0, 692.0 - (660.0 + 12.0), 12.0),
            "左沿要按裁掉的 100 收进来",
        );
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
        let document = PdfDocument::open(&path).unwrap();
        assert!(page_text(&document, 0).is_some(), "没转的那一页读得出字");
        assert!(
            page_text(&document, 1).is_none(),
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
        let document = PdfDocument::open(&path).expect("WPS 那本该开得动");
        let text = page_text(&document, 0).expect("这一页写着字");
        assert_eq!(
            (text.width, text.height),
            (612.0, 792.0),
            " Letter 一页，poppler 报 612×792"
        );

        // 第一个词 `TITLE` 的首字与末字右沿。poppler：xMin=249.000000 yMin=99.230000
        // xMax=280.210906 yMax=115.262000。
        let first = text.chars[0];
        assert_eq!(first.ch, 'T', "阅读序第一个字是 TITLE 的 T");
        assert!(
            (first.rect.x - 249.0).abs() < 0.01,
            "左沿该与 poppler 逐字对上：{} vs 249.000",
            first.rect.x
        );
        assert!(
            (first.rect.y - 99.23).abs() < 1.0,
            "顶该落在 poppler 那一格附近（差的是行高与字号那一条 leading）：{}",
            first.rect.y
        );
        let title_end = text.chars[4];
        assert_eq!(title_end.ch, 'E');
        assert!(
            (title_end.rect.x + title_end.rect.width - 280.211).abs() < 0.01,
            "末字右沿该落在 poppler 那个词的 xMax 上：{}",
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

        // 这一页**记下一条已知限制**，而不是让它没人说：WPS 那一族把词尾的空格算进了
        // 前一个字的前进量里（`F` 的宽 9.60 = 字形 6.67 + 空格 2.93），于是几何上没有缝、
        // 引擎的聚类也不把它分成两词 —— 摊平出来是 `OFTHE`。补缝那一段注释写了为什么
        // 不在我们这层按词编号硬补（样式分片会补出 `Hel lo`）。
        //
        // 这一条断言的存在理由是「哪天引擎补回空格，这里就红」：那时该改的是这句期望，
        // 而不是让复制出去的串悄悄变好而没人知道为什么。
        let flat = text.text();
        assert!(
            flat.starts_with("TITLE OFTHE PAPER"),
            "实测的形状是词缝被并进前进量（少那两个空格）：{}",
            flat.chars().take(40).collect::<String>()
        );
        // 字多不多也问一句：整页三千多个字，一字节不缺地摊平了。
        assert!(
            text.char_count() > 3_000,
            "这一页该有几千个字，实测 {}",
            text.char_count()
        );
        // 第二页同样读得出字（一本真书不会只有首页有文字层）。
        assert!(page_text(&document, 1).is_some(), "第二页也该有文字层");
    }

    /// 没有文字层的一页就是「画得出但读不出」：交 `None`，与从前一样只是图。
    #[test]
    fn a_page_without_a_text_layer_is_still_a_page() {
        let dir = tempfile::tempdir().unwrap();
        // 本仓库那套三页夹具只有灰矩形，一个字符都没有。
        let path = three_pages(dir.path());
        let document = PdfDocument::open(&path).unwrap();
        assert_eq!(page_text(&document, 0), None);
        // 页号不在书里也是 `None`，不是 panic。
        assert_eq!(page_text(&document, 404), None);
    }

    /// 框不必写在自己身上：`/MediaBox` 挂在 `/Pages` 上时也要读得出（规范里它是可继承属性）。
    #[test]
    fn an_inherited_media_box_is_not_read_as_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_pages(dir.path());
        let document = PdfDocument::open(&path).unwrap();
        let rotated = page_boxes(&document, 2).expect("继承来的 MediaBox 也该读得出");
        assert_eq!(
            (rotated.visible_width, rotated.visible_height),
            (792.0, 612.0),
            "转 90 度之后宽长"
        );
        let first = page_boxes(&document, 0).unwrap();
        assert_eq!((first.visible_width, first.visible_height), (595.0, 842.0));
        assert_eq!(raster_size(&document, 2, RENDER_EDGE), (2048, 1583));
        // 页不在书里：读不出框，页表因此存 0×0，由绘制侧按视口兜底。
        assert_eq!(raster_size(&document, 404, RENDER_EDGE), (0, 0));
        assert!(page_boxes(&document, 404).is_none());
    }
}
