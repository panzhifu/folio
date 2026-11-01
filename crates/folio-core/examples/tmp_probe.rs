use folio_core::content::dom::Dom;
use folio_core::content::format::css::computed::{ComputedStyle, cascade};
use folio_core::content::format::css::{Origin, RuleSet};
use folio_core::layout::{FontKey, LayoutOptions, LineMetrics, Measure, NoImages, layout};
struct Mono;
impl Measure for Mono {
    fn char_width(&self, _c: char, _f: &FontKey, s: f32) -> f32 {
        s
    }
    fn run_metrics(&self, t: &str, _f: &FontKey, s: f32) -> LineMetrics {
        LineMetrics {
            width: t.chars().count() as f32 * s,
            ascent: s * 0.8,
            descent: s * 0.2,
        }
    }
}
fn main() {
    let src = std::fs::read_to_string("/tmp/fm001.xhtml").unwrap();
    let css = format!(
        "{}\n{}",
        std::fs::read_to_string("/tmp/main.css").unwrap(),
        std::fs::read_to_string("/tmp/fonts.css").unwrap()
    );
    let dom = Dom::parse(src.as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse(&css, Origin::Author),
        &ComputedStyle::default(),
        "",
    );
    let body = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map(|(id, _)| id)
        .unwrap();
    let laid = layout(
        &dom,
        &styles,
        body,
        LayoutOptions {
            width: 400.0,
            light: false,
            allow_vertical: false,
            page_height: Some(800.0),
            screen_width: Some(400.0),
            screen_height: Some(900.0),
        },
        &Mono,
        &NoImages,
    );
    println!("height {} lines {}", laid.height, laid.lines.len());
    for (i, s) in laid.decor_seeds(&styles).iter().enumerate() {
        println!(
            "seed {i} full_bleed={} bg={:?} pictures={} rect=({},{},{},{})",
            s.full_bleed,
            s.background,
            s.pictures.len(),
            s.x,
            s.y,
            s.width,
            s.height
        );
    }
}
