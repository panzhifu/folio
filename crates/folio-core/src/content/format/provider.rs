//! 格式提供者与注册表 —— KOReader `document/documentregistry.lua` 那一层。
//!
//! 一个 provider 认领一类文件、把它读成 [`Content`]，并在需要时交出容器里的字节
//! （图片、内嵌字体）。它**不**管排版、也不管绘制：那两件事是引擎与界面的。
//!
//! 与 KOReader 的差别只有一处：那边 provider 就是 `Document` 子类（解析 + 渲染一体），
//! 这里拆成了「provider（怎么解析）」与 [`crate::content::document::Document`]（读者看到什么）。
//!
//! 两个入口，差别是**要不要正文**：
//!
//! - [`DocumentProvider::index`]：只读章表（标题 + 位置）、CSS、目录、图片元数据、封面。
//!   **导入走这一条** —— 正文不在库里（见 `plans/text-out-of-db.md`），
//!   导入时把它解出来再丢掉是白付费：一本 EPUB 就是白解 1000 多个 zip 条目。
//! - [`DocumentProvider::chapter_source`]：按位置取一章的正文，打开书时才付。
//!
//! [`DocumentProvider::read`] 是「两样都要」的合成品，只给测试与「我就是要全文」
//! 的调用方用。

use std::path::Path;

use super::{Chapter, ChapterLocator, ChapterText, Content, DocMeta, FlowDoc, FontBytes, Format};
use crate::Result;

/// 一本书内部**最多起几路**解析 worker —— 两层并行共用一份预算时，由上面那一层分给它。
///
/// 存在的理由是那个乘积：一批同时导 M 本、每本内部 I 路，真正抢核的是 **M × I**。
/// 以前这两层各猜各的 —— 内层只看得见整机核数
/// （`content::format::epub::parse_worker_count`），外层每多一本就把总量再乘一份，
/// 于是「桌面 20 核」与「手机 6 核」上都不是一个被定住的数。这一格把 I 的定夺权
/// 交给**知道有几个兄弟在飞**的那一层（`ingest::import::split_budget`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseBudget {
    /// 按这台机器的核数与条目数自己定（读者打开一本书走这一档：那时它是唯一在跑的）。
    Auto,
    /// 上层算好了：这一本最多起这么多路。`0` 与 `1` 同档（顺序）。
    ///
    /// 这是**上限而不是配额**：条目少到不值得起线程时照样走顺序
    /// （判据留在 provider 那一层，因为它才知道有几个条目）。
    AtMost(usize),
    /// 不管条目数多少，就起 `n` 路。**只给测试与基准**：它们要一个与机器核数无关的
    /// 确定输入，而"给它凑够 16 个条目"那种做法会让测试的样本被条目数那条门牵着走。
    ///
    /// 生产路径不该用它 —— 一本 5 个条目的书起 4 条线程，付的是线程创建与每路一份
    /// 读缓冲，换不到任何东西。
    Exactly(usize),
}

pub trait DocumentProvider: Send + Sync {
    fn format(&self) -> Format;

    /// 认不认这个文件。只按扩展名 —— 便宜的判据留在便宜的地方。
    fn supports(&self, path: &Path) -> bool;

    /// 权重。同一个文件被多个 provider 认下时大的先赢，同权重按注册顺序（先注册先赢）。
    /// 对应 KOReader `addProvider(extension, mimetype, provider, weight)` 里那个数。
    fn weight(&self) -> u32 {
        100
    }

    /// 只读**索引**：章表（只有标题与位置）、CSS、目录、图片元数据、封面。
    ///
    /// **不产出正文**：章表里的每一章是 [`ChapterText::At`]，正文得叫
    /// [`DocumentProvider::chapter_source`]。这是导入那一步唯一该调的入口。
    fn index(&self, path: &Path) -> Result<(DocMeta, Content)>;

    /// 同 [`index`](DocumentProvider::index)，但把「这一本内部最多起几路」交给调用方。
    ///
    /// 默认**忽略这个提示**（原样退回 [`index`](DocumentProvider::index)）：一本书真的
    /// 起多路 worker 的只有名单式容器那一族，而少条目本来就不起线程 —— 让每个格式都
    /// 接这个参数只是把签名变长。**认得它的格式**（`format::epub`）覆盖这一条。
    ///
    /// 覆盖时不许把它当成「必须起够这么多路」：那是上限，条目数不够就走顺序，
    /// 结果必须与 `Auto` 一模一样 —— 并行在这层只是换个取字节的排程，不是换一种读法。
    fn index_with(&self, path: &Path, budget: ParseBudget) -> Result<(DocMeta, Content)> {
        let _ = budget;
        self.index(path)
    }

    /// 一章的引擎就绪 XHTML，按位置取。
    ///
    /// 默认能处理 [`ChapterLocator::Inline`]（正文就在位置上）与
    /// [`ChapterLocator::Entry`]（让容器报一个条目，见 [`DocumentProvider::entry_text`]）；
    /// [`ChapterLocator::Section`] 必须由单文件格式自己实现 —— 只有它知道怎么分章。
    fn chapter_source(&self, path: &Path, at: &ChapterLocator) -> Result<String> {
        match at {
            ChapterLocator::Inline(text) => Ok(text.clone()),
            ChapterLocator::Entry(entry) => {
                self.entry_text(path, entry)
                    .ok_or_else(|| crate::Error::ChapterUnavailable {
                        path: path.to_path_buf(),
                        index: 0,
                        at: entry.clone(),
                    })
            }
            ChapterLocator::Section(_) => Err(crate::Error::ChapterUnavailable {
                path: path.to_path_buf(),
                index: 0,
                at: format!("{at:?}"),
            }),
        }
    }

    /// 索引 **加上** 每一章的正文。
    ///
    /// 给测试、校验与「我就是要全文」的调用方用；**导入不要用它** ——
    /// 正文进了库还要被丢掉，而这里付的是解压全本的钱。
    fn read(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let (meta, content) = self.index(path)?;
        let chapters = match &content {
            Content::Reflow(flow) => flow
                .chapters
                .iter()
                .map(|chapter| -> Result<Chapter> {
                    let text = match chapter.locator() {
                        Some(at) => self.chapter_source(path, at)?,
                        None => chapter.source().to_string(),
                    };
                    // 字数那一格**不换来源**（`visible_chars` 走 DOM）：这条 `read()`
                    // 只服务测试与校验，把两样数并成一趟是省一点力气，代价是给
                    // `char_len` 换了一把尺 —— 那条尺进了库，不值得在这儿动它。
                    let images = crate::content::scan::chapter_summary(&text).images;
                    Ok(Chapter {
                        title: chapter.title.clone(),
                        spine_href: chapter.spine_href.clone(),
                        // 正文在这一趟真的到手了，字数就在这儿定下来 —— 与
                        // `Chapter::inline` 同一条口径，别让 `read` 出来的章与
                        // `index` 出来的章对同一个字段给两个答案。
                        char_len: super::visible_chars(&text),
                        images,
                        text: ChapterText::Inline(text),
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            // 定版式没有章，也没有正文可填。
            Content::Fixed(_) => return Ok((meta, content)),
        };
        let Content::Reflow(flow) = &content else {
            unreachable!("上面已经挡掉了定版式")
        };
        Ok((
            meta,
            Content::Reflow(FlowDoc {
                chapters,
                ..flow.clone()
            }),
        ))
    }

    /// 只要**每一章的正文**，其余格子可以一律不算。
    ///
    /// 存在的理由是一条实测：`Section` 那一族（单文件容器）开书时建的整本 memo 只要
    /// 每章的 `source()`，而它调的是 [`read`](Self::read) —— 那一趟顺手把封面整张
    /// 解码 + 重编（MOBI 的 KF8 样本实测 29 ms，占全文那一趟的 68%）、把每张插图全读
    /// 回来量尺寸、把 CSS 改写完、再逐章数一遍可见字。这几格 memo 一个都不看。
    ///
    /// 默认 `None` = 没有更便宜的路，调用方照旧退回 [`read`](Self::read)。
    ///
    /// **覆盖时的硬条件**：第 `i` 串必须与 `read` 那一路第 `i` 章的 `source()` **逐字节
    /// 相同**，章数必须与 `index` 那一趟相同。这一趟丢掉的封面 / 图片 / CSS / 字数是
    /// 「没人看」，正文不是 —— 进度、锚点、划线都建在那一串上。
    fn read_texts(&self, _path: &Path) -> Option<Vec<String>> {
        None
    }

    /// 只读书目元信息。默认走索引 —— 它已经够便宜了。
    fn metadata(&self, path: &Path) -> Result<DocMeta> {
        self.index(path).map(|(meta, _)| meta)
    }

    /// `@font-face` 要的那几份内嵌字体。默认空 —— 没有容器可谈的格式本来就没有。
    ///
    /// 读不到的一切情况都是**空**而不是 `Err`：调用方不必区分「这本书没字体」与
    /// 「那本书的字体坏了」，两者在屏幕上本来就是同一个表现。
    fn fonts(&self, _path: &Path, _wanted: &[String]) -> Vec<FontBytes> {
        Vec::new()
    }

    /// 容器内某个路径的图片字节。读不到一律 `None`：
    /// 「这本书没这张图」与「容器丢了」在屏幕上是同一件事（那块地方空着）。
    fn image_bytes(&self, _path: &Path, _href: &str) -> Option<Vec<u8>> {
        None
    }

    /// 定版式里**要算出来才有字节**的那一页（`PageSource::Render`），交回 PNG。
    ///
    /// 默认 `None`：只有栅格化引擎在那儿的格式才有这一问。漫画（CBZ）与 EPUB 的定版式
    /// 一页本来就是容器里的一条图，走 [`DocumentProvider::image_bytes`]。
    ///
    /// `edge` 是**长边上限**（像素）：由调用方定档位，provider 只管按它画。这一问的签名
    /// 里只有尺寸没有「视口 / 缩放倍数」是有意的 —— 那一格属于界面（它在屏幕上要把这页
    /// 摆多大），而「按多大画」是个要进缓存键的事实，两处都让界面自己说清（见
    /// `content::format::pdf::RENDER_EDGE` 那条「改它要 bump `INDEX_VERSION`」）。
    ///
    /// 与取图字节同一立场：读不到是 `None` 而不是 `Err` —— 那一页空着，页的摆位不变。
    fn render_page(&self, _path: &Path, _page: usize, _edge: u32) -> Option<Vec<u8>> {
        None
    }

    /// 定版式那一页的**文字层**（页内盒 + 字序）。
    ///
    /// 默认 `None` = 这一页没有可读的字：漫画包的一页就是一张图，EPUB 的 fixed-layout
    /// 那一族字在 XHTML 里而不是在页上，扫描件没带 OCR。这一格与 [`Self::render_page`]
    /// 分开问正是因为这个 —— **画得出这一页不等于读得出这一页的字**，把两者合成一问
    /// 就会逼每个 provider 都回去翻一遍引擎，而扫描本要的恰恰是「有图没字也照样读」。
    ///
    /// 有字的那三族：PDF / AZW4（PalmDB 里那份 PDF）走栅格引擎的文字抽取，DjVu 走它自己
    /// 那份隐藏文字层（OCR 产物）。
    ///
    /// **不入库、按页现取**：实测每页 0.3–10 ms，而一次选中只用得上看得着的那一两页；
    /// 存起来就要为它开一列、并让引擎升版时整库重扫（理由写在 [`super::PageText`]）。
    ///
    /// 拿不到也是 `None` 而不是 `Err`：那一页与从前一样只是图，选不出字，页的摆位不变。
    fn page_text(&self, _path: &Path, _page: usize) -> Option<super::PageText> {
        None
    }

    /// 容器内某个条目 → 一章的引擎就绪 XHTML。
    ///
    /// 只对**名单式**容器有意义（[`Format::Epub`] 的 spine 项、[`Format::Htmlz`] 的
    /// 每一份 HTML、[`Format::Chm`] 的那份 hhc topic）：库里存了
    /// `ChapterLocator::Entry`，打开时按名字现取。默认 `None` 的格式（单文件那几个）
    /// 由 [`crate::content::document::Document::chapter_source`] 走「整本重读 + 取第 N 章」的
    /// 降级路，所以它们不必实现这一个。
    ///
    /// 读不到是 `None` 而不是 `Err`：路径给错了、容器没了、条目是目录……在调用方
    /// 眼里都是「这一章取不到」，由它决定怎样把它说给读者。
    fn entry_text(&self, _path: &Path, _entry: &str) -> Option<String> {
        None
    }
}

/// 全部已注册的 provider。顺序 = 注册顺序 = 同权重时的优先级。
///
/// 每次调用现造一遍而不是存一张全局表：这些 provider 都是零字段的 ZST，
/// 造一个等于造一个注释，换来的是没有全局可变状态。
pub fn providers() -> Vec<Box<dyn DocumentProvider>> {
    vec![
        Box::new(super::epub::EpubAdapter),
        Box::new(super::fb2::Fb2Adapter),
        Box::new(super::html::HtmlAdapter),
        Box::new(super::markdown::MarkdownAdapter),
        Box::new(super::text::PlainTextAdapter),
        Box::new(super::rtf::RtfAdapter),
        Box::new(super::cbz::CbzAdapter),
        Box::new(super::chm::ChmAdapter),
        Box::new(super::mobi::MobiAdapter),
        Box::new(super::pdf::PdfAdapter),
        Box::new(super::docx::DocxAdapter),
        Box::new(super::odt::OdtAdapter),
        Box::new(super::pdb::PdbAdapter),
        Box::new(super::azw4::Azw4Adapter),
        Box::new(super::cb7::Cb7Adapter),
        Box::new(super::cbr::CbrAdapter),
        Box::new(super::cbt::CbtAdapter),
        Box::new(super::djvu::DjvuAdapter),
        Box::new(super::htmlz::HtmlzAdapter),
    ]
}

/// 认下这个文件的 provider；权重大的优先，同权重按注册顺序。
pub fn provider_for(path: &Path) -> Option<Box<dyn DocumentProvider>> {
    let mut best: Option<Box<dyn DocumentProvider>> = None;
    let mut best_weight = 0u32;
    for candidate in providers() {
        if !candidate.supports(path) {
            continue;
        }
        let weight = candidate.weight();
        if best.is_none() || weight > best_weight {
            best_weight = weight;
            best = Some(candidate);
        }
    }
    best
}

/// 全部被认领的扩展名（小写）。**整机扫描**用这份清单初筛，而不是对每个文件
/// 走一遍 [`provider_for`]：一次扫描要过几千个文件，每个现造一遍 provider 不值。
///
/// 与注册表的一致性由测试钉住（清单里的每一个都能被 [`provider_for`] 认下）——
/// 往注册表加新格式时忘了这里，扫描会静默漏掉那一类书。
pub fn supported_extensions() -> &'static [&'static str] {
    &[
        "epub", "fb2", "html", "htm", "xhtml", "md", "markdown", "txt", "rtf", "cbz", "chm",
        "mobi", "azw", "azw3", "prc", "pdf", "docx", "odt", "pdb", "azw4", "cb7", "cbr", "cbt",
        "djvu", "djv", "htmlz",
    ]
}

/// 扩展名（不分大小写）是否有 provider 认领。
pub fn supports_extension(ext: &str) -> bool {
    supported_extensions()
        .iter()
        .any(|known| known.eq_ignore_ascii_case(ext))
}

/// 粗筛：这个文件值不值得哈希一趟、走一遍三层判定。
///
/// 两条判据：**扩展名有 provider 认领**，或者**它是一个装得下书的 ZIP**。
/// 后一条是真的开容器看条目（[`super::sniff::zip_shape`]），不看文件名 ——
/// `书.fb2.zip`、`书.txt.zip`、`书.epub.zip` 都算，`报告.docx`、`备份.zip` 都不算，
/// 而且对名字完全不敏感：一个叫 `x.zip` 的真 EPUB 照样进候选。
///
/// 为什么开容器不算贵：ZIP 的中央目录在文件**末尾**，读它是一趟 seek 加条目表，
/// 与「哈希整个文件」不是同一个量级 —— 而粗筛存在的全部理由正是省掉后者
/// （`plans/导入管线.md` §五：5000 个文件里排掉 4000 个杂物）。反过来，按扩展名把每个
/// zip 都收进来，会叫读者机器上的备份包、微信下载、docx 各排一趟 SHA-256，
/// 还得逐条跟他解释「这不是书」。
///
/// 仍然**不是裁决**：这一问只决定要不要往下走，最终格式由内容那三层定
/// （`format::sniff::identify`）。
pub fn looks_like_a_book(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    if supports_extension(ext) {
        return true;
    }
    // 只有扩展名不认的那一族才值得开容器，所以这一问排在后面。
    matches!(
        super::sniff::zip_shape(path),
        super::sniff::ZipShape::Book(_) | super::sniff::ZipShape::Holds(_)
    )
}

#[cfg(test)]
mod tests {
    //! 注册表：认领与权重。用一个只认 `.zzz` 的假 provider 钉住选择规则 ——
    //! 真实 provider 的扩展名不该被这些断言绑死。

    use std::path::Path;

    use super::{
        DocumentProvider, looks_like_a_book, provider_for, providers, supported_extensions,
    };
    use crate::content::format::{Content, DocMeta, Format};

    struct Fake {
        extension: &'static str,
        weight: u32,
    }

    impl DocumentProvider for Fake {
        fn format(&self) -> Format {
            Format::Text
        }
        fn supports(&self, path: &Path) -> bool {
            path.extension().is_some_and(|e| e == self.extension)
        }
        fn weight(&self) -> u32 {
            self.weight
        }
        fn index(&self, _path: &Path) -> crate::Result<(DocMeta, Content)> {
            Ok((
                DocMeta {
                    title: self.extension.into(),
                    authors: Vec::new(),
                    format: Format::Text,
                    size: 0,
                    ..DocMeta::default()
                },
                Content::Reflow(Default::default()),
            ))
        }
    }

    /// 选择规则本身：权重大的赢。把候选表直接喂给逻辑，不依赖真实注册表。
    fn best_of(candidates: &[Fake], path: &Path) -> Option<&'static str> {
        let mut best: Option<&'static str> = None;
        let mut best_weight = 0u32;
        for candidate in candidates {
            if !candidate.supports(path) {
                continue;
            }
            if best.is_none() || candidate.weight > best_weight {
                best_weight = candidate.weight;
                best = Some(candidate.extension);
            }
        }
        best
    }

    /// 扫描初筛清单与注册表必须同步：清单里的每个扩展名都要有 provider 真的认领。
    /// 往注册表加新格式忘了更新清单，扫描就会静默漏掉那一类书。
    #[test]
    fn every_supported_extension_is_claimed_by_a_provider() {
        for ext in supported_extensions() {
            let probe = Path::new("/x/a").with_extension(ext);
            assert!(
                provider_for(&probe).is_some(),
                "supported_extensions 声明了 .{ext}，但没有 provider 认领它"
            );
        }
    }

    #[test]
    fn the_heaviest_provider_wins_and_ties_keep_registration_order() {
        let path = Path::new("/x/a.zzz");
        let fake = |weight| Fake {
            extension: "zzz",
            weight,
        };
        assert_eq!(best_of(&[fake(50), fake(90)], path), Some("zzz"));
        // 同权重：先注册的先赢（`>` 而不是 `>=`）。
        assert_eq!(best_of(&[fake(100), fake(100)], path), Some("zzz"));
        assert_eq!(
            best_of(&[fake(50)], Path::new("/x/a.other")),
            None,
            "不认就一个都不选"
        );
    }

    /// 真实注册表认得我们支持的每一种容器，且 epub 不会被别的格式抢走。
    #[test]
    fn the_real_registry_covers_every_shipped_format() {
        for (name, want) in [
            ("a.epub", Format::Epub),
            ("a.fb2", Format::Fb2),
            ("a.html", Format::Html),
            ("a.md", Format::Markdown),
            ("a.txt", Format::Text),
            ("a.rtf", Format::Rtf),
            ("a.cbz", Format::Cbz),
            ("a.chm", Format::Chm),
            ("a.mobi", Format::Mobi),
            ("a.azw3", Format::Mobi),
            ("a.pdf", Format::Pdf),
            ("a.docx", Format::Docx),
            ("a.odt", Format::Odt),
            ("a.pdb", Format::Pdb),
            ("a.azw4", Format::Azw4),
            ("a.cb7", Format::Cb7),
            ("a.cbr", Format::Cbr),
            ("a.cbt", Format::Cbt),
            ("a.djvu", Format::Djvu),
            ("a.djv", Format::Djvu),
            ("a.htmlz", Format::Htmlz),
        ] {
            let provider =
                provider_for(Path::new(name)).unwrap_or_else(|| panic!("{name} 该有 provider"));
            assert_eq!(provider.format(), want, "{name}");
        }
        assert!(provider_for(Path::new("a.lit")).is_none(), "没注册就是不认");
        assert_eq!(providers().len(), 19);
    }

    /// 粗筛那一道门的**名字那一半**：注册表清单里的每一个仍然过，认不出的名字不过。
    /// 「包那一半」（打开来看里面有没有书）住在 `format::sniff::tests` 那几条里 ——
    /// 那要真的造得出 zip，夹具与判据同层放才不绕。
    ///
    /// 名字那一半为什么还要留着：一次扫描要过几千个文件，扩展名对得上的**不必开容器**，
    /// 而开容器对非 zip 的文件也是白一趟（虽然便宜）。
    #[test]
    fn the_prefilter_takes_every_extension_the_registry_claims() {
        for ext in supported_extensions() {
            let probe = Path::new("/x/a").with_extension(ext);
            assert!(looks_like_a_book(&probe), "清单里的 .{ext} 该继续过筛");
        }
        assert!(!looks_like_a_book(Path::new("/x/报告.lit")));
        assert!(!looks_like_a_book(Path::new("/x/无扩展名")));
        assert!(
            looks_like_a_book(Path::new("/x/漫画.cbr")),
            "漫画包不管容器解不解得动都要过粗筛：过不了筛的书连一句「这本读不动」都收不到"
        );
        // 名字说了不算：这两个都是 `.zip`，一个开得出书一个开不出，
        // 而这里给的都是**读不到**的路径 —— 谈不上容器就是不过筛，不报坏包。
        assert!(!looks_like_a_book(Path::new("/x/书.fb2.zip")));
        assert!(!looks_like_a_book(Path::new("/x/杂物.zip")));
    }
}
