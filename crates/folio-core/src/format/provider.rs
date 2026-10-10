//! 格式提供者与注册表 —— KOReader `document/documentregistry.lua` 那一层。
//!
//! 一个 provider 认领一类文件、把它读成 [`Content`]，并在需要时交出容器里的字节
//! （图片、内嵌字体）。它**不**管排版、也不管绘制：那两件事是引擎与界面的。
//!
//! 与 KOReader 的差别只有一处：那边 provider 就是 `Document` 子类（解析 + 渲染一体），
//! 这里拆成了「provider（怎么解析）」与 [`crate::document::Document`]（读者看到什么）。

use std::path::Path;

use super::{Content, DocMeta, FontBytes, Format};
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

    /// 只读书目元信息，不建正文模型（学 FBReader 的 `readMetaInfo` / `readModel` 之分）。
    /// 默认实现读完就丢正文；provider 想省这一趟解析可以自己覆盖。
    fn metadata(&self, path: &Path) -> Result<DocMeta> {
        self.read(path).map(|(meta, _)| meta)
    }

    /// 读成文档内容。
    fn read(&self, path: &Path) -> Result<(DocMeta, Content)>;

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

#[cfg(test)]
mod tests {
    //! 注册表：认领与权重。用一个只认 `.zzz` 的假 provider 钉住选择规则 ——
    //! 真实 provider 的扩展名不该被这些断言绑死。

    use std::path::Path;

    use super::{DocumentProvider, provider_for, providers};
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
        fn read(&self, _path: &Path) -> crate::Result<(DocMeta, Content)> {
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
