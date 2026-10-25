//! 临时复现：《人类简史》图文版在宿主上走完整排版 + 分片 + 锚点映射，
//! 找真机上「往前翻页」的那个 char boundary panic。用假度量（每字 1em）。
use folio_core::content::anchor::TextMap;
use folio_core::content::dom::Dom;
use folio_core::content::format::css::computed::{ComputedStyle, cascade};
use folio_core::content::format::css::{Origin, RuleSet};
use folio_core::layout::fragment::{FragmentOptions, fragments};
use folio_core::layout::{FontKey, LayoutOptions, LineMetrics, Measure, NoImages, layout};

struct StdMeasure;

impl Measure for StdMeasure {
    fn char_width(&self, _ch: char, _f: &FontKey, s: f32) -> f32 {
        s
    }
    fn run_metrics(&self, text: &str, _f: &FontKey, s: f32) -> LineMetrics {
        LineMetrics {
            width: text.chars().count() as f32 * s,
            ascent: s * 0.8,
            descent: s * 0.2,
        }
    }
}

fn main() {
    let css = std::fs::read_to_string("/tmp/sapiens/book.css").unwrap();
    let mut paths: Vec<_> = std::fs::read_dir("/tmp/sapiens")
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "xhtml"))
        .collect();
    paths.sort();

    for path in &paths {
        let src = std::fs::read_to_string(path).unwrap();
        let dom = Dom::parse(src.as_bytes());
        let rules = RuleSet::parse(&css, Origin::Author);
        let styles = cascade(&dom, &rules, &ComputedStyle::default(), "");
        let root = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map(|(id, _)| id)
            .unwrap_or(dom.document);
        let map = TextMap::build(&dom, root);
        let laid = layout(
            &dom,
            &styles,
            root,
            LayoutOptions {
                width: 370.0,
                light: false,
                allow_vertical: false,
                page_height: None,
            },
            &StdMeasure,
            &NoImages,
        );
        // 锚点映射（chars_of_bytes 的主调用方）
        for line in 0..laid.lines.len() {
            let _ = folio_core::content::anchor::line_chars(&laid, &dom, &map, line);
        }
        // 分片
        let options = FragmentOptions {
            height: 640.0,
            ..FragmentOptions::default()
        };
        let all = fragments(&laid, &styles, options);
        println!(
            "{}: lines={} fragments={}",
            path.file_name().unwrap().to_string_lossy(),
            laid.lines.len(),
            all.len()
        );
    }
    println!("no panic");
}
