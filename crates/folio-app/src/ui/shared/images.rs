//! 书籍图片索引：把 `<img src>` 解析成容器路径，并按 href 交出一张图的**取字节方式**。
//!
//! 分三层是为了少复制：`BookImages` 在打开一本书时建一次，每张图只存**元数据**
//! （href、媒体类型、固有尺寸）与一个按需读字节的来源 [`ByteSource`]；
//! **要画时才产出一条 [`DecodeRequest`]**（怎么读字节、要不要缩小、按什么格式解），
//! 真去读去解的那一趟由 `reader::image_atlas` 在**后台线程**上跑；`ChapterImages`
//! 只按当前章的目录解析相对路径，换章时重建，成本是一张 `HashMap` 的键拷贝。
//!
//! 字节不随 `FlowDoc` 常驻内存，也不进数据库 —— 对应 KOReader 的
//! [`NodeImageProxy`]：只持 `refName` 与宽高，`Decode()` 时才 `container->OpenStream`
//! 现取（`reference/koreader/base/thirdparty/kpvcrlib/crengine/crengine/src/lvtinydom.cpp:22178`）。
//! 解出来的**像素**同样不常驻：那一份现在有人回收了（`reader::image_atlas`）。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use folio_core::content::document::Document;
use folio_core::content::format::{ImageRes, dir_of, guess_media_type, resolve_href, shrink_image};
use folio_core::layout::{ImageProvider, ResolvedImage};
#[cfg(test)]
use gpui_kit::RenderImage;
use gpui_kit::{Image, ImageFormat};

/// 容器绝对路径 → 图片字节。
///
/// 拿 `Arc<dyn Fn>` 而不是一个具体类型，是为了让「真实容器」与「测试里的内存表」
/// 共用一个入口：[`DecodeRequest::decode`] 只管问要字节，不问它从哪来。
///
/// `Send + Sync` 是硬要求：排版线程要它，`reader::image_atlas` 那趟后台解码更要它。
pub type ByteSource = Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

/// 一张图都不来的来源（Markdown / TXT，或「容器丢掉了」那条降级路）。
#[must_use]
pub fn no_images() -> ByteSource {
    Arc::new(|_href| None)
}

/// 把一本书接成字节来源：问 [`Document::image_bytes`] 要那一条。
///
/// 字节从哪来（解 ZIP、单文件、书库、什么都没有）是 `Document` 的事，这一层只转发 ——
/// 所以界面里不再有「按格式再查一次 provider」那一步（那正是 `Format` 渗进排版链的入口）。
///
/// 每次调用重开容器是刻意的，跟 KOReader 对齐（`NodeImageProxy::Decode()` 也是重开）。
/// 「一张图在屏幕上这段时间只读一次」现在由图集那一层保证（`reader::image_atlas`），
/// 不在这里 —— 这里连字节都不碰。
pub fn document_source(doc: Arc<dyn Document>) -> ByteSource {
    Arc::new(move |href| doc.image_bytes(href))
}

/// 内嵌位图长边的上限（图像原始像素）。
///
/// 选 2048 不是随便挑的：gpui 把图放进 sprite atlas，单张 atlas 纹理的上限是设备的
/// `max_texture_dimension_2d`（GLES 上常见 4096，部分只有 2048），而 tile 用的是
/// **图像原始像素**（不是显示尺寸）。一张 1200×1600 的插图在 2.8 倍屏手机上显示出来
/// 是 3360×4480，超过了就只写一条日志、图静默消失 —— 手机上「书里的图看不见」
/// 十有八九是这一条。KOReader 的 `ImageWidget` 同样先按显示尺寸缩一遍再缓存
/// （`reference/koreader/frontend/ui/widget/imagewidget.lua` 的 `scale_factor` 与 `ImageCache`）。
///
/// 这一条**不是**显存阀门：《未来简史》55 张插图最大的是 1039×1386，一张都没被它夹到
/// （2026-10-15 按 `images` 表与 EPUB 里真实的 IHDR/SOF 头各数过一遍，两边同一个总数）。
/// 它省的是「解码 + 重编码」那一笔 CPU，实测 8–38 ms / 张。真正管显存的是
/// `reader::image_atlas`。
const MAX_IMAGE_EDGE: u32 = 2048;

/// 每本开着的书一个身份，只用来给图集的键加前缀。
static NEXT_BOOK_TOKEN: AtomicU64 = AtomicU64::new(0);

/// 一格的**固有尺寸**没记在表里时，按多大来记账（`宽 × 高 × 4` 那一套算法）。
///
/// 不知道尺寸的书就是 SVG 与 manifest 没写宽高的那些（`0` 是「没写」的哨兵值）。
/// 这个数只影响回收的**顺序**（字节封顶那一档先挤谁），不影响画不画得出来，
/// 而解好之后图集会拿真的像素数把它换掉，所以往大里猜是安全的那一侧。
const UNKNOWN_TILE_BYTES: usize = 1024 * 1024 * 4;

struct ImageEntry {
    href: String,
    width: f32,
    height: f32,
    /// manifest 声明的媒体类型。字节不在手里，这是格式认不出的最后一道参考
    /// （第一道是字节头，见 [`DecodeRequest::decode`]）。
    media_type: String,
}

/// 一张图「怎么才画得出来」：图集拿着它，在后台线程上跑一趟就得到像素。
///
/// 拆成「谁问」与「谁干活」两半的理由：界面每帧只想知道「这张此刻画得出来吗」，
/// 而那一趟真活（开容器、读、必要时缩小、解码）一次都不能落在绘制帧里。
///
/// `Clone` 是免费的（一条链子的引用计数 + 那个键），因为场景与绘制之间要过一手：
/// 场景只登记「这一帧该摆它」，像素要到绘制那一层才问图集要。
#[derive(Clone)]
pub struct DecodeRequest {
    /// 这张图在图集里的身份：`<这本书的 token>/<容器路径>`。
    ///
    /// token 不是多余的：同一本重新打开是另一份 `BookImages`、另一个 token，
    /// 而两本书的容器路径完全可以同名 —— 键撞上的后果是「第二本画出第一本的图」
    /// 那种看不出来的错（字节来源与固有尺寸都在第一个 token 那份上）。
    pub key: String,
    /// 现取字节 → 必要时缩小 / 栅格化 → 认出格式，交回一份能交给 gpui 解码的 `Image`。
    /// `None` = 这本书里就没有这张图，或容器读不出它（两种表现相同：那块空着）。
    loader: Arc<dyn Fn() -> Option<Arc<Image>> + Send + Sync>,
    /// 这一格进图集大概要占多少字节（**固有像素** × 4，按 [`MAX_IMAGE_EDGE`] 夹过）。
    ///
    /// 是估的：SVG 与 manifest 没写尺寸的那些只能猜，而解好之后图集会按真的像素数换掉它。
    /// 估的用途只有一个 —— 还没解完的那几趟也占个名额，别让封顶那一档把它们当免费。
    pub tile_bytes: usize,
}

impl DecodeRequest {
    /// 跑那一趟活儿。**在后台线程上调**（`reader::image_atlas` 负责），UI 线程调它
    /// 就是回到「每滚进一张图顿一下」那条老路。
    pub fn decode(&self) -> Option<Arc<Image>> {
        (self.loader)()
    }

    /// 那条活儿本身（图集要在后台线程上拿着它跑，所以给的是链子而不是结果）。
    pub(crate) fn loader(&self) -> Arc<dyn Fn() -> Option<Arc<Image>> + Send + Sync> {
        Arc::clone(&self.loader)
    }
}

impl ImageEntry {
    /// 这张图**按索引里记着的固有尺寸**就编得进设备吗（= 不必缩小）。
    ///
    /// 两个条件缺一不可：尺寸得是知道的（`0` 是「manifest 没写」，那种情况只有解一次
    /// 字节才知道多大），且长边不超过 [`MAX_IMAGE_EDGE`]。判据用**固有尺寸**而不是显示
    /// 尺寸，与那条常量的理由同一套：进 atlas 的是原始像素。
    fn fits_the_device_limits(&self) -> bool {
        self.width > 0.0
            && self.height > 0.0
            && self.width <= MAX_IMAGE_EDGE as f32
            && self.height <= MAX_IMAGE_EDGE as f32
    }

    /// 一格的字节数（估）：固有像素夹到上限之后 `宽 × 高 × 4`。
    fn tile_bytes(&self) -> usize {
        if self.width <= 0.0 || self.height <= 0.0 {
            return UNKNOWN_TILE_BYTES;
        }
        let cap = MAX_IMAGE_EDGE as f32;
        let edge = |v: f32| (v.min(cap)).ceil() as usize;
        edge(self.width) * edge(self.height) * 4
    }

    /// 产出「这张图怎么画出来」那一条活儿。
    ///
    /// 四步都在这里，而**这四步都发生在被调用的那一刻**（= 图集在后台线程上跑的那一趟）：
    ///
    /// 1. **取字节**：从 `bytes` 那条来源按容器路径现读。读不到（文件被移走 /
    ///    容器坏了 / 路径不在了）就回 `None`，调用方不画那块 —— 与 KOReader
    ///    容器丢失时的降级一致；
    /// 2. **缩小**：超过 [`MAX_IMAGE_EDGE`] 的先缩到上限内（见那条常量的理由）；
    /// 3. **认格式**：优先信字节头，其次信 manifest 声明的 media-type。真书里
    ///    `image/jpg`、`IMAGE/PNG`、`image/octet-stream` 这类写错比字节头写错常见得多，
    ///    而 gpui 按格式挑解码器（`Image::to_image_data` 是 `match self.format`），
    ///    给错了就是一片静默空白。两者都不认才按 PNG 试一把；
    /// 4. **尺寸在上限内就一步都不做**（`fits_the_device_limits`）：字节原样交出去。
    ///    第 2 步与第 4 步是一对 —— 那条快路省掉的正是「解码 + 重编码」这一笔。
    fn job(&self, bytes: &ByteSource) -> DecodeRequest {
        let bytes = Arc::clone(bytes);
        let href = self.href.clone();
        let media_type = self.media_type.clone();
        let fits = self.fits_the_device_limits();
        let loader: Arc<dyn Fn() -> Option<Arc<Image>> + Send + Sync> = Arc::new(move || {
            let raw = bytes(&href)?;
            // SVG 走栅格化，别交给 `image` crate（它不认 SVG，`guess_media_type` 会回
            // None，最后退回按 PNG 试一把 → 静默空白）。判定看字节头优先，其次看
            // manifest 声明（真书里两边都可能只对一边）。
            let is_svg =
                folio_core::content::format::svg::looks_like_svg(&raw) || mime_is_svg(&media_type);
            let (data, format) = if is_svg {
                match folio_core::content::format::svg::rasterize(
                    &raw,
                    Some((MAX_IMAGE_EDGE, MAX_IMAGE_EDGE)),
                ) {
                    Some((png, _)) => {
                        let fmt = guess_media_type(&png)
                            .and_then(format_of_mime)
                            .unwrap_or(ImageFormat::Png);
                        (png, fmt)
                    }
                    // 栅格化不了（坏 SVG、超大）：维持原样，大概率不显示 ——
                    // 与「这张图本来就没有」同一个表现。
                    None => (raw, ImageFormat::Png),
                }
            } else if fits {
                // 固有尺寸是索引里就有的、也没超过上限 —— **一个像素都不必重编**。
                // 这一格原来无条件走 `shrink_image`，而它是「解码 + 重编码」：
                // 真机实测一张 1000×1386 的插图要 8–38 ms（`plans/阅读管线.md` §十 的
                // 判据：这一步能省就省）。挪到后台线程之后它不再卡帧，但仍然是白花的钱。
                //
                // EXIF 方向不会因此丢：gpui 自己那条解码路
                // （`decode_static_image_from_decoder`）同样会应用方向，两边口径一致。
                let fmt = guess_media_type(&raw)
                    .or(Some(media_type.as_str()))
                    .and_then(format_of_mime)
                    .unwrap_or(ImageFormat::Png);
                (raw, fmt)
            } else {
                let (data, declared) = match shrink_image(&raw, MAX_IMAGE_EDGE) {
                    Some((small, mime)) => (small, mime),
                    None => (raw, media_type.as_str()),
                };
                let fmt = guess_media_type(&data)
                    .or(Some(declared))
                    .and_then(format_of_mime)
                    .unwrap_or(ImageFormat::Png);
                (data, fmt)
            };
            tracing::info!(
                "图集: {} 交出去 {} 字节，按 {:?} 解",
                href,
                data.len(),
                format
            );
            Some(Arc::new(Image::from_bytes(format, data)))
        });
        DecodeRequest {
            key: String::new(),
            loader,
            tile_bytes: self.tile_bytes(),
        }
    }
}

/// 一本书的全部图片，按容器路径索引。
///
/// `bytes` 是这份索引的「容器」那一半：元数据在 `entries` 里，字节要画的时候才从它读。
/// 分开之后，打开一本书拿到的是一张很轻的表，而不是整本图的字节。
pub struct BookImages {
    entries: Vec<ImageEntry>,
    index: HashMap<String, usize>,
    bytes: ByteSource,
    /// 这一本的身份（见 [`DecodeRequest::key`]）。
    token: u64,
}

impl BookImages {
    pub fn new(images: &[ImageRes], bytes: ByteSource) -> Self {
        let token = NEXT_BOOK_TOKEN.fetch_add(1, Ordering::Relaxed);
        let mut entries = Vec::with_capacity(images.len());
        let mut index = HashMap::with_capacity(images.len());
        for r in images {
            let idx = entries.len();
            index.entry(r.href.clone()).or_insert(idx);
            entries.push(ImageEntry {
                href: r.href.clone(),
                width: r.width as f32,
                height: r.height as f32,
                media_type: r.media_type.clone(),
            });
        }
        Self {
            entries,
            index,
            bytes,
            token,
        }
    }

    /// 那张图在容器里的**原始字节**（不经缩小、不经解码）。
    ///
    /// 分享与下载递给系统的是这一份：摊开看着的那一份像素是按 [`MAX_IMAGE_EDGE`]
    /// 缩过的、格式也可能是 `fallback=` 那一条换过的位图 —— 拿它去分享就是把读者
    /// 书里的原图降一档给他。取不到给 `None`：容器丢了，或那一条根本不在表里。
    pub fn raw_bytes(&self, href: &str) -> Option<Vec<u8>> {
        (self.bytes)(href)
    }

    /// 取字节那条链子本身（**可以跨线程拿**）。
    ///
    /// 一次递出去的分享该在后台那一趟里读解压，而不是压在读者按下按钮的那一帧：
    /// 一页漫画是 1–3 MB 的 JPEG， inflate 在手机上是要看得见的一笔。
    pub fn source(&self) -> ByteSource {
        Arc::clone(&self.bytes)
    }

    fn get(&self, href: &str) -> Option<&ImageEntry> {
        self.index.get(href).map(|i| &self.entries[*i])
    }

    /// 这张图**声明**的媒体类型（容器路径 → manifest 里那一格），表里没有给 `None`。
    ///
    /// 分享与下载要按它递系统 intent（`image/png` 与 `image/*` 在接收方那边不是一回事），
    /// 而字节要到后台那一趟才到手 —— 界面这边问得出、也只有这张表给得出的就是这一格。
    fn media_type(&self, href: &str) -> Option<&str> {
        self.get(href).map(|entry| entry.media_type.as_str())
    }

    /// 这张图的活儿（键已带上本书的 token）。
    fn job(&self, entry: &ImageEntry) -> DecodeRequest {
        let mut request = entry.job(&self.bytes);
        request.key = format!("{}/{}", self.token, entry.href);
        request
    }
}

/// mime → gpui 的 `ImageFormat`。认不出时返回 `None`，由调用方决定最后的降级 ——
/// 不在这里默认成某个格式，是因为那个 `unwrap_or` 的位置决定“默认”算谁的。
fn format_of_mime(media_type: &str) -> Option<ImageFormat> {
    let mime = media_type.split(';').next().unwrap_or("").trim();
    ImageFormat::from_mime_type(mime)
}

/// 声明的媒体类型是不是 SVG（参数与大小写都归一）。
fn mime_is_svg(media_type: &str) -> bool {
    media_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("image/svg+xml")
}

/// mime → gpui 的 `ImageFormat`。认不出时按 PNG 试（最常见，且解不出时只是不显示，
/// 不会 panic）。**只给封面这条「只有字节、没有 manifest 媒体类型」的路用**：
/// 正文里的图片走 [`ImageEntry::rendered`]，那条会拿字节头当第一判据。
fn format_of(media_type: &str) -> ImageFormat {
    format_of_mime(media_type).unwrap_or(ImageFormat::Png)
}

/// 封面字节 → 可画的 `Image`；格式认不出时返回 `None`。
///
/// 返回 `None` 而不是按 PNG 试一把，是因为 `Image::from_bytes` 之后 gpui 就按那个
/// 格式挑解码器（`platform.rs` 里 `to_image_data` 是 `match self.format`），
/// 给错格式的结果是**静默画空**：书架上是一块和「没有封面」长得一样的空白，
/// 而界面已经分不清这两件事了。认不出时调用方要画显式的占位块。
pub fn cover_image(bytes: &[u8]) -> Option<Arc<Image>> {
    // SVG 封面：先栅格化成 PNG 再交给 gpui（它不认 SVG）。
    if folio_core::content::format::svg::looks_like_svg(bytes) {
        let (png, _) = folio_core::content::format::svg::rasterize(bytes, Some((512, 512)))?;
        return Some(Arc::new(Image::from_bytes(ImageFormat::Png, png)));
    }
    let media_type = folio_core::content::format::guess_media_type(bytes)?;
    Some(Arc::new(Image::from_bytes(
        format_of(media_type),
        bytes.to_vec(),
    )))
}

/// 一章的图片视图：`src` 相对本章目录解析成容器路径后查 `BookImages`。
pub struct ChapterImages {
    book: Arc<BookImages>,
    base_dir: String,
}

impl ChapterImages {
    pub fn new(book: Arc<BookImages>, spine_href: &str) -> Self {
        Self {
            book,
            base_dir: dir_of(spine_href),
        }
    }

    /// 一张图都没有时的索引（Markdown / TXT 恒如此，EPUB 无图文时也成立）。
    /// 任何 `src` 都解不出来，排版因此跳过每个 `<img>`，不会塌成空盒。
    pub fn empty() -> Self {
        Self {
            book: Arc::new(BookImages::new(&[], no_images())),
            base_dir: String::new(),
        }
    }

    /// 绘制用：容器路径 → 这一张图的活儿（键 + 怎么解 + 占多少字节的估计）。
    ///
    /// 查不到（这本书的索引里就没有这条路径）才给 `None`。**字节在不在、解不解得开这里
    /// 不知道** —— 那两件事要到后台跑 [`DecodeRequest::decode`] 才知道，而它们的表现
    /// 都是「那块空着，正文照排」，与从前一致。
    pub fn decode_request(&self, href: &str) -> Option<DecodeRequest> {
        let entry = self.book.get(href)?;
        Some(self.book.job(entry))
    }

    /// 一张图的**固有像素**尺寸（容器路径 → `(宽, 高)`），查不到给 `(0, 0)`。
    ///
    /// 问的是表，不重解 `src`：`href` 已经是容器路径，再走一遍 [`Self::resolve`] 会把
    /// 本章目录再拼一次。放大查看用它算「一个源像素对一个 CSS 像素」的那个顶
    /// （见 `reader::image_viewer`）。
    pub fn natural_size(&self, href: &str) -> (f32, f32) {
        self.book
            .get(href)
            .map(|entry| (entry.width, entry.height))
            .unwrap_or((0.0, 0.0))
    }

    /// 这张图声明的媒体类型（容器路径 → `image/png` 那一格），表里没有给 `None`。
    ///
    /// 问的是**整本书那张表**，与本章目录无关，所以这一格不需要先 `resolve`。
    /// 图片查看器那两条递给系统的活儿（分享、下载）按它决定 intent 的类型。
    pub fn media_type(&self, href: &str) -> Option<&str> {
        self.book.media_type(href)
    }

    /// 取字节那条链子（转交整本书那一份，可以跨线程拿走）。
    ///
    /// 定版式那一支手里拿的就是这一格（`FixedBook::images`），它要分享一张页图时
    /// 不必再回头找 `BookImages`。
    pub fn source(&self) -> ByteSource {
        self.book.source()
    }
}

impl ImageProvider for ChapterImages {
    fn resolve(&self, src: &str) -> Option<ResolvedImage> {
        let href = resolve_href(&self.base_dir, src);
        let entry = self.book.get(&href)?;
        tracing::info!(
            "ChapterImages::resolve: {src:?} → {href:?} 命中 {}×{}",
            entry.width,
            entry.height
        );
        Some(ResolvedImage {
            href: entry.href.clone(),
            width: entry.width,
            height: entry.height,
        })
    }
}

/// 一张 2×2 的**真实** PNG，`Image::from_bytes` 解得开。
///
/// 测试里凡是要「画得出来的一张图」都拿这一份：只有文件头的那种字节要等绘制期才出错，
/// 而画不出来的图与「这本书没有图」在屏幕上长得一模一样 —— 挂测试的人不该去赌那一步。
#[cfg(test)]
pub(crate) const TINY_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 2, 8, 6, 0,
    0, 0, 114, 182, 13, 36, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 144, 139, 58, 241, 31, 132,
    25, 96, 12, 0, 74, 204, 8, 253, 188, 207, 161, 153, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96,
    130,
];

/// 测试里造一份画得出来的 `RenderImage`（口径见函数体那条说明）。
///
/// 放在文件级而不是 `mod tests` 里：`reader::image_viewer` 与 `reader::image_atlas`
/// 的测试同样要「一张真解得开的图」，而私有的 `mod tests` 里的东西外面拿不到。
#[cfg(test)]
/// 按 gpui 自己的解码口径造一份 `RenderImage`。
///
/// 口径照抄 `gpui::platform::decode_static_image_from_decoder`：解成 RGBA 之后
/// **每四字节把 0 与 2 换位**，交出去的是 BGRA（`RenderImage` 的文档明写
/// "in BGRA format"）。换位这一步是硬要求 —— 少做一道的图颜色红蓝颠倒，
/// 而「颜色怪」不像「画不出来」那样会让人怀疑到解码上。
pub(crate) fn bgra_render_image(width: u32, height: u32) -> Arc<RenderImage> {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for (index, chunk) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (
            (index % width as usize) as u8,
            (index / width as usize) as u8,
        );
        chunk.copy_from_slice(&[x.saturating_mul(16), y.saturating_mul(16), 128, 255]);
    }
    for chunk in pixels.as_chunks_mut::<4>().0 {
        chunk.swap(0, 2);
    }
    let buffer = image::ImageBuffer::<image::Rgba<u8>, Vec<u8>>::from_raw(width, height, pixels)
        .expect("像素数与宽高对得上");
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

#[cfg(test)]
mod tests {
    //! 共用图片层里「封面那一小块」在宿主上跑得动的部分。
    //!
    //! 为什么单独一个文件：`ui/shared/images.rs` 排版侧的索引（`<img src>` → 容器路径）
    //! 由 `tests/shared_layout.rs` 管，而封面那条走的是另一件事 —— 从一列**没有媒体类型**
    //! 的字节里把格式猜出来。它错了不会报错，只会让书架上那张卡片永远画不出来，
    //! 所以按契约钉住：认得出的要给出图，认不出的要给 `None` 而不是「按 PNG 试一把」。

    use crate::ui::shared::images::{bgra_render_image, cover_image};
    use folio_core::layout::ImageProvider as _;

    /// 够 `with_guessed_format` 探测的一段 PNG 头。
    const PNG: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, b'I', b'H', b'D', b'R',
    ];
    const JPEG: &[u8] = &[
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, b'J', b'F', b'I', b'F', 0, 0, 0, 0, 0, 0, 0,
    ];

    #[test]
    fn recognised_cover_bytes_become_a_drawable_image() {
        assert!(cover_image(PNG).is_some(), "PNG 封面要能画");
        assert!(cover_image(JPEG).is_some(), "JPEG 封面要能画");
    }

    /// 认不出格式时给 `None`。
    ///
    /// 按 PNG 硬试的后果是**静默画空**：gpui 后面就照你给的那个格式挑解码器，
    /// 解不出来不报错，界面上「没有封面」和「封面坏了」会长成同一块空白 ——
    /// 而这两种情况要说的话不一样（见 `shelf_view` 的两个空状态）。
    #[test]
    fn unrecognised_cover_bytes_are_reported_as_absent_not_as_a_blank_image() {
        assert!(
            cover_image(b"<svg xmlns='x'></svg>").is_none(),
            "SVG 封面不该被当成能画的位图"
        );
        assert!(cover_image(b"").is_none());
        assert!(cover_image(b"not an image at all, just bytes").is_none());
    }

    /// 正文插图认格式时要**先信字节头**：真书里 manifest 的 media-type 写错比字节头错得多。
    /// 以前只信 media-type，写成 `image/jpg` 的书里那张 PNG 就按 JPEG 去解，
    /// 结果是静默画空 —— 与「这本没有图」在屏幕上长得一样。
    #[test]
    fn a_book_image_takes_its_format_from_the_bytes_not_the_declared_mime() {
        use folio_core::content::format::ImageRes;
        use gpui_kit::ImageFormat;

        use crate::ui::shared::images::{BookImages, ChapterImages};

        let book = BookImages::new(
            &[ImageRes {
                href: "OEBPS/Images/pic.png".into(),
                source: "OEBPS/Images/pic.png".into(),
                // 故意写错：这正是真书里会出现的几种。
                media_type: "image/jpg".into(),
                width: 1,
                height: 1,
            }],
            // 字节不由 `BookImages` 持有，而是从这条来源现读 —— 测试里用一个内存表
            // 代替容器。
            std::sync::Arc::new(|_href: &str| Some(PNG.to_vec())),
        );
        let chapter = ChapterImages::new(std::sync::Arc::new(book), "OEBPS/Text/chap.xhtml");
        let image = chapter
            .decode_request("OEBPS/Images/pic.png")
            .expect("容器路径该命中")
            .decode()
            .expect("字节应当取得到");
        assert_eq!(
            image.format(),
            ImageFormat::Png,
            "字节头是 PNG，就不该被 `image/jpg` 带到 JPEG 去"
        );
    }

    /// 尺寸在设备上限内的插图**一个像素都不解**：这条快路是滚动不卡的关键之一。
    ///
    /// `shrink_image` 是「解码 + 重编码」，真机实测一张 1000×1386 的插图要 8–38 ms，
    /// 而且跑在 UI 线程、正好落在「这张图刚滚进屏」的那一帧 —— 读者感觉就是顿一下。
    /// 索引里本来就记着固有尺寸（`ImageRes::width/height`），够判这一刀。
    ///
    /// 钉的是 `Metric::ImageShrinks`（= `shrink_image` 被调了几次）而不是字节相等：
    /// 这张 1×1 的 PNG 重编之后反而更大，`shrink_image` 会退回原字节 —— 拿字节做断言
    /// 两条路一模一样，门是假的。计数才分得开「没解」与「解了又退回去」。
    #[test]
    fn an_image_within_the_atlas_cap_is_not_decoded_at_all() {
        use folio_core::content::format::ImageRes;
        use folio_core::infra::metrics::{Metric, take_local};

        use crate::ui::shared::images::{BookImages, ChapterImages};

        let book = BookImages::new(
            &[ImageRes {
                href: "OEBPS/Images/pic.png".into(),
                source: "OEBPS/Images/pic.png".into(),
                media_type: "image/png".into(),
                width: 1,
                height: 1,
            }],
            std::sync::Arc::new(|_href: &str| Some(ONE_PIXEL_PNG.to_vec())),
        );
        let chapter = ChapterImages::new(std::sync::Arc::new(book), "OEBPS/Text/chap.xhtml");
        take_local(Metric::ImageShrinks);
        let image = chapter
            .decode_request("OEBPS/Images/pic.png")
            .expect("容器路径该命中")
            .decode()
            .expect("字节应当取得到");
        assert_eq!(
            image.bytes, ONE_PIXEL_PNG,
            "固有尺寸在上限内，字节该原样进 gpui"
        );
        assert_eq!(
            take_local(Metric::ImageShrinks),
            0,
            "尺寸在上限内还去解码+重编码，就是每滚进一张图顿一下"
        );
    }

    /// 反过来：固有尺寸超过上限的插图**仍然要缩** —— 那是 atlas 纹理上限的防线，
    /// 少了它超限的图会被 atlas 静默丢掉（书上「这张图不显示」正是这个症状）。
    #[test]
    fn an_image_over_the_cap_still_goes_through_the_shrink_path() {
        use folio_core::content::format::ImageRes;
        use folio_core::infra::metrics::{Metric, take_local};

        use crate::ui::shared::images::{BookImages, ChapterImages};

        // 谎报一个超限尺寸：判据取的是索引里那个数，正好把这条分支逼出来。
        let book = BookImages::new(
            &[ImageRes {
                href: "OEBPS/Images/big.png".into(),
                source: "OEBPS/Images/big.png".into(),
                media_type: "image/png".into(),
                width: 4000,
                height: 4000,
            }],
            std::sync::Arc::new(|_href: &str| Some(ONE_PIXEL_PNG.to_vec())),
        );
        let chapter = ChapterImages::new(std::sync::Arc::new(book), "OEBPS/Text/chap.xhtml");
        take_local(Metric::ImageShrinks);
        let _ = chapter
            .decode_request("OEBPS/Images/big.png")
            .expect("容器路径该命中")
            .decode();
        assert_eq!(
            take_local(Metric::ImageShrinks),
            1,
            "超过上限的图必须走缩那一条"
        );
    }

    /// 一张真能解开的 1×1 RGBA PNG（三段 CRC 都算过）。
    const ONE_PIXEL_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // 签名
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1 × 1
        0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, // 8 位 RGBA
        0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, // IDAT
        0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, //
        0x0D, 0x0A, 0x2D, 0xB4, //
        0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82, // IEND
    ];

    /// 容器丢了（或那张图不在里面）时，取字节那一步必须给 `None` 而不是 panic
    /// 或空盒 —— 正文照排，图片那块地方空着。
    ///
    /// 对应 KOReader 的 `NodeImageProxy::IsInvalid()`：容器读不到就不画，不把
    /// 「这本书没这张图」与「容器坏了」拆成两种表现。
    #[test]
    fn a_missing_container_yields_no_image_instead_of_a_panic() {
        use folio_core::content::format::ImageRes;

        use crate::ui::shared::images::{BookImages, ChapterImages, no_images};

        let book = BookImages::new(
            &[ImageRes {
                href: "OEBPS/Images/pic.png".into(),
                source: "OEBPS/Images/pic.png".into(),
                media_type: "image/png".into(),
                width: 1,
                height: 1,
            }],
            no_images(),
        );
        let chapter = ChapterImages::new(std::sync::Arc::new(book), "OEBPS/Text/chap.xhtml");
        let request = chapter
            .decode_request("OEBPS/Images/pic.png")
            .expect("索引里有这条路径，活儿就交得出去");
        assert!(
            request.decode().is_none(),
            "字节来源给不出东西时，图片就该不画（取字节那一步在后台那一趟里）"
        );
        // 元数据仍在：排版依旧知道那儿有一张图（尺寸 / 位置），只是画不出像素。
        assert!(
            chapter.resolve("../Images/pic.png").is_some(),
            "排版侧的尺寸信息不依赖字节"
        );
    }

    use std::sync::Arc;

    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Context, IntoElement, ParentElement, Render, RenderImage,
        Styled, TestAppContext, Window, div, img, px, size,
    };

    /// 只画一张图的视图。`open_window` 要的是一个实现 [`Render`] 的实体，
    /// 而 `div()` 只是元素，所以得有这一层壳。
    /// 一张图的壳视图。`Option` 那一格就是「这张图在不在元素树里」——
    /// 摘掉它对应生产里「这一栏滚出可见带，不再产出 `img` 元素」。
    struct OneImage(Option<Arc<RenderImage>>);

    impl Render for OneImage {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let mut root = div().size_full();
            if let Some(image) = self.0.clone() {
                root = root.child(img(image));
            }
            root
        }
    }

    /// 图集那一格的生命周期：**离开元素树 → `drop_image` 放掉 → 回屏自己重新上传**。
    ///
    /// 这一条是「回收图片」那件事的地基门，两头各钉一件事：
    /// - **放得掉**：`App::drop_image` 之后 `Window::has_image_atlas_entry` 必须变
    ///   `false`。仍为 `true` 就说明 gpui 那个口是空转的，任何回收方案都白做。
    /// - **回得来**：图重新上屏必须自动再传一次（`window.rs:4944` 的
    ///   `sprite_atlas.get_or_insert_with`）。回不来，「回收」就等于「这张图永久画丢」——
    ///   那是 `plans/facts.md` 里「手机上看不到图」那一类查不出根因的错。
    ///
    /// 这里先让图**离开树**再回收，是因为那就是生产里的形状（滚出可见带 ⇒ 那一栏不再
    /// 产出 `img` 元素）。顺序本身**不是**约束 —— 另一条
    /// `a_tile_dropped_while_still_painted_comes_back_on_the_next_paint` 钉的是
    /// 「图还在树上就回收，下一帧也会自己补回来」。
    ///
    /// `has_image_atlas_entry` 是 gpui 的 test-only 接口（`test-support` 已是本 crate 的
    /// dev 依赖特性），所以这条门不需要真 GPU，也不看渲染器脸色。
    #[gpui_kit::test]
    fn an_atlas_tile_is_reclaimed_after_it_leaves_the_tree(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let image = bgra_render_image(8, 6);
        let on_screen = image.clone();
        let window = cx.open_window(size(px(64.0), px(64.0)), move |_, _| {
            OneImage(Some(on_screen))
        });
        let handle: AnyWindowHandle = window.into();
        let in_atlas = |cx: &mut TestAppContext| {
            window
                .update(cx, |_, window, _| window.has_image_atlas_entry(&image))
                .expect("窗口应当可读")
        };
        // 把这张图装回 / 摘出元素树，并**真的**画一帧：改完实体还要 `refresh` 才会
        // 再走一遍绘制阶段（第一次试的时候就是漏了这一步，重传看着没发生）。
        let set_on_screen = |cx: &mut TestAppContext, on: bool| {
            let held = if on { Some(image.clone()) } else { None };
            window
                .update(cx, |view, _, _| view.0 = held)
                .expect("视图应当可改");
            cx.update_window(handle, |_, window, _| window.refresh())
                .expect("窗口应当可标脏");
            cx.update_window(handle, |_, _, _| {})
                .expect("窗口应当可画");
        };

        // `open_window` 自己就画了一帧。这一条同时是「`has_image_atlas_entry` 量得到东西」
        // 的探测：它要是永远为真，下面那句「drop 之后为 false」就会红 —— 两句互相排除空跑。
        assert!(
            in_atlas(cx),
            "开完窗口（已经画过一帧）图集里却没有这一格：上传根本没发生，\
             后面所有断言都只是空跑的绿"
        );

        set_on_screen(cx, false);
        cx.update_window(handle, |_, window, app| {
            app.drop_image(image.clone(), Some(window))
        })
        .expect("窗口应当可回收");
        assert!(
            !in_atlas(cx),
            "调用过 `drop_image` 却没放掉：图集那一格收不回来，回收方案全部作废"
        );

        set_on_screen(cx, true);
        assert!(
            in_atlas(cx),
            "重新上屏之后图集里没有这一格 —— 回收过的图不会再画出来"
        );
    }

    /// 回收**不要求**元素先离开树：图还挂在树上时把 tile 放掉，下一帧绘制会自己补回来。
    ///
    /// 这条是来纠正一个我自己踩过的判断的：第一次试的时候最后那句「回屏自动重传」怎么
    /// 都不绿，我据此以为存在「必须先离开树」的硬约束。真正的原因在测试那一侧 ——
    /// `update_window(|_,_,_| {})` 并不保证重画，得先 `window.refresh()` 标脏。
    /// 补上那一步之后两种顺序都成立，所以这条测试钉的是**没有**那条约束：
    /// 实现里可以放心地按「不在可见带 ⇒ 回收」来写，不必等元素从树上消失。
    #[gpui_kit::test]
    fn a_tile_dropped_while_still_painted_comes_back_on_the_next_paint(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let image = bgra_render_image(8, 6);
        let on_screen = image.clone();
        let window = cx.open_window(size(px(64.0), px(64.0)), move |_, _| {
            OneImage(Some(on_screen))
        });
        let handle: AnyWindowHandle = window.into();
        let in_atlas = |cx: &mut TestAppContext| {
            window
                .update(cx, |_, window, _| window.has_image_atlas_entry(&image))
                .expect("窗口应当可读")
        };

        assert!(in_atlas(cx), "前提：图在树上、已经画过一帧");
        cx.update_window(handle, |_, window, app| {
            app.drop_image(image.clone(), Some(window))
        })
        .expect("窗口应当可回收");
        assert!(!in_atlas(cx), "回收这一步没生效，下面那条限制无从谈起");

        // 图还在树上，只是再画两帧。
        for _ in 0..2 {
            cx.update_window(handle, |_, window, _| window.refresh())
                .expect("窗口应当可标脏");
            cx.update_window(handle, |_, _, _| {})
                .expect("窗口应当可画");
        }
        assert!(
            in_atlas(cx),
            "图还在树上、又画了帧却没有补传 —— 那「必须先离开树再回收」这条约束是真的，\
             回收的时机就得跟着收紧"
        );
    }
}
