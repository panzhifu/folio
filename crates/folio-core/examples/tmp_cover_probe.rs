//! 临时探针：《诡秘之主》第 1 节（cover.xhtml 的 SVG 封面）与第 2 节（top001 制作说明）
//! 在手机那套逻辑尺寸下排出来多高、带不带整页画。用完即删。
use folio_core::content::dom::Dom;
use folio_core::content::format::css::computed::{ComputedStyle, cascade};
use folio_core::content::format::css::{Origin, RuleSet};
use folio_core::layout::{
    FontKey, ImageProvider, LayoutOptions, LineFragment, LineMetrics, Measure, ResolvedImage,
    layout,
};

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

/// 真图的原生尺寸（从 epub 里量出来的）。
struct Real;
impl ImageProvider for Real {
    fn resolve(&self, src: &str) -> Option<ResolvedImage> {
        let name = src.rsplit('/').next().unwrap_or(src);
        let (w, h) = match name {
            "0.jpg" => (1000.0, 1333.0),
            "0~slim.jpg" => (1080.0, 2400.0),
            "p0.jpg" => (1080.0, 2400.0),
            _ => return None,
        };
        Some(ResolvedImage {
            href: name.to_string(),
            width: w,
            height: h,
        })
    }
}

fn one(name: &str, css: &str, width: f32, page: Option<f32>, screen: (f32, f32)) {
    let src = std::fs::read_to_string(format!("/tmp/guiji/{name}")).unwrap();
    let dom = Dom::parse(src.as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse(css, Origin::Author),
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
            width,
            light: false,
            allow_vertical: false,
            page_height: page,
            screen_width: Some(screen.0),
            screen_height: Some(screen.1),
        },
        &Mono,
        &Real,
    );
    println!(
        "== {name} 版心 {width} page {page:?} screen {screen:?}\n   高 {}  块 {}  行 {}",
        laid.height,
        laid.blocks.len(),
        laid.lines.len()
    );
    let mut big = 0.0;
    for (i, line) in laid.lines.iter().enumerate() {
        for frag in &line.fragments {
            match frag {
                LineFragment::Image(run) => {
                    big = run.width;
                    println!(
                        "   line {i} IMG {} box {}x{} y {}",
                        run.href, run.width, run.height, line.y
                    );
                }
                LineFragment::Text(run) if run.width > 0.0 => {
                    println!("   line {i} TEXT w {} y {}", run.width, line.y)
                }
                _ => {}
            }
        }
    }
    println!("   最大图宽 {big}（版心的 {:.2} 倍）", big / width);
    for (i, seed) in laid.decor_seeds(&styles).iter().enumerate() {
        println!(
            "   seed {i} full_bleed={} rect=({:.1},{:.1},{:.1},{:.1}) pics={}",
            seed.full_bleed,
            seed.x,
            seed.y,
            seed.width,
            seed.height,
            seed.pictures.len()
        );
    }
}

fn main() {
    let css = format!(
        "{}\n{}",
        std::fs::read_to_string("/tmp/guiji/main.css").unwrap(),
        std::fs::read_to_string("/tmp/guiji/fonts.css").unwrap()
    );
    // 魅族 20：1080x2400 @ density 450 → 逻辑 384x853。页眉带+安全区 218/2.8125=77.5，
    // 页脚 114/2.8125=40.6 → 内容区 735；版心 = 384 - 2*margin_x。
    let (w, h) = (384.0, 853.0);
    let content = h - 77.5 - 40.6;
    for margin in [27.0, 0.0] {
        let pane = w - 2.0 * margin;
        one("cover.xhtml", &css, pane, Some(content), (w, h));
        one("top001.xhtml", &css, pane, Some(content), (w, h));
    }
}
