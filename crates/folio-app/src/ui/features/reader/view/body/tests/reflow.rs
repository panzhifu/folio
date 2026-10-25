//! 重排与物化：shaping 表与行单元表的账（含性能基线）。

use super::harness::{chapters_flow, flow, open, prefs, scroll_down, scroll_mode, settle};
use crate::ui::services::config::ConfigHolder;
use folio_core::Locator;
use folio_core::infra::metrics::{Metric, take_local};
use gpui_kit::TestAppContext;

/// 改字号全链重排：进度的章号不换 —— 坐标是「第几个字」，跨重排存活。
#[gpui_kit::test]
fn a_pref_change_reflows_the_whole_spine_without_losing_the_chapter(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &chapters_flow(3),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    scroll_mode(cx, handle);
    for _ in 0..40 {
        scroll_down(cx, handle, 400.0);
        settle(cx, handle);
        if view.read_with(cx, |v, _| v.chapter_index()) == 1 {
            break;
        }
    }
    assert_eq!(
        view.read_with(cx, |v, _| v.resolve_position().map(|(chapter, _)| chapter)),
        Some(1),
        "上面那一串滚动要真把人送进第二章，否则下面量的是别处"
    );
    // 刚跨进第二章那一刻，位置正是「章内第 0 个字」：换字号再怎么重排，章首那一行盒顶
    // 反解回来还是 0，于是「偏移变了才算真重排过」这一条永远证不了。补一格滚动把落点
    // 摆进章内 —— 段距一改，章首那一刻停在第几行也会跟着挪，所以这一步不能省。
    scroll_down(cx, handle, 300.0);
    settle(cx, handle);
    let (chapter, offset) =
        view.read_with(cx, |v, _| v.resolve_position().expect("重排前就该有位置"));
    assert_eq!(chapter, 1, "补的那一格滚动不该把人送出第二章");
    assert!(
        offset > 0,
        "落点要在章内而不是章首，否则下面那条「偏移变了」无从成立：实测 {offset}"
    );

    cx.update(|cx| ConfigHolder::edit_reader(cx, |prefs| prefs.font_size = 24.0));
    settle(cx, handle);
    let (after_chapter, after_offset) =
        view.read_with(cx, |v, _| v.resolve_position().expect("重排后该有位置"));
    assert_eq!(after_chapter, 1, "重排不换章");
    assert!(
        after_offset > 0 && after_offset != offset,
        "字号变了行数变了，章内偏移落在别处才是重排过的证据：{offset} → {after_offset}"
    );
}

/// 量具在记：往前滚一屏，新进入带的行要物化单元、没 shape 过的行要落到 gpui 那层。
///
/// 这条是下面两条目标门（`scrolling_back_to_the_same_band_materializes_no_new_runs` 与
/// `scrolling_back_to_the_same_band_does_not_reshape`）的**配对探测器**：它钉的是
/// 「计数器真的会动」—— 把 `Document::row_units` 或 `paint::shape` 里那句计数删掉，
/// 它当场红（三格读出来全是零），于是那两条目标门不会变成永远绿的假灯。
///
/// **两张表接上之后这一条不删**（原稿写的是「接上之后要删掉」，2026-10-15 接线时更正）：
/// 单元表是**按行懒物化**的，往前滚必然有新行第一次进带，那一趟该付；`SceneRuns` 的口径
/// 也已同步改成「这一帧新建了几条单元」，所以它量的始终是真正付过的物化次数，
/// 而不是一帧搬运了多少条。
#[gpui_kit::test]
fn a_scrolled_frame_currently_rematerializes_the_visible_runs(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (_view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    // 先滚一段把上一帧的账结掉，再把三格清零：断言的是「只滚动」这一步的增量，
    // 而不是进程总量（`take_local` 的读法，见 `infra::metrics`）。
    scroll_down(cx, handle, 240.0);
    settle(cx, handle);
    take_local(Metric::SceneRebuilds);
    take_local(Metric::SceneRuns);
    take_local(Metric::ShapeLineCalls);

    scroll_down(cx, handle, 240.0);
    settle(cx, handle);
    let rebuilds = take_local(Metric::SceneRebuilds);
    let runs = take_local(Metric::SceneRuns);
    let shapes = take_local(Metric::ShapeLineCalls);
    // 实测基线（2026-10-13，debug 构建，`flow()` 那份语料、窗宽 460、字号 16）：
    // 一次 240px 滚动走完两帧 = 5 次场景重建、62 个 run 物化、62 次 `shape_line`。
    // 5 次重建对应连续模式**每个进链的章各一份场景**（锚章 ±2）；`runs` 与 `shapes`
    // 相等，说明这一屏里每一个被拷出来的 run 同时都付了一次查表。
    // 记在这里是为了下一轮能拿同一组数比，而不是只看到「红/绿」。
    assert!(
        rebuilds > 0 && runs > 0 && shapes > 0,
        "滚动这一步应当数得出场景重建、run 物化与 shape_line 调用，实际 {rebuilds} / {runs} / {shapes}"
    );
}

/// 章那张 shaping 表接上之后的**目标断言**：滚回走过的地方，一次都不该重新 shape。
///
/// 与上面那条探测器配成一对读：它钉的是「`ShapeLineCalls` 真的会动」（往前滚一屏增量 > 0），
/// 所以这一条不会是一盏永远绿的假灯。把 `Document::shaped` 那张表拿掉、或把
/// `view::shape_runs` 那一步拿掉，这一条当场红。
///
/// 为什么「表没接上」就必然红：gpui 自己的 `LineLayoutCache` 只保两帧
/// （`previous_frame` 与 `current_frame` 在 `finish_frame` 里互换），而这里往返隔了远不止
/// 两帧 —— 邻章那些行从「排版作业 shape 完」到「进屏」也是同样的道理，所以进屏那一帧
/// 从前总要重 shape 一次，并且命中也要把整段文本再哈希一遍。
///
/// **这一条与 `SceneRuns` 那条 `#[ignore]` 的目标门是两件事**：那条钉的是 run 物化
/// （行单元表，§九），这一条钉的是 shaping。物化那一笔今天照付，所以那条门还红着。
#[gpui_kit::test]
fn scrolling_back_to_the_same_band_does_not_reshape(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (_view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    // 往前一屏：这一趟**该**付钱（新进入带的行第一次 shape），所以先把账结掉。
    // 往返只用一屏的量，是为了待在同一个锚章的 ±2 窗口里 —— 走远了整个 `Document`
    // 被裁掉，表跟着它一起没了，那一次的重新 shape 是对的，不该记在这笔账上。
    scroll_down(cx, handle, 240.0);
    settle(cx, handle);
    take_local(Metric::ShapeLineCalls);

    // 回原处（`scroll_down` 的约定是「向下滚为负」）。
    scroll_down(cx, handle, -240.0);
    settle(cx, handle);
    assert_eq!(
        take_local(Metric::ShapeLineCalls),
        0,
        "滚回走过的地方不该再 shape 一次：几何该由章那张 shaping 表原样给回"
    );
}

/// 表的**失效**：改一次字号，同样的行必须重新 shape。
///
/// 钉的是「表不能比排版活得久」。失效信号只有一处 —— `Document::drop_fragments`
/// （`set_chapter` / `set_prefs` / `set_images` / `apply_typeset` 四个都走它）；哪天有人
/// 给 shaping 表另立一套失效判断、或者漏挂在某一条上，这一条就会红。
/// 旧几何是按旧字号量的，留在表里就是「看着改了等于没改」那种看不出来的错。
#[gpui_kit::test]
fn changing_the_font_size_reshapes_the_same_lines(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (_view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    settle(cx, handle);
    take_local(Metric::ShapeLineCalls);

    cx.update(|cx| ConfigHolder::edit_reader(cx, |prefs| prefs.font_size = 24.0));
    settle(cx, handle);
    assert!(
        take_local(Metric::ShapeLineCalls) > 0,
        "改字号必须作废 shaping 表：那一屏的行要按新字号重新 shape"
    );
}

/// 目标断言（`plans/阅读管线.md` §九 + §十一 第 6 条）：**滚下去再滚回同一屏，第二趟一个
/// 新 run 都不该物化**。
///
/// 口径为什么是**往返**而不是「往前滚增量为 0」：行单元表是**懒物化**的（§九 已决：整章一份、
/// 某一行的单元只在它第一次进带时才建）。往前滚必然有新行进来，那些行第一次被画就是要拷一次
/// 文本——拿「往前滚为 0」当验收钉的是一件本来就不该发生的事。回到走过的地方才是那个确定的零。
///
/// 根子在 `GlyphRun` 的 `x` / `baseline` 存的是**相对元素原点的屏幕值**（`paint.rs:26-38`）：
/// 滚动一动，每条 run 的 `baseline` 都跟着变，场景就只能整份重造。这是「叶子只存局部值、
/// 全局值靠现算」那条规则在呈现层欠着的一笔——排版层已经照它做了（`BlockBox.y` 相对章顶、
/// 全局 y 由 ±2 章的拼接链现算），绘制层还没。
///
/// 修法的形状：「按行号索引的行单元表（值全是章内局部量）+ 每帧一个平移量」，整章一份挂在
/// `Document` 上随它进出窗口；主题色与解析好的 `Font` **都不许烘进表**（前者因为 `LayoutKey`
/// 里没有主题，后者因为 `FontCache` 已有自己的失效纪律，见 §九 那四条硬约束）。
///
/// **这一条只断 `SceneRuns`，不断 `ShapeLineCalls`**：两笔钱分开钉。shaping 那一笔
/// 2026-10-15 也接了，但接成了**另一张自己的表**（`Document::shaped`，键
/// `(行号, 片段槽位)`，值是 `Arc<LineLayout>`），配自己的门
/// [`scrolling_back_to_the_same_band_does_not_reshape`]，没有并进这张行单元表 ——
/// 因为 gpui 的 `ShapedLine.layout` 是 `pub(crate)`（外面拿不出那份 `Arc`），
/// 而它自带的 `decoration_runs` 里有主题色。
///
/// **2026-10-15 已接线，`#[ignore]` 摘掉了**（原来那句「接线之后去掉这一句就成了守门员」
/// 走的就是 `epub::tests` 那条「一百次取图只开一次容器」走过的路）：行单元表在
/// `Document::row_units`（挂在 `Document` 上随章进出窗口，失效只跟 `drop_fragments`
/// 那一处信号），`Metric::SceneRuns` 的口径同步从「每帧摆几条 run」改成「这一帧新建几条
/// 单元」——不改口径这一格就会在缓存生效时照样涨，那条确定的零就永远量不出来。
/// 上面那条探测器（`a_scrolled_frame_currently_rematerializes_the_visible_runs`）现在
/// 反过来才是它的对头：往前滚那一趟仍然该付，所以它照旧断三格都 > 0。
#[gpui_kit::test]
fn scrolling_back_to_the_same_band_materializes_no_new_runs(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let flow = flow();
    let (_view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);

    // 往前一屏：这一趟**该**付钱（新行第一次进带），所以先把账结掉。
    scroll_down(cx, handle, 240.0);
    settle(cx, handle);
    take_local(Metric::SceneRuns);

    // 回原处：`scroll_down` 的约定是「向下滚为负」，给它一个负数就是向上滚。往返只用一屏的量，
    // 是为了待在同一个锚章的 ±2 窗口里——走远了整个 `Document` 被裁掉，那一次的重新物化是对的，
    // 不该记在「还没接」这笔账上。
    scroll_down(cx, handle, -240.0);
    settle(cx, handle);
    assert_eq!(
        take_local(Metric::SceneRuns),
        0,
        "走过的地方再走回来，只该改帧内那个平移量，不该物化任何一段新文本"
    );
}
