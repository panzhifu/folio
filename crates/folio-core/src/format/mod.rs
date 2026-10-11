use std::path::Path;

use serde::{Deserialize, Serialize};

pub mod cbz;
pub mod content;
pub mod epub;
pub mod fb2;
pub mod html;
pub mod markdown;
pub mod provider;
pub mod woff;
pub mod xhtml;
pub(crate) mod zip;

pub use content::{Content, ContentKind, FixedDoc, FixedPage, FlowDoc, PageSource};
pub use provider::{DocumentProvider, provider_for, providers, supported_extensions, supports_extension};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Format {
    Markdown,
    Text,
    Epub,
    /// FictionBook 2：一种 XML 单文件的电子书格式。
    Fb2,
    /// 单个 HTML / XHTML 文件。
    Html,
    /// Comic Book Zip：一个 zip，里面按页排着图片。定版式。
    Cbz,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Markdown => "markdown",
            Format::Text => "text",
            Format::Epub => "epub",
            Format::Fb2 => "fb2",
            Format::Html => "html",
            Format::Cbz => "cbz",
        }
    }

    pub fn parse(s: &str) -> Option<Format> {
        match s {
            "markdown" => Some(Format::Markdown),
            "text" => Some(Format::Text),
            "epub" => Some(Format::Epub),
            "fb2" => Some(Format::Fb2),
            "html" => Some(Format::Html),
            "cbz" => Some(Format::Cbz),
            _ => None,
        }
    }
}

/// 索引方案的版本。**分章规则或位置方案一改就要加一。**
///
/// 库里的位置（[`ChapterLocator`]）是「拿什么规则算出来的」这件事的承诺：
/// `Section(3)` 只在「按今天这套规则分章」的前提下指到第三章。Markdown 的标题层级、
/// FB2 的 section 嵌套、EPUB 的 spine 过滤，任一改动都会让老库的序号指到别处 ——
/// 那时版本号对不上，[`crate::store::Store::stale_reason`] 会说「该重扫」。
///
/// v2：CSS 出库 —— `books.css` 列从拼接文本改成位置清单（`CssStored` 信封），
/// 老库靠重扫改写成新格式；重扫前旧文本当内联样式兜底。
pub const INDEX_VERSION: i64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocMeta {
    pub title: String,
    pub authors: Vec<String>,
    pub format: Format,
    pub size: u64,
}

/// 可重排文档的一章。
///
/// **正文的契约：永远是引擎就绪的 XHTML 片段。** 这是格式差异的终点 —— provider
/// 负责把自家格式归约成它（EPUB / FB2 / HTML 本来就产出 XHTML，Markdown / TXT 经
/// [`xhtml::from_plain_text`] 包一层），引擎只管吃。所以界面上不存在
/// 「这个格式要不要先包一下」这种分支，`Format` 也不参与排版链。
///
/// 阅读位置不在这套坐标里：`crate::Locator` 的 `offset` 是**章内可见文本偏移**
/// （`crate::anchor::TextMap`），进度、锚点、选中都用它；正文怎么包不影响它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    pub title: String,
    /// 容器内的条目名（EPUB 的 spine 项）。单文件格式是空串。
    ///
    /// 两个用处：相对资源（`<img src>`）的基准目录，以及 EPUB 那一路的
    /// [`ChapterLocator::Entry`]。**不落库** —— 位置列里已经有了，见
    /// `plans/text-out-of-db.md`。
    pub spine_href: String,
    /// 正文在哪儿。**这一格取代了「一个 `String` 加一个平行数组」**：
    /// 之前 `source` 空串 + `FlowDoc::chapter_locators[i]` 两个东西拼出来「正文在不在
    /// 手上」，还得靠约定保证等长；现在它是类型里的一个字段。
    pub text: ChapterText,
}

/// 一章的正文在哪儿。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChapterText {
    /// 正文就在手上（provider 刚读完、测试替身、`from_content`）。
    Inline(String),
    /// 正文在容器里，按位置现取。
    At(ChapterLocator),
}

impl Chapter {
    /// 正文就在手上的那一章。
    pub fn inline(title: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            spine_href: String::new(),
            text: ChapterText::Inline(source.into()),
        }
    }

    /// 容器里的一章，正文按条目名现取（EPUB 的 spine 项）。
    pub fn entry(title: impl Into<String>, href: impl Into<String>) -> Self {
        let href = href.into();
        Self {
            title: title.into(),
            spine_href: href.clone(),
            text: ChapterText::At(ChapterLocator::Entry(href)),
        }
    }

    /// 单文件容器里的第 N 章，正文重读一遍现分。
    pub fn section(title: impl Into<String>, index: usize) -> Self {
        Self {
            title: title.into(),
            spine_href: String::new(),
            text: ChapterText::At(ChapterLocator::Section(index)),
        }
    }

    /// 补上条目名（相对资源的基准目录）。
    pub fn with_spine_href(mut self, href: impl Into<String>) -> Self {
        self.spine_href = href.into();
        self
    }

    /// 正文；不在手上时是空串。要区分「空章」与「正文在别处」，看 [`Chapter::text`]。
    pub fn source(&self) -> &str {
        match &self.text {
            ChapterText::Inline(text) => text,
            ChapterText::At(_) => "",
        }
    }

    /// 正文的位置；在手上时是 `None`（还没算过它该在哪儿）。
    pub fn locator(&self) -> Option<&ChapterLocator> {
        match &self.text {
            ChapterText::At(at) => Some(at),
            ChapterText::Inline(_) => None,
        }
    }

    /// 正文的字符数。只是**书目层的权重**（SQLite 那一列），不是可见文本长度 ——
    /// XHTML 的标记也算在内。正文不在手上时是 0（那要读一遍才算得出）。
    pub fn char_len(&self) -> usize {
        self.source().chars().count()
    }
}

/// 一章正文在容器里的位置 —— 库里只存这个，不存正文。
///
/// 判据与图片同一条（见 `plans/images-lazy.md`）：**库里只放「算出来贵、且原文件
/// 给不了」的东西**。正文原文件给得了 —— 重开一次容器就回来了 —— 而一本二十万字的
/// 中文书入库是 0.4 MB（还要再加一倍以上的全文索引），库于是与内容同量级。
///
/// 三种位置对应两类容器：
///
/// - [`ChapterLocator::Entry`]：容器里能按名字取一个条目。EPUB 的 spine 项、CBZ 的一页。
///   取一章 = 解一个 zip 条目，最便宜的一种。
/// - [`ChapterLocator::Section`]：单文件容器（TXT / MD / HTML / FB2）里的第 N 段。
///   重新分一次章就能拿回它 —— 需要 provider 的分章规则稳定，见 [`ChapterLocator::Section`]
///   自己的说明。
/// - [`ChapterLocator::Inline`]：正文就在手上（测试、`from_content`、没有可寻址容器
///   的格式）。这一档是**降级**，不是常态：能算出前两种时不该落回它。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChapterLocator {
    /// 容器内条目名（与 [`TocEntry::href`] 同形）。
    Entry(String),
    /// 单文件容器里的第几章（0 起）。
    ///
    /// 存序号而不是字节区间：分章规则（Markdown 的 `#`、FB2 的 `<section>`）一旦
    /// 调整，字节区间就指到别处，而序号只会让「第几章」这个说法本身失效 —— 两者都
    /// 要重扫，但后者坏得看得见。
    Section(usize),
    /// 正文随文档一起来（那就是说没有可寻址的容器）。
    Inline(String),
}

impl Default for ChapterLocator {
    fn default() -> Self {
        Self::Inline(String::new())
    }
}

impl ChapterLocator {
    /// 这一档的正文是否就在位置上。
    pub fn is_inline(&self) -> bool {
        matches!(self, Self::Inline(_))
    }

    pub fn as_inline(&self) -> Option<&str> {
        match self {
            Self::Inline(text) => Some(text),
            _ => None,
        }
    }
}

/// 一章该按哪种位置存。
///
/// 名单式容器（EPUB）按 [`Chapter::spine_href`] 认条目；其余是单文件，按序号认。
/// CBZ 是定版式，没有章，走不到这里。
///
/// 已经算过位置的（从库里读回来又存回去）原样保留 —— 重算一遍只会多一次分支，
/// 而两种来源算出来的结果本来就该一致。
pub fn locator_for(format: Format, chapter: &Chapter, index: usize) -> ChapterLocator {
    if let Some(at) = chapter.locator() {
        return at.clone();
    }
    match format {
        Format::Epub if !chapter.spine_href.is_empty() => {
            ChapterLocator::Entry(chapter.spine_href.clone())
        }
        Format::Text | Format::Markdown | Format::Html | Format::Fb2 => {
            ChapterLocator::Section(index)
        }
        // 没有可寻址的位置（EPUB 少了 href、将来的格式、测试替身）：只能说正文在手上。
        _ => ChapterLocator::Inline(chapter.source().to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TocEntry {
    pub title: String,
    /// 解析后的绝对容器路径（已百分号解码、去 fragment），与 `Chapter::spine_href` 同形。
    pub href: String,
    /// 同一个文件里的落点，也就是 `href` 里 `#` 后面那一截（已百分号解码）。`None` = 章首。
    ///
    /// 单独立一个字段而不是并回 `href`：`href` 与 `Chapter::spine_href` 同形是「这条目录
    /// 对得上哪一章」的比对键，带上 `#` 就比不中了，而 nav 里多条指向同一个章文件的不同
    /// 小节恰恰是靠这一截互相区分的。
    ///
    /// `#[serde(default)]`：库里已有的 `toc_json` 没有这一项，旧书解出来是 `None`，
    /// 表现就是「跳到章首」，与它被加进来之前的行为一致。
    #[serde(default)]
    pub fragment: Option<String>,
    pub children: Vec<TocEntry>,
}

/// 一本书里的一张图片的**元数据**。`href` 是解析后的**容器绝对路径**（与
/// `Chapter::spine_href` 同形），排版与绘制都用它当键；`width`/`height` 是解码前的
/// 固有像素尺寸，读取时算好，布局期只查表。SVG 等取不到尺寸的格式为 0，由排版层降级。
///
/// **字节不在这里，也不进数据库**（学 KOReader 的 `NodeImageProxy`）：真要画时按
/// `href` 去容器里现取，见 [`image_bytes`]。原图随时能从书里再拿一次，就不该在库里
/// 留第二份 —— 一本 200 MB 的漫画，图片一入库就等于把书抄了一遍。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRes {
    pub href: String,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
}

/// 这本书里取到的一份字体字节，按容器绝对路径索引。
///
/// **不进 [`FlowDoc`]、也不进 SQLite**：字体是**锦上添花**的那一层 —— 容器不在时的
/// 表现是「回到今天的样子」（读者字体照常排，只是没有补缺字的那一份），
/// 而不是少一页正文。于是 `fonts_at` 每次开书去容器现读一次，几个小文件、一次解压，
/// 不常驻。（正文也不在库里，但它有 [`ChapterLocator`] 那条位置线，
/// 见 `plans/text-out-of-db.md`；字体没有，只能每次现读。）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontBytes {
    /// 容器内的规范路径，与 `@font-face` 的 `src` 改写后同形（见 `css::FontSrc::Url`）。
    pub href: String,
    /// manifest 声明的 media-type，供调用方打日志（WOFF 已在这里解回 sfnt，
    /// 所以 `bytes` 一律是 TTF/OTF 那一层的字节）。
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// 从字节头猜一张图的 mime。
///
/// 存在的理由是 `books.cover` 那一列**只有字节、没有媒体类型**：封面是从容器里按
/// OPF 的 cover id 捞出来的，那条记录不带 manifest 的 `media-type`。而 gpui 的
/// `Image::from_bytes` 必须显式给格式（它按格式挑解码器，给错了解不出来，
/// 一片空白且不报错），所以只能在画之前按 magic bytes 反推。
///
/// 只认根 `Cargo.toml` 给 `image` 开的那几种 feature；猜不出或没编译进来的格式返回
/// `None`，由调用方决定降级成什么占位，而不是假装它是 PNG 去试一次。
pub fn guess_media_type(bytes: &[u8]) -> Option<&'static str> {
    let guessed = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .format()?;
    match guessed {
        image::ImageFormat::Png => Some("image/png"),
        image::ImageFormat::Jpeg => Some("image/jpeg"),
        image::ImageFormat::WebP => Some("image/webp"),
        image::ImageFormat::Gif => Some("image/gif"),
        image::ImageFormat::Bmp => Some("image/bmp"),
        _ => None,
    }
}

/// 一张位图缩到长边不超过 `max_edge`，再编码回一个紧凑格式。
///
/// 返回 `Some((bytes, mime))` **只在真的缩过**时：已经够小、解不出来、或本来就
/// 不该按位图缩的（SVG）都返回 `None`，调用方保留原字节。
///
/// 为什么这件事存在：gpui 的 `img` 把位图放进 sprite atlas，而单张 atlas 纹理的
/// 上限是设备的 `max_texture_dimension_2d`（GLES 上常见 4096，部分只有 2048），
/// 并且 tile 用的是**图像原始像素**。一张 1200×1600 的插图本身不大，但在
/// devicePixelRatio 2.8 的手机上仍按 1200×1600 进 atlas —— 一旦超过上限，
/// `paint_image` 只写一条日志、图就静默消失。KOReader 的 `ImageWidget` 也是
/// 先把位图按显示尺寸缩一遍再缓存（`frontend/ui/widget/imagewidget.lua` 的
/// `scale_factor` 与 `ImageCache`），这里取同一个思路。
///
/// 编码格式跟着透明通道走：有 alpha 的走 PNG（无损），没有的走 JPEG（照片缩完
/// 仍然小一个量级）。两者都在 workspace 已开的 `image` feature 里。
pub fn shrink_image(bytes: &[u8], max_edge: u32) -> Option<(Vec<u8>, &'static str)> {
    use image::ImageDecoder as _;

    if max_edge == 0 {
        return None;
    }
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    // 动画 GIF 不缩：`ImageReader` 只解第一帧，缩完就把动画丢了。书里的 GIF 一般是
    // 小装饰图，本来也过不了 `max_edge` 那道门，这里只是把「大 GIF」这条路显式让开。
    if reader.format() == Some(image::ImageFormat::Gif) {
        return None;
    }
    // 先拿 decoder 把 EXIF 方向读出来再解码：`from_decoder` 不应用方向，
    // 不自己补一下的话，手机上拍的竖图缩完会横着。gpui 自己那条解码路
    // （`decode_static_image_from_decoder`）也是这么做的，两边口径一致。
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder.orientation().ok()?;
    let mut image = image::DynamicImage::from_decoder(decoder).ok()?;
    image.apply_orientation(orientation);

    let (width, height) = (image.width(), image.height());
    let longest = width.max(height);
    // **不管原图多大都重编一遍**：封面的问题不是「太大」而是「按原格式存」。
    // 一张 800×1200 的 PNG 封面就有 400 KB，而书架上显示几十像素宽 —— 它过不了
    // 「超过 max_edge 才缩」那道门，于是原样进了库。缩到显示尺寸 + 一律 JPEG，
    // 才是这一列真正该占的地方。
    let image = if longest > max_edge {
        let scale = max_edge as f32 / longest as f32;
        let width = ((width as f32 * scale).round() as u32).max(1);
        let height = ((height as f32 * scale).round() as u32).max(1);
        image.resize_exact(width, height, image::imageops::FilterType::Triangle)
    } else {
        image
    };

    // 带 alpha 的封面先铺一层白底再编 JPEG：JPEG 没有 alpha，直接丢掉会让透明区
    // 变成黑的。书封极少靠透明表达什么，白底是印刷品的默认。
    let rgb = image.to_rgb8();
    let mut out = Vec::new();
    let mut encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::Cursor::new(&mut out), 82);
    encoder.encode_image(&rgb).ok()?;
    // 重编之后反而更大的（小图、或者已经高度优化的 JPEG）就退回原图。
    if out.len() >= bytes.len() {
        return None;
    }
    Some((out, "image/jpeg"))
}

// 内容家族（可重排 / 定版式）与 provider 注册表各自成文件，见 `content` / `provider`。
// 这里把它们再导出，旧路径 `format::{FlowDoc, Content, FormatAdapter, adapters}` 全线保留：
// 调用方不需要知道内部拆成了几块。

/// 扩展名的小写形式（不带点）。认不出返回 `None`。
pub(crate) fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// 文件名去掉扩展名，作为无名之书的兜底标题。
pub(crate) fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "未命名".to_string())
}

/// 位图的固有像素尺寸。只读 header，不解像素；取不到尺寸（SVG / 未知格式）时返回
/// `(0, 0)`，由排版的降级规则兜底。
pub(crate) fn image_dims(bytes: &[u8]) -> (u32, u32) {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .unwrap_or((0, 0))
}

/// 扩展名 → 图片 mime。用在「容器没声明 media-type」的场合（HTML 的 `<img>`、FB2 的
/// `<binary>` 缺失 content-type）；认不出给 `application/octet-stream` 而不是猜 PNG。
pub(crate) fn media_type_of(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

pub fn detect(path: &Path) -> Option<Format> {
    provider_for(path).map(|provider| provider.format())
}

/// 把 `href` 相对 `base_dir`（通常是 OPF 或某章所在的目录）解析成容器内的规范路径。
/// 去掉 `#fragment`，做百分号解码，折叠 `./` 与 `../`。
///
/// 这是 epub 适配器与 app 侧图片 resolver 共用的同一份实现 —— 两边各写一次
/// 迟早会在「文件名里有空格 / 百分号编码」这类书上分叉。
pub fn resolve_href(base_dir: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href).trim();
    let decoded = percent_decode(href);
    let merged = if decoded.starts_with('/') {
        decoded.trim_start_matches('/').to_string()
    } else if base_dir.is_empty() {
        decoded
    } else {
        format!("{base_dir}{decoded}")
    };
    let mut stack: Vec<&str> = Vec::new();
    for seg in merged.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    stack.join("/")
}

/// 取 `href` 里 `#` 后面那一截并百分号解码；没有 `#`、截出来是空的、或根本不是元素 id
/// 的都是 `None`（= 跳章首）。
///
/// 必须在 [`resolve_href`] 之前取：那条的第一件事就是把 `#` 整条剥掉。解码也走同一份
/// 实现 —— 两处口径不一致的话，锚点里带百分号编码的书会在「路径解开了、锚点解不开」
/// 这种最难查的地方分叉。
///
/// 只认 XHTML 的 `id`。`epubcfi(...)` 是另一套定位语法、OEB 17 的 `$` 是历史遗留，
/// 这里都不解析：认不出时读者的表现是落到章首，而不是跳到一个错的位置上。
pub fn fragment_of(href: &str) -> Option<String> {
    // fragment 是**第一个** `#` 之后的全部内容（它自己也可以含 `#`）。
    let raw = href.split_once('#')?.1.trim();
    if raw.is_empty() || raw.starts_with("epubcfi(") {
        return None;
    }
    // `#a,xpointer(...)` 这种复合片段里只有第一段是元素 id。
    let id = raw.split(',').next().unwrap_or(raw);
    (!id.is_empty()).then(|| percent_decode(id))
}

/// 路径所在目录，带结尾 `/`；没有目录时是空串。
pub fn dir_of(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/"),
        None => String::new(),
    }
}

pub(crate) fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(n) = u8::from_str_radix(&input[i + 1..i + 3], 16)
        {
            out.push(n);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    // href 按 UTF-8 百分号编码；解出来的字节流不一定落在字符边界上，
    // 那种情况保留替换字符而不是整本读失败。
    String::from_utf8_lossy(&out).into_owned()
}

/// 把一份 CSS 里的 `url(…)` 按**那份 CSS 自己的目录**解成容器绝对路径。
///
/// 正文出库之后样式表按清单回容器现取（见 `crate::document::Document::stylesheet`），
/// 每一份都要在拼接前进这一步 —— 拼完就再也不知道某条 `url(Fonts/a.otf)` 原本写在
/// `OEBPS/Styles/` 还是 `OEBPS/` 下面。而 `@font-face` 的 `src` 要拿去查容器里的
/// 条目 —— 差一层目录的表现是「书里明明有字体，就是读不到」，且屏幕上看不出原因。
///
/// 输出**一律带引号**：解出来的路径常含空格（真书里的 `body italic.otf`），
/// 不加引号那是非法 CSS，解析器会在空格处断开。
pub(crate) fn absolutize_css_urls(css: &str, base_dir: &str) -> String {
    // 先清注释：注释里的 `url(x)` 不是声明，改写它等于往 CSS 里塞假路径。
    let text = crate::css::strip_comments(css);
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let Some(start) = find_url_function(rest) else {
            out.push_str(rest);
            return out;
        };
        // `rest[start..start + 4]` 就是 `url(`，`open` 是它之后第一位。
        let open = start + 4;
        let Some(close) = find_close_paren(&rest[open..]) else {
            // 括号不闭合：剩下的原样抄走，不猜。
            out.push_str(rest);
            return out;
        };
        let token_end = open + close + 1; // 整个 `url(…)` 函数 token 的结束（不含）
        let inner = rest[open..open + close]
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .trim();
        // `url(` 之前的部分先抄走；`url(` 本身**不能**跟着抄，否则改写会变成
        // `url(url("解出来的路径")` —— 括号一下就不配对了。
        out.push_str(&rest[..start]);
        if inner.is_empty() || crate::css::is_remote(inner) {
            // 远程与空的都原样留着：一期不取外部字体，也不假装取过。
            out.push_str(&rest[start..token_end]);
        } else {
            out.push_str("url(\"");
            out.push_str(&resolve_href(base_dir, inner));
            out.push_str("\")");
        }
        rest = &rest[token_end..];
    }
    out
}

/// 下一个 `url(` 函数 token 的起始字节偏移。
///
/// 要判前一个字符：`dataurl(` / `my-url(` 里都含 `url(`，按子串找会把它们当函数改写。
fn find_url_function(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i + 4 <= bytes.len() {
        if bytes[i..i + 4].eq_ignore_ascii_case(b"url(")
            && (i == 0 || !is_function_char(bytes[i - 1]))
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 能构成一个标识符的字符：命中的话它前面那串就不是独立的 `url` 函数名。
fn is_function_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b >= 0x80
}

/// 从 `text` 开头找到配对 `)` 的位置（不含）。引号里的括号跳过。
fn find_close_paren(text: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in text.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            ')' => return Some(i),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    //! href 的 fragment 拆分，以及目录条目在 SQLite 那一列上的往返。

    use super::{TocEntry, fragment_of, shrink_image};

    /// 造一张真位图 —— `shrink_image` 要真的解码+重编，够 header 的假字节过不了。
    fn png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .expect("合成 PNG 应能编码");
        out
    }

    /// 大图缩下来、小图原样：这是手机上图片能不能上屏的那一条。
    #[test]
    fn covers_are_bounded_and_re_encoded_as_jpeg() {
        // 解不出来的字节不动：不能把「不是图」当成「要缩的图」。
        assert!(shrink_image(b"not an image", 256).is_none());
        // 0 是「不设上限」的哨兵，不能被当成要缩到 0。
        assert!(shrink_image(&png(100, 80), 0).is_none());
        // 无字可压缩的极小图：重编反而更大，退回原图（`None` = 用原字节）。
        assert!(shrink_image(&png(2, 2), 256).is_none());

        // 超长边：缩到上限内，纵横比保住，而且**一律 JPEG** —— 带 alpha 的封面
        // 铺白底再编，不然透明区会变黑。
        let (small, mime) = shrink_image(&png(1024, 512), 256).expect("大图该被缩");
        let decoded = image::load_from_memory(&small).expect("缩过的字节要还能解");
        assert_eq!(
            (decoded.width(), decoded.height()),
            (256, 128),
            "纵横比要保住"
        );
        assert_eq!(mime, "image/jpeg");

        // **没超过上限**的也要重编：封面的问题是「按原格式存」，不是「太大」——
        // 一张 800×1200 的 PNG 原样进库就是 400 KB。
        let (_, mime) = shrink_image(&png(400, 600), 512).expect("没过上限也要重编");
        assert_eq!(mime, "image/jpeg");
    }

    #[test]
    fn fragments_are_taken_after_the_first_hash() {
        assert_eq!(
            fragment_of("Text/chap2.xhtml#court").as_deref(),
            Some("court")
        );
        assert_eq!(fragment_of("Text/chap1.xhtml"), None, "没有 # 就是章首");
        assert_eq!(
            fragment_of("Text/chap1.xhtml#"),
            None,
            "空 fragment 也是章首"
        );
        // fragment 是第一个 # 之后的全部：它自己可以含 #（`a#b#c` 的落点是 `b#c`）。
        assert_eq!(fragment_of("a.xhtml#b#c").as_deref(), Some("b#c"));
        assert_eq!(fragment_of("a.xhtml#%20x").as_deref(), Some(" x"));
        // 只有第一段是元素 id，后面的 xpointer 部分丢掉。
        assert_eq!(
            fragment_of("a.xhtml#sec,xpointer()").as_deref(),
            Some("sec")
        );
    }

    /// 认不出来的两套 fragment 语法都当「没有」，也就是落到章首，而不是跳到一个错的位置。
    #[test]
    fn unsupported_fragments_read_as_chapter_start() {
        assert_eq!(fragment_of("a.xhtml#epubcfi(/6/8[chap2]!4:12)"), None);
    }

    /// 库里已有的 `toc_json` 没有 fragment 这一项。解不开旧数据比少一个落点严重得多 ——
    /// 那是一整本书的目录凭空消失。
    #[test]
    fn toc_entries_without_a_fragment_field_still_load() {
        let old = r#"[{"title":"第一章","href":"OEBPS/Text/chap1.xhtml","children":[]}]"#;
        let entries: Vec<TocEntry> = serde_json::from_str(old).expect("旧 toc_json 要能解");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].fragment, None);
        assert_eq!(entries[0].href, "OEBPS/Text/chap1.xhtml");
    }
}

#[cfg(test)]
mod contract {
    //! **引擎输入契约**：`Chapter::source` 永远是引擎就绪的 XHTML。
    //!
    //! 这一层以前是破的 —— EPUB / FB2 / HTML 的章源是 XHTML，Markdown / TXT 是规范化
    //! 纯文本，于是界面必须知道「哪个格式还要包一层」（`Format::source_is_xhtml` +
    //! `chapter_xhtml`），`Format` 就这样渗进了排版链。包装下沉到 provider 之后，
    //! 这条契约成为 provider 的**唯一输出约定**，界面与引擎都不再问格式。
    //!
    //! 这里用真实 provider 跑（不是构造 `Chapter`），因为要钉住的正是「provider 有没有
    //! 真的守约」。三种源都覆盖：本来就是标记的（HTML / FB2 / EPUB）、纯文本的
    //! （Markdown / TXT）、以及不进引擎的定版式（CBZ）。

    use std::path::{Path, PathBuf};

    use crate::anchor::TextMap;
    use crate::dom::Dom;
    use crate::format::{Content, Format, provider_for};

    /// 一段 XHTML 的可见文本。契约里真正要保的是**内容**，不是标记 —— 断言只能看它。
    fn visible(xhtml: &str) -> String {
        let dom = Dom::parse(xhtml.as_bytes());
        let body = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map_or(dom.document, |(id, _)| id);
        TextMap::build(&dom, body).text().to_string()
    }

    fn without_whitespace(text: &str) -> String {
        text.chars().filter(|c| !c.is_whitespace()).collect()
    }

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    /// EPUB 用仓库里那份合成书（`tests/fixtures/oss/minimal-zh.epub`），
    /// 它是唯一一个「真容器」样本，其余格式临时造一份就够。
    fn epub_fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/minimal-zh.epub")
    }

    fn fb2(dir: &Path) -> PathBuf {
        write(
            dir,
            "a.fb2",
            r#"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">
<description><title-info><book-title>契约</book-title></title-info></description>
<body><section><title><p>第一章</p></title><p>正文</p></section></body>
</FictionBook>"#,
        )
    }

    /// 可重排的每一种 provider：产物必须能直接进引擎。
    ///
    /// 断言是「能解析 + body 里有可见文本」，不是「标签在白名单里」—— 引擎对不认识的
    /// 标签本来就有 block 降级（`css::ua`），把清单写死会把合法的新标签误判成违约，
    /// 而这条契约真正要保的是**引擎拿到的东西是标记、且内容没丢**。
    #[test]
    fn every_reflow_provider_hands_the_engine_parseable_xhtml() {
        let dir = tempfile::tempdir().unwrap();
        let cases: Vec<(&str, PathBuf)> = vec![
            (
                "markdown",
                write(
                    dir.path(),
                    "a.md",
                    "# 第一章\n\n正文一\n\n## 第二章\n\n正文二\n",
                ),
            ),
            ("text", write(dir.path(), "a.txt", "第一段\n\n第二段\n")),
            (
                "html",
                write(
                    dir.path(),
                    "a.html",
                    "<html><body><h1>标题</h1><p>正文</p></body></html>",
                ),
            ),
            ("fb2", fb2(dir.path())),
            ("epub", epub_fixture()),
        ];

        for (name, path) in cases {
            let provider = provider_for(&path).unwrap_or_else(|| panic!("{name} 该有 provider"));
            let (_, content) = provider
                .read(&path)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let Content::Reflow(flow) = content else {
                panic!("{name} 该读成可重排");
            };
            assert!(
                provider.format().as_str() == name,
                "{name} 的 format 对不上"
            );
            assert!(!flow.chapters.is_empty(), "{name} 一章都没有");
            for (ix, chapter) in flow.chapters.iter().enumerate() {
                assert!(
                    chapter.source().contains('<'),
                    "{name} 第 {ix} 章的 source 不是标记：{:?}",
                    &chapter.source()[..chapter.source().len().min(120)]
                );
                let text = visible(&chapter.source());
                assert!(
                    !text.trim().is_empty(),
                    "{name} 第 {ix} 章解析出来没有可见文本"
                );
            }
        }
    }

    /// 纯文本那一支的核心不变量：**包装不增删一个可见字符**。
    ///
    /// 这条断了，读者的进度与批注坐标会整体漂移 —— 而屏幕上什么都看不出来。
    #[test]
    fn the_plain_text_wrapper_preserves_visible_text() {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in [
            (
                "a.md",
                "# 第一章\n\n正文一\n第二行\n\n## 第二章\n\n正文二\n",
            ),
            ("a.txt", "第一段\n\n第二段\n"),
        ] {
            let path = write(dir.path(), name, body);
            let provider = provider_for(&path).unwrap();
            let (_, content) = provider.read(&path).unwrap();
            let Content::Reflow(flow) = content else {
                panic!("{name} 该读成可重排");
            };
            let joined: String = flow.chapters.iter().map(|c| visible(&c.source())).collect();
            let expected = super::xhtml::atx_markers_removed(body);
            assert_eq!(
                without_whitespace(&joined),
                without_whitespace(&expected),
                "{name} 的包装改了内容"
            );
        }
    }

    /// 定版式走另一族：产出的是页表，不是章源。它压根不该有 `Chapter`。
    #[test]
    fn the_fixed_family_hands_pages_not_chapters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.cbz");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        use std::io::Write as _;
        for (name, w, h) in [("1.png", 4u32, 6u32), ("10.png", 4, 6)] {
            zip.start_file(name, opts).unwrap();
            let img = image::RgbaImage::from_pixel(w, h, image::Rgba([1, 2, 3, 255]));
            let mut bytes = Vec::new();
            image::DynamicImage::ImageRgba8(img)
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap();

        let provider = provider_for(&path).unwrap();
        assert_eq!(provider.format(), Format::Cbz);
        let (_, content) = provider.read(&path).unwrap();
        let Content::Fixed(fixed) = content else {
            panic!("CBZ 该读成定版式");
        };
        assert_eq!(fixed.page_count(), 2);
        for page in &fixed.pages {
            assert!(page.image_href().is_some(), "CBZ 的页都是图源");
        }
    }
}
