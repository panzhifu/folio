//! M0 排版地基验证。`cargo run --example spike`
//! spike① 关键问题：shaping（shape_line/shape_text/layout_line）挂在 WindowTextSystem 上，
//!        度量/断行/字体（advance、line_wrapper、resolve_font、add_fonts）挂在 App 级 TextSystem 上。
//!        于是「DOM→Layout 能否后台跑」= 「Arc<WindowTextSystem> 能否 move 进 background 线程并被使用」。
//! spike② canvas + ShapedLine::paint / paint_glyph 自绘（含竖排轴向对调）。
//! spike④ CJK 断行质量（kinsoku 违规检测）。spike③ 字体加载。

use std::borrow::Cow;
use std::sync::Arc;

use gpui_kit::component::*;
use gpui_kit::*;

const HANZI: &str = "排版引擎的可行性不取决于结论有多乐观，而取决于每个结论都有签名可对。";
const MIXED: &str = "Folio 把 position.x 当纵向、position.y 当横向，汉字就竖起来了。";
const WRAP_PROBE: &str = "于是我们奋力向前，逆水行舟——被许多人当作标点排版的检验样本：「了不起的盖茨比」中译本里那句，！？；：『』《》";

const FONT_FAMILY: &str = "LXGW WenKai";
const FONT_PATH: &str = "/home/noke/.local/share/fonts/design/LXGWWenKai-Regular.ttf";

fn ink() -> Hsla {
    gpui_kit::black()
}

fn wenkai(weight: f32) -> Font {
    Font {
        family: FONT_FAMILY.into(),
        weight: FontWeight(weight),
        ..Default::default()
    }
}

fn run(text: &str, font: Font, color: Hsla) -> TextRun {
    TextRun {
        len: text.len(),
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// spike① + ④：整段在后台线程执行，不带 Window。
/// 能编译（两个 Arc 都被 move 进 Send 闭包）且数值合理，即为「可后台排版」的结论。
fn probe_offthread(window_ts: Arc<WindowTextSystem>, app_ts: Arc<TextSystem>, font: Font) {
    let tid = format!("{:?}", std::thread::current().id());
    println!(
        "=== spike① 离窗排版（后台线程 {tid}，name={:?}）===",
        std::thread::current().name()
    );

    let shaped = window_ts.shape_line(
        MIXED.into(),
        px(17.0),
        &[run(MIXED, font.clone(), ink())],
        None,
    );
    println!(
        "WindowTextSystem::shape_line OK：width={:.1} ascent={:.1} descent={:.1} runs={} 字形数={}",
        shaped.width,
        shaped.ascent,
        shaped.descent,
        shaped.runs.len(),
        shaped.runs.iter().map(|r| r.glyphs.len()).sum::<usize>()
    );

    let forced = window_ts.shape_line(
        HANZI.into(),
        px(17.0),
        &[run(HANZI, font.clone(), ink())],
        Some(px(400.0)),
    );
    let natural = window_ts.shape_line(
        HANZI.into(),
        px(17.0),
        &[run(HANZI, font.clone(), ink())],
        None,
    );
    println!(
        "force_width 两端对齐钩子：自然宽 {:.1} → 强制 400 后 width={:.1}",
        natural.width, forced.width
    );

    // 单字度量：App 级，本来就该能离窗。
    let font_id = app_ts.resolve_font(&font);
    let per_char = '排'.escape_debug().to_string();
    println!(
        "TextSystem::advance({per_char}) -> {:?}",
        app_ts.advance(font_id, px(17.0), '排')
    );

    println!("=== spike④ CJK 断行质量（TextSystem::line_wrapper，kinsoku 检查）===");
    let mut wrapper = app_ts.line_wrapper(font, px(17.0));
    let fragments = [LineFragment::text(WRAP_PROBE)];
    let ends: Vec<usize> = wrapper
        .wrap_line(&fragments, px(340.0), IndentAdjustment::NoIndent)
        .map(|b| b.ix)
        .collect();
    let mut start = 0usize;
    for end in ends.iter().chain(std::iter::once(&WRAP_PROBE.len())) {
        let line = &WRAP_PROBE[start..*end];
        let head_ban = line
            .chars()
            .next()
            .is_some_and(|c| "。，、！？」』；：）》".contains(c));
        let tail_ban = line.chars().last().is_some_and(|c| "「『《（".contains(c));
        println!(
            "  [{}] {}",
            if head_ban || tail_ban {
                "违规"
            } else {
                "  ok"
            },
            line
        );
        start = *end;
    }
}

struct SpikeView {
    samples: Vec<Sample>,
    _probe: Task<()>,
}

#[derive(Clone)]
struct Sample {
    label: &'static str,
    text: SharedString,
    font_size: Pixels,
    vertical: bool,
    split_bold: bool,
}

impl SpikeView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let window_ts = window.text_system().clone();
        let app_ts = cx.text_system().clone();
        let font = wenkai(400.0);
        let _probe = cx.spawn(async move |_, cx| {
            cx.background_spawn(async move {
                probe_offthread(window_ts, app_ts, font);
            })
            .await
        });
        Self {
            samples: vec![
                Sample {
                    label: "② 横排 + 同字号混合字重（前半常规 / 后半粗体，runs 一次 shaping）",
                    text: HANZI.into(),
                    font_size: px(16.0),
                    vertical: false,
                    split_bold: true,
                },
                Sample {
                    label: "② 横排中英混排",
                    text: MIXED.into(),
                    font_size: px(16.0),
                    vertical: false,
                    split_bold: false,
                },
                Sample {
                    label: "② 竖排：ShapedGlyph.position.x→纵向、.y→横向，逐 glyph paint_glyph，右起",
                    text: HANZI.chars().take(24).collect::<String>().into(),
                    font_size: px(16.0),
                    vertical: true,
                    split_bold: false,
                },
            ],
            _probe,
        }
    }
}

impl Render for SpikeView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .v_flex()
            .gap_4()
            .p_6()
            .child("Folio M0 spike —— stdout：spike① ③ ④；窗口：spike② 自绘")
            .children(self.samples.clone().into_iter().map(|s| {
                let height = if s.vertical { px(460.0) } else { px(60.0) };
                div().v_flex().gap_1().child(s.label.to_string()).child(
                    canvas(
                        move |_, _, _| s,
                        move |bounds, s, window, cx| {
                            let ts = window.text_system().clone();
                            let runs = if s.split_bold {
                                let half = s.text.len() / 2;
                                let cut = s
                                    .text
                                    .char_indices()
                                    .find(|(i, _)| *i >= half)
                                    .map_or(s.text.len(), |(i, _)| i);
                                vec![
                                    run(&s.text[..cut], wenkai(400.0), ink()),
                                    run(&s.text[cut..], wenkai(700.0), ink()),
                                ]
                            } else {
                                vec![run(&s.text, wenkai(400.0), ink())]
                            };
                            let shaped = ts.shape_line(s.text.clone(), s.font_size, &runs, None);
                            let top = bounds.origin.y + px(24.0);
                            if s.vertical {
                                let column = shaped.width.max(px(24.0));
                                let x0 = bounds.origin.x + bounds.size.width - column;
                                for r in &shaped.runs {
                                    let base_y =
                                        r.glyphs.first().map(|g| g.position.y).unwrap_or(px(0.0));
                                    for g in &r.glyphs {
                                        let origin =
                                            point(x0 + g.position.y - base_y, top + g.position.x);
                                        let _ = window.paint_glyph(
                                            origin,
                                            r.font_id,
                                            g.id,
                                            shaped.font_size,
                                            ink(),
                                        );
                                    }
                                }
                            } else {
                                let _ = shaped.paint(
                                    point(bounds.origin.x, top),
                                    shaped.ascent + shaped.descent + px(8.0),
                                    TextAlign::Left,
                                    None,
                                    window,
                                    cx,
                                );
                            }
                        },
                    )
                    .w_full()
                    .h(height),
                )
            }))
    }
}

fn main() {
    gpui_kit::application().run(|cx: &mut App| {
        gpui_kit::init(cx);

        println!("=== spike③ 字体加载 ===");
        match std::fs::read(FONT_PATH) {
            Ok(bytes) => println!(
                "add_fonts -> {:?}",
                cx.text_system()
                    .add_fonts(vec![Cow::Owned(bytes)])
                    .map(|_| "Ok")
            ),
            Err(e) => println!("读不到 {FONT_PATH}：{e} —— 回退系统字体"),
        }
        let seen: Vec<String> = cx
            .text_system()
            .all_font_names()
            .into_iter()
            .filter(|n| n.contains("LXGW") || n.contains("WenKai"))
            .collect();
        println!("TextSystem 可见 WenKai 家族：{seen:?}");
        println!(
            "resolve_font(LXGW WenKai) -> FontId({:?})",
            cx.text_system().resolve_font(&wenkai(400.0))
        );

        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| SpikeView::new(window, cx))
        })
        .expect("spike window");
    });
}
