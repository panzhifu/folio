//! 随书字体的别名表 —— **app 侧只剩桥**。
//!
//! 管线本体（`@font-face` → 身份去重 → 读字节 → 子集化 → 改名 → 别名表）住在
//! [`folio_core::text::load`]；2026-11-09 下沉，gpui 的接触点收进 [`TextSystemSink`]
//! 一个实现。这一层保留三样东西：
//!
//! - [`TextSystemSink`]：`TextSystem` 的三个调用点（`add_fonts_with_families` /
//!   `all_font_names` / `font_has_glyph`）包成 core 的 [`FontSink`]；
//! - [`for_document`] / [`for_texts`]：两行壳，从 `App` / `TextSystem` 拿 sink 再转调；
//! - 要窗口或真书 fixture 的那几条测试（纯逻辑的门随管线进了 core）。
//!
//! 历史口径（primary 闸、子集命名、身份缓存）的说明都在 core 那份的行内注释里。

use std::sync::Arc;

use folio_core::text::load::FontSink;
use gpui_kit::TextSystem;

// 名称原样可用：调用点（book / chapter_layout / measure / 宿主测试）沿用的就是这些。
// `FaceName` 只有度量与测试在用；prod 侧真正的是下面三个。
#[allow(unused_imports)]
pub use folio_core::text::load::note_external_registration;
pub use folio_core::text::load::{BookFonts, FontLoadJob, FontRequest};

// `for_document` / `for_texts` 是测试与探针的同步入口；生产走 `FontLoadJob`。
#[cfg(test)]
use folio_core::content::document::Document;
#[cfg(test)]
use folio_core::text::load::FontLoadStats;

/// [`FontSink`] 的 app 实现：包着排版作业手里那份 `Arc<TextSystem>`。
///
/// 三个方法都是直转，没有逻辑 —— 这正是下沉的全部意义：core 管线测得到的那些分支，
/// 从此不再需要开窗口。
pub struct TextSystemSink(pub Arc<TextSystem>);

impl FontSink for TextSystemSink {
    fn add_faces(&self, faces: Vec<Vec<u8>>) -> Option<Vec<Vec<String>>> {
        // gpui 的批量注册收 `Cow<'static, [u8]>`；这里全是刚解出来的字节，全部 Owned。
        let owned = faces.into_iter().map(std::borrow::Cow::Owned).collect();
        let family_sets = self.0.add_fonts_with_families(owned).ok()?;
        Some(family_sets)
    }

    fn known_families(&self) -> Vec<String> {
        self.0.all_font_names()
    }

    fn family_has_glyph(&self, family: &str, ch: char) -> bool {
        let font = gpui_kit::Font {
            family: family.to_string().into(),
            ..Default::default()
        };
        self.0.font_has_glyph(&font, ch)
    }
}

/// 从一本书的 CSS 与容器建别名表。同步入口：测试与 `font_load_bench` 用；生产开书
/// 走 [`FontLoadJob`]（排版作业），避免在 UI 线程读字节。
#[cfg(test)]
pub fn for_document(
    doc: &dyn Document,
    css: &str,
    cx: &gpui_kit::App,
) -> (std::sync::Arc<BookFonts>, FontLoadStats) {
    let rules = folio_core::content::format::css::RuleSet::parse(
        css,
        folio_core::content::format::css::Origin::Author,
    );
    let request = FontRequest::from_rules(&rules);
    let sink = TextSystemSink(cx.text_system().clone());
    BookFonts::for_request(&sink, doc, &request)
}

#[cfg(test)]
mod tests {
    //! 要窗口或真书 fixture 的那几条。纯逻辑的门在
    //! [`folio_core::text::load`](folio_core::text::load) 的测试里。

    use folio_core::content::format::css::{Origin, RuleSet};
    use gpui_kit::TestAppContext;

    use folio_core::text::load::{
        FontId, charset_hash, charset_with_ascii, rename_subset_family, subset_family_tag,
        subset_font_bytes,
    };

    use super::{BookFonts, FontRequest, TextSystemSink, for_document};

    /// 第二次打开同一本 HP：身份缓存命中，`read_font` 一次都不调。
    ///
    /// 忽略：真书只在 `tests/fixtures/local/`，不入库。手动跑：
    /// `cargo test -p folio-app --lib -- --ignored --nocapture the_second_open_reads_no_font_bytes`
    #[ignore = "真书 HP 不在仓库；手动跑"]
    #[gpui_kit::test]
    fn the_second_open_reads_no_font_bytes(cx: &mut TestAppContext) {
        use std::path::PathBuf;

        cx.update(gpui_kit::init);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/local/hali-bote-quanji.epub");
        if !path.is_file() {
            eprintln!("{} 不在，跳过", path.display());
            return;
        }
        let doc = folio_core::content::document::open(&path).expect("打开 HP");
        let css = doc.stylesheet();
        let rules = RuleSet::parse(&css, Origin::Author);
        let request = FontRequest::from_rules(&rules);
        let (_, first) = cx.update(|cx| {
            let sink = TextSystemSink(cx.text_system().clone());
            BookFonts::for_request(&sink, doc.as_ref(), &request)
        });
        let (_, second) = cx.update(|cx| {
            let sink = TextSystemSink(cx.text_system().clone());
            BookFonts::for_request(&sink, doc.as_ref(), &request)
        });
        assert_eq!(first.faces, 2, "HP 有两份内嵌字体");
        assert_eq!(first.bytes_read, 2, "第一次要读两份字节");
        assert_eq!(second.bytes_read, 0, "第二次命中身份缓存，不该再读字节");
        assert_eq!(second.cache_hits, 2, "两份都该命中");
    }

    /// worker 先问 `font_refs_in`（共享容器），只有它给不出时才退回自己开容器的
    /// `font_refs`。这条门用一个「`font_refs_in` 有值、`font_refs` 一调就 panic」的
    /// 文档钉住顺序 —— 反了就会在真实的 EPUB 上重开一次 ZIP、再解析一遍 OPF。
    #[gpui_kit::test]
    fn the_worker_prefers_the_shared_container_over_opening_its_own(cx: &mut TestAppContext) {
        use folio_core::content::document::{ContentRef, Document as OpenedDocument};
        use folio_core::content::format::{
            Content, ContentKind, DocMeta, FlowDoc, FontRef, Format,
        };

        struct SharedOnly(Box<dyn OpenedDocument>);
        impl OpenedDocument for SharedOnly {
            fn meta(&self) -> &DocMeta {
                self.0.meta()
            }
            fn kind(&self) -> ContentKind {
                self.0.kind()
            }
            fn content(&self) -> ContentRef<'_> {
                self.0.content()
            }
            fn image_bytes(&self, href: &str) -> Option<Vec<u8>> {
                self.0.image_bytes(href)
            }
            fn stylesheet(&self) -> String {
                self.0.stylesheet()
            }
            fn chapter_source(&self, index: usize) -> folio_core::content::format::Result<String> {
                self.0.chapter_source(index)
            }
            fn font_refs_in(&self, _wanted: &[String]) -> Option<Vec<FontRef>> {
                Some(vec![FontRef {
                    href: "x".to_string(),
                    media_type: "font/ttf".to_string(),
                    raw_len: 1,
                    crc32: 1,
                }])
            }
            fn font_refs(&self, _wanted: &[String]) -> Option<Vec<FontRef>> {
                panic!("共享容器已经给出身份，不该再开第二个 ZIP")
            }
        }

        cx.update(gpui_kit::init);
        let inner = folio_core::content::document::from_content(
            DocMeta {
                title: "共享容器".into(),
                format: Format::Epub,
                ..DocMeta::default()
            },
            Content::Reflow(FlowDoc::default()),
        );
        let doc = SharedOnly(inner);
        let request = FontRequest {
            wanted: vec!["x".to_string()],
            declared_of: [("x".to_string(), "Fam".to_string())].into_iter().collect(),
            faces: Vec::new(),
        };
        let (_, stats) = cx.update(|cx| {
            let sink = TextSystemSink(cx.text_system().clone());
            BookFonts::for_request(&sink, &doc, &request)
        });
        assert_eq!(stats.faces, 1, "用的就是共享容器给的那一份");
        assert_eq!(
            stats.failed, 1,
            "没有容器的测试文档读不到字节，只记一次失败"
        );
    }

    /// 判据必须与 gpui 的 `charmap.map('m') == 0` 同源：随包的 emoji 字体没有 `m`
    /// （有数字与 #、*，没有拉丁字母），所以它当不了 primary；名字对不上时按整份字体
    /// 「有没有任何一张脸带 m」回答，不因为比对失败就把字体丢掉。
    #[test]
    fn a_font_without_latin_m_is_not_primary_material() {
        use folio_core::text::latin_by_family;

        // fixture 是 app 的随包资产，路径相对本 crate —— 留在这一侧。
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("android/app/src/main/assets/fonts/NotoColorEmoji.ttf");
        let bytes = std::fs::read(&path).expect("随包的 emoji 字体在仓库里");
        assert!(!latin_by_family(&bytes).covers("Noto Color Emoji"));
        assert!(
            !latin_by_family(&bytes).covers("A Family That Is Not In This Font"),
            "名字对不上时退回整份字体：这份里一张带 m 的脸都没有"
        );
        assert!(
            !latin_by_family(b"not a font").any(),
            "认不出的字节不 panic"
        );
    }

    /// 真书 HP：导入收的整本字符集 → skera 子集 → 家族名加唯一后缀。
    ///
    /// 忽略：真书只在 `tests/fixtures/local/`，不入库。手动跑：
    /// `cargo test -p folio-app --lib -- --ignored --nocapture hp_font_subset_is_renamed_and_smaller`
    #[test]
    #[ignore = "真书 HP 不在仓库；手动跑"]
    fn hp_font_subset_is_renamed_and_smaller() {
        use folio_core::text::{SUBSET_MARKER, latin_by_family};

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/local/hali-bote-quanji.epub");
        if !path.is_file() {
            eprintln!("{} 不在，跳过", path.display());
            return;
        }
        let doc = folio_core::content::document::open(&path).expect("打开 HP");
        let charset = charset_with_ascii(doc.font_charset());
        assert!(
            charset.len() > 1000,
            "导入该收到整本字符集：{}",
            charset.len()
        );
        let hash = charset_hash(&charset);
        let css = doc.stylesheet();
        let rules = RuleSet::parse(&css, Origin::Author);
        let request = FontRequest::from_rules(&rules);
        let refs = doc.font_refs(&request.wanted).expect("EPUB 有身份");
        assert_eq!(refs.len(), 2, "HP 有两份内嵌字体");

        for reference in refs {
            let id = FontId::of_ref(&reference);
            let bytes = doc.read_font(&reference).expect("读一份");
            let subset = subset_font_bytes(&bytes, &charset).expect("skera 该切得动");
            assert!(
                subset.len() < bytes.len(),
                "{}: {} B 的子集不该比原字体大",
                reference.href,
                subset.len()
            );
            let renamed = rename_subset_family(&subset, subset_family_tag(id, hash)).expect("改名");
            let coverage = latin_by_family(&renamed);
            let names: Vec<String> = coverage.families().map(str::to_string).collect();
            assert!(
                names.iter().all(|name| name.contains(SUBSET_MARKER)),
                "子集家族名必须带唯一后缀：{names:?}"
            );
            assert!(
                names.iter().any(|name| coverage.covers(name)),
                "子集必须还带拉丁 m：{names:?}"
            );
            println!(
                "{}: {} B -> {} B ({:.1}%), families={names:?}",
                reference.href,
                bytes.len(),
                renamed.len(),
                100.0 * renamed.len() as f64 / bytes.len() as f64,
            );
        }
    }

    /// 真实容器 + 真实文本系统的开书计时；只在手动读数字时跑。
    ///
    /// 跑法：
    /// ```text
    /// cargo test -p folio-app --lib --release -- --ignored --nocapture font_load_bench
    /// ```
    #[ignore = "量墙钟，只在读数字时手动跑"]
    #[gpui_kit::test]
    fn font_load_bench(cx: &mut TestAppContext) {
        use std::time::Instant;

        use folio_core::text::latin_by_family;

        cx.update(gpui_kit::init);
        // 默认 HP；`FOLIO_FONT_BENCH=<路径>` 指任何一本在盘上的书（换大本读数字用）。
        let path = std::env::var("FOLIO_FONT_BENCH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/local/hali-bote-quanji.epub")
            });
        if !path.is_file() {
            eprintln!("{} 不在，跳过（真书不入库）", path.display());
            return;
        }
        let doc = folio_core::content::document::open(&path).expect("打开 HP");
        let css = doc.stylesheet();
        // 先把 href 对一遍：如果 CSS 的 wanted 和容器回来的 href 不同形，别名表空是路径
        // 对不上，不是注册失败。
        let rules = RuleSet::parse(&css, Origin::Author);
        let mut wanted: Vec<String> = Vec::new();
        for face in &rules.font_faces {
            for src in &face.src {
                if let folio_core::content::format::css::FontSrc::Url(href) = src
                    && !wanted.iter().any(|existing| existing == href)
                {
                    wanted.push(href.clone());
                }
            }
        }
        let faces = doc.fonts(&wanted);
        println!("wanted 条数={}", wanted.len());
        for face in &faces {
            // 每份字体：大小、它每一张脸带不带拉丁 `m`。
            let coverage = latin_by_family(&face.bytes);
            let mut names: Vec<String> = coverage.families().map(str::to_string).collect();
            names.sort();
            println!(
                "  {:<28} {:>9} B  any_m={}  {}",
                face.href,
                face.bytes.len(),
                coverage.any(),
                names
                    .iter()
                    .map(|name| format!("{name}={}", coverage.covers(name)))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        let missing: Vec<&str> = wanted
            .iter()
            .map(String::as_str)
            .filter(|href| !faces.iter().any(|face| face.href == *href))
            .collect();
        println!(
            "CSS 点了 {} 条，容器里真有 {} 份，**对不到文件的 {} 条**：{missing:?}",
            wanted.len(),
            faces.len(),
            missing.len()
        );
        for round in 1..=3 {
            let started = Instant::now();
            let (fonts, stats) = cx.update(|cx| for_document(doc.as_ref(), &css, cx));
            let total_us = started.elapsed().as_micros();
            println!(
                "round {round}: total_us={total_us} parse_us={} refs_us={} subset_us={} bytes_us={} register_us={} faces={} failed={} empty_diff={} subset={}->{} subset_fail={} families={}",
                stats.parse_ns / 1000,
                stats.refs_ns / 1000,
                stats.subset_ns / 1000,
                stats.bytes_ns / 1000,
                stats.register_ns / 1000,
                stats.faces,
                stats.failed,
                stats.empty_diff,
                stats.subset_bytes_before,
                stats.subset_bytes_after,
                stats.subset_failures,
                fonts.families_len(),
            );
            // `TestAppContext` 的文本系统可能整类忽略 `add_fonts`（`families=0`、
            // `empty_diff=faces`）；这条 ignored 探针只负责把数打出来，真机注册耗时要
            // 在设备上量，不在宿主测试里假装测到。
        }
    }
}
