//! 格式提供者与注册表 —— KOReader `document/documentregistry.lua` 那一层。
//!
//! 一个 provider 认领一类文件、把它读成 [`Content`]，并在需要时交出容器里的字节
//! （图片、内嵌字体）。它**不**管排版、也不管绘制：那两件事是引擎与界面的。
//!
//! 与 KOReader 的差别只有一处：那边 provider 就是 `Document` 子类（解析 + 渲染一体），
//! 这里拆成了「provider（怎么解析）」与 [`crate::document::Document`]（读者看到什么）。
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
                    Ok(Chapter {
                        title: chapter.title.clone(),
                        spine_href: chapter.spine_href.clone(),
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

    /// 容器内某个条目 → 一章的引擎就绪 XHTML。
    ///
    /// 只对**名单式**容器（[`Format::Epub`]，将来的 zip 族）有意义：库里存了
    /// `ChapterLocator::Entry`，打开时按名字现取。默认 `None` 的格式（单文件那几个）
    /// 由 [`crate::document::Document::chapter_source`] 走「整本重读 + 取第 N 章」的
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
        Box::new(super::markdown::PlainTextAdapter),
        Box::new(super::cbz::CbzAdapter),
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
        "epub", "fb2", "html", "htm", "xhtml", "md", "markdown", "txt", "cbz",
    ]
}

/// 扩展名（不分大小写）是否有 provider 认领。
pub fn supports_extension(ext: &str) -> bool {
    supported_extensions()
        .iter()
        .any(|known| known.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
    //! 注册表：认领与权重。用一个只认 `.zzz` 的假 provider 钉住选择规则 ——
    //! 真实 provider 的扩展名不该被这些断言绑死。

    use std::path::Path;

    use super::{DocumentProvider, provider_for, providers, supported_extensions};
    use crate::format::{Content, DocMeta, Format};

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
            ("a.cbz", Format::Cbz),
        ] {
            let provider =
                provider_for(Path::new(name)).unwrap_or_else(|| panic!("{name} 该有 provider"));
            assert_eq!(provider.format(), want, "{name}");
        }
        assert!(
            provider_for(Path::new("a.mobi")).is_none(),
            "没注册就是不认"
        );
        assert_eq!(providers().len(), 6);
    }
}
