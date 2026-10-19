//! 竖排（`writing-mode: vertical-*`）的几何门。
//!
//! 这一片全部围绕**转置**本身，一个像素都不碰界面。判据的形状只有一条：
//! 同一份 XHTML、同一个折行纯量，横排排一遍、竖排排一遍，然后把两份对起来看 ——
//! 竖排里「沿推进轴」的那些数必须与横排逐字相同，只有物理的哪一条边换了名字。
//! 这样写的好处是不用算期望值：漏了翻面、把轴接反、符号错一次，自己就会露出来，
//! 而这三种恰恰是手算期望值最容易「挑一个恰好对的数」蒙过去的错。
//!
//! 度量用的是测试里那份 `Mono`：它只认「沿字走了多少」这一个纯量，本来就没有轴。
//! 这正是要钉的事 —— **转轴不该改变任何断点**。

use super::{Mono, find, typeset};
use crate::content::dom::{Dom, NodeId};
use crate::content::format::css::computed::{ComputedStyle, StyledDom, cascade};
use crate::content::format::css::{Origin, RuleSet};
use crate::layout::fragment::{Fragment, FragmentOptions, fragments};
use crate::layout::{Layout, LayoutOptions, LineBox, NoImages, layout};

/// 排一遍，同时把样式表留着（分片器要它读断点意图，`typeset` 那一份没带出来）。
fn typeset_kept(html: &str, css: &str, wrap: f32) -> (Dom, StyledDom, Layout) {
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse(css, Origin::Author),
        &ComputedStyle::default(),
    );
    let root: NodeId = find(&dom, "body");
    let laid = layout(
        &dom,
        &styles,
        root,
        LayoutOptions {
            width: wrap,
            light: false,
            allow_vertical: true,
        },
        &Mono,
        &NoImages,
    );
    (dom, styles, laid)
}

/// 够折出二十来条行盒的三段，另有短段专门看块与块之间的接缝。
const PROSE: &str = "<p>中文足够长的一段，长到必须折成好几行才排得下，这样才有好几条行盒可以对比。\
                     再多写几句，好让行数配得上「接缝」这两个字：折行处、留白处、\
                     块与块的接缝处，都该有东西可探，不然转置错了也没人替我说话。</p>\
                     <p>第二段同样长，为的是让第二个块盒自己占上几行，\
                     这样「一个块跨两页」与「块边界正好落在页边」这两种情况都会发生一次。</p>\
                     <p>第三段。</p>";

/// 凡是「从物理坐标里减回来」的那几格，按容差比。
///
/// 转置是 `x = 总长 − (y + 厚)`，读回来又是 `总长 − (x + 厚)`：两次减法一趟，
/// 结果带 1e-5 量级的浮点误差（实测 `18.799988` 对 `18.8`；横排那一句
/// `(行顶 + 基线) − 行顶` 同样给 `18.800003`，所以这不是转置独有的，是减法本身的）。
/// 一像素的千分之一远低于任何屏幕能分辨的量，所以这里不该要求逐位相等 ——
/// 而直接从逻辑值搬过去、没有重算的那几个数（厚度、内轴起点、基线深度）
/// 照旧用 `assert_eq!` 精确比。
fn recovered_eq(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

/// 横排与竖排是同一份几何，只是两条边换了名字。
///
/// 逐条对的是**沿轴**的那三个纯量：推进轴起点、推进轴厚度、内联轴起点 —— 这三个在两根轴上
/// 必须是同一个数。基线不在这一列里：它是物理坐标，两档各在自己的那条侧轴上，
/// 要钉的是「距本行起点边永远等于 `baseline`」那一条式子，两档各写一遍。
/// 三个纯量都相等、投影确实对调，转置就是对的 —— 这里不需要任何手算的期望值。
#[test]
fn vertical_preserves_every_flow_number_and_swaps_the_axes() {
    let (_, hz) = typeset(PROSE, "", 240.0);
    let (_, vt) = typeset(PROSE, "body { writing-mode: vertical-rl }", 240.0);
    assert!(
        hz.lines.len() >= 8,
        "这段得折出足够多的行才有东西可比：{}",
        hz.lines.len()
    );
    assert_eq!(
        hz.lines.len(),
        vt.lines.len(),
        "转轴不该改变断点：`Mono` 只算沿字走了多少"
    );
    for i in 0..hz.lines.len() {
        assert_eq!(hz.line_text(i), vt.line_text(i), "第 {i} 行的内容");
        let a = &hz.lines[i];
        let b = &vt.lines[i];
        assert!(
            recovered_eq(hz.flow_start(a), vt.flow_start(b)),
            "第 {i} 行沿推进轴的起点：{} 对 {}",
            hz.flow_start(a),
            vt.flow_start(b)
        );
        assert_eq!(
            hz.flow_thickness(a),
            vt.flow_thickness(b),
            "第 {i} 行沿推进轴的厚度"
        );
        assert_eq!(
            hz.inline_start(a),
            vt.inline_start(b),
            "第 {i} 行的内联轴起点"
        );
        // 基线**不是**跨档相等的那一个数（它是物理坐标，两档各自的侧轴不同条）。
        // 这里钉的是它各自那条式子：距「本行的起点边」永远等于 `baseline` ——
        // 横排从行顶往右下量，`vertical-rl` 从列的右缘往左量。写反号就死在这一句。
        assert_eq!(a.baseline, b.baseline, "基线深度这一个纯量不因转轴而变");
        assert!(
            recovered_eq(hz.cross_baseline(a) - a.y, a.baseline),
            "横排：基线 = 行顶 + 深度"
        );
        assert!(
            recovered_eq((b.x + b.width) - vt.cross_baseline(b), b.baseline),
            "竖排 rl：基线 = 列的右缘 − 深度"
        );
        // 物理那两条边确实换了名字：横排的 (x, y) 在竖排里成了 (y 那格, x 那格)。
        assert_eq!(
            hz.project(hz.inline_start(a), hz.cross_baseline(a)),
            (a.x, hz.cross_baseline(a)),
            "横排的投影就是恒等"
        );
        assert_eq!(
            vt.project(vt.inline_start(b), vt.cross_baseline(b)),
            (vt.cross_baseline(b), b.y),
            "竖排：内联轴落在物理 y，侧轴落在物理 x"
        );
    }
    // 整章那两条边：折行宽落成物理的高，长条的总长落成物理的宽。
    assert_eq!(hz.width, vt.height, "折行宽 = 列长 = 物理的高");
    assert_eq!(hz.height, vt.width, "长条总长 = 物理的宽");
}

/// `vertical-rl` 的列从右往左堆：物理 `x` 随阅读顺序**递减**，而推进轴上的起点递增。
///
/// 这一条钉的就是那个翻面。少了它，症状是「第一页是这本书的最后一页」——
/// 每一页内部看着都正常，只有翻页的方向反了，比整章排坏更难查。
#[test]
fn vertical_rl_stacks_columns_from_the_right() {
    let (_, vt) = typeset(PROSE, "body { writing-mode: vertical-rl }", 240.0);
    assert!(vt.vertical());
    let mut prev: Option<&LineBox> = None;
    for line in &vt.lines {
        if let Some(p) = prev {
            assert!(
                line.x <= p.x,
                "阅读顺序往后一列应该更靠左：{} 之后是 {}",
                p.x,
                line.x
            );
            assert_eq!(p.y, line.y, "每一列都从同一个列顶开始");
        }
        prev = Some(line);
    }

    // 同一份内容排成 `vertical-lr`：那一档不翻面，物理 x 递增。
    let (_, lr) = typeset(PROSE, "body { writing-mode: vertical-lr }", 240.0);
    let mut prev: Option<&LineBox> = None;
    for line in &lr.lines {
        if let Some(p) = prev {
            assert!(
                line.x >= p.x,
                "lr 那一档列从左往右堆：{} 之后是 {}",
                p.x,
                line.x
            );
        }
        prev = Some(line);
    }
    // 两档的阅读顺序完全一致，只有摆的位置不同。
    assert_eq!(vt.lines.len(), lr.lines.len());
}

/// 基线那个符号：`depth` 为正在横排是往下，在 `vertical-rl` 必须往**左**。
///
/// 单独一条门是因为这一处错了以后画面仍然「像竖排」—— 字都竖着，只是每列里的字
/// 跑到基线另一侧去，而两处各自看起来都合理。
#[test]
fn baseline_depth_goes_left_in_rl_and_down_when_horizontal() {
    let (_, hz) = typeset(PROSE, "", 240.0);
    let (_, vt) = typeset(PROSE, "body { writing-mode: vertical-rl }", 240.0);
    let line = &hz.lines[2];
    assert!(
        hz.cross_from_baseline(line, 3.0) > hz.cross_baseline(line),
        "横排：正数往下"
    );
    let line = &vt.lines[2];
    assert!(
        vt.cross_from_baseline(line, 3.0) < vt.cross_baseline(line),
        "竖排 rl：同一个正数往左"
    );
    // 基线落在这一列的两条边之间，不许跑到盒子外面去。
    assert!(
        line.x < vt.cross_baseline(line) && vt.cross_baseline(line) < line.x + line.width,
        "基线得落在这一列的两条边之间：x {}，宽 {}，基线 {}",
        line.x,
        line.width,
        vt.cross_baseline(line)
    );
}

/// 沿推进轴的那三个查询在竖排下仍然答「阅读顺序上的那一条行」。
///
/// 判据全部从行盒自己反推，不写死任何数：每一行的起点加一点，问回来必须是它自己；
/// 覆盖整条长带的可见带要给全每一行；一列宽的带正好圈住一列。
#[test]
fn the_flow_queries_answer_in_reading_order_when_vertical() {
    let (_, vt) = typeset(PROSE, "body { writing-mode: vertical-rl }", 240.0);
    assert!(vt.stacked(), "转置后行盒照样不许互相压住");
    for i in 0..vt.lines.len() {
        let probe = vt.flow_start(&vt.lines[i]) + 0.5;
        assert_eq!(
            vt.line_at(probe),
            Some(i),
            "第 {i} 行的起点问回来必须是它自己"
        );
        assert_eq!(vt.line_covering_or_above(probe), i);
    }
    assert_eq!(
        vt.lines_in_band(0.0, vt.width + 1.0),
        0..vt.lines.len(),
        "覆盖整条长带的可见带要给全每一行"
    );
    let first = &vt.lines[0];
    assert_eq!(
        vt.lines_in_band(vt.flow_start(first), vt.flow_end(first)),
        0..1,
        "一列厚的带就该只圈住第一列"
    );
}

/// 分片器在竖排下切的是**列**：可用长度给页宽，一片就是一页里的几列。
///
/// 三条判据一条都不能松：片与片按行号首尾相接（漏一行就是「翻页翻掉一段」）、
/// 每片沿推进轴的总厚不超过页宽、片原点沿阅读顺序递增（切片与翻页都靠它排序）。
/// 最后再与横排那份对一次切点 —— 分片器问的是推进轴，两根轴上同一串厚度必须切在同一处。
#[test]
fn the_fragmenter_slices_columns_by_the_page_width() {
    let page_width = 100.0f32;
    let (_, styles, vt) = typeset_kept(PROSE, "body { writing-mode: vertical-rl }", 240.0);
    let slices = fragments(
        &vt,
        &styles,
        FragmentOptions {
            height: page_width,
            columns: 1,
        },
    );
    let (_, h_styles, hz) = typeset_kept(PROSE, "", 240.0);
    let h_slices = fragments(
        &hz,
        &h_styles,
        FragmentOptions {
            height: page_width,
            columns: 1,
        },
    );
    assert!(
        slices.len() > 2,
        "一章切成一片就不叫分片了：{}",
        slices.len()
    );
    let mut next = 0usize;
    for slice in &slices {
        assert_eq!(slice.lines.start, next, "片与片之间漏了行");
        next = slice.lines.end;
        let thick: f32 = slice
            .lines
            .clone()
            .map(|i| vt.flow_thickness(&vt.lines[i]))
            .sum();
        assert!(
            thick <= page_width + 0.01,
            "这一片占了 {} 宽，超过页宽 {page_width}",
            thick
        );
    }
    assert_eq!(next, vt.lines.len(), "最后一页之后还有行没切进来");
    let origins: Vec<f32> = slices.iter().map(|s| s.origin_y).collect();
    for pair in origins.windows(2) {
        assert!(pair[1] >= pair[0], "片原点必须沿阅读顺序递增：{pair:?}");
    }
    let ranges = |v: &[Fragment]| -> Vec<(usize, usize)> {
        v.iter().map(|s| (s.lines.start, s.lines.end)).collect()
    };
    assert_eq!(ranges(&h_slices), ranges(&slices), "转轴不该改变切点");
}

/// 轻量档（只算长）与实排必须给出同一个列长与同一条长带：界面拿它做进度与跳转。
#[test]
fn light_layout_agrees_with_the_full_one_on_the_flow_extent() {
    let css = "body { writing-mode: vertical-rl }";
    let dom = Dom::parse(PROSE.as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse(css, Origin::Author),
        &ComputedStyle::default(),
    );
    let root: NodeId = find(&dom, "body");
    let opts = |light: bool| LayoutOptions {
        width: 240.0,
        light,
        allow_vertical: true,
    };
    let full = layout(&dom, &styles, root, opts(false), &Mono, &NoImages);
    let light = layout(&dom, &styles, root, opts(true), &Mono, &NoImages);
    assert_eq!(
        full.height, light.height,
        "列长那一格两档必须一致，否则滚动位置会错位"
    );
    assert_eq!(full.width, light.width, "长条总长同样要对得上");
}
