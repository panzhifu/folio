//! 一本真实的书里，我们的 CSS 子集还差多少 —— 按「影响到几个元素」排名。
//!
//! 跑法（无界面，纯引擎，不需要窗口也不需要 GPU）：
//! ```text
//! cargo run -p folio-core --example css-gap                       # 用仓库里的两本 fixture
//! cargo run -p folio-core --example css-gap -- path/to/other.epub # 也可以指别的书
//! ```
//!
//! 为什么数**元素**而不是数声明：一本书里 `border-bottom` 写三十次但只落在三个小元素上，
//! 和 `text-align` 落在全书两千个段落上，是两种完全不同量级的落差。声明数只说明作者
//! 写了多少，元素数才说明读者看得见多少。
//!
//! 判定用的是层叠那一份逻辑（`css::computed::declaration_status`，内部就是 `apply`），
//! 所以这张表和实际渲染行为不会分叉。统计口径：
//! - **每一章的规则表 = 清单里那几份样式表 + 这一章自己写的 `<style>`**，与排版那一趟
//!   逐字同一条（`folio-app` 的 `typeset_job` 也是 `clone` 书级表再 `extend`
//!   `css::chapter_style_rules`）。从前这里只喂 `Document::stylesheet()` 那一份，于是
//!   写在章里的样式整块不进这张表：《斗战狂潮》封面页那句 `position: relative` 明明
//!   在屏上没生效，表上却报「全部接住」—— 量具读的样式集与渲染用的不是同一份，
//!   这张表说的就只是「清单里那几份接住了」，而不是「这本书接住了」。
//! - 每个元素取**层叠胜出**的那条声明（`matching_declarations` 已按优先级从低到高排好，
//!   同一属性取最后一条），所以「写了但被更高 specificity 覆盖」的声明不算落差；
//! - `Inert` = 属性名认得但一期不生效（`KNOWN_UNIMPLEMENTED`）；
//! - `Unparsed` = 属性名或值不认识，会进 `StyledDom.ignored`；
//! - 样式表里连规则都解析不出来的部分（选择器不认识、`@import` 之类）单独报一行，
//!   它落在 `RuleSet.ignored` 里，不在上面两栏；章内那块 `<style>` 拼不出来的同样
//!   计进去（两边都算才是这本书的真实数）。
//!
//! 栏宽、分栏这类**视口**参数与作者的块级多栏是两件事，这里只统计声明本身。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use folio_core::content::document::Document;
use folio_core::content::dom::Dom;
use folio_core::content::format::FlowDoc;
use folio_core::content::format::css::computed::{
    ComputedStyle, DeclStatus, cascade, declaration_status, inline_declarations,
};
use folio_core::content::format::css::{Origin, RuleSet, chapter_style_rules};

/// 一个属性的落地面。`elements` 是**这个属性上有任何一条胜出且没接住的声明**的元素数。
#[derive(Debug, Default)]
struct Gap {
    /// 胜出且未接住的声明数（≈ 元素数，同一元素同属性只算一次）。
    decls: usize,
    elements: usize,
    inert: usize,
    unparsed: usize,
    /// 书里实际写成什么样的例子，最多留两条 —— 决定「值语法要不要一起支持」要看这个。
    examples: Vec<String>,
}

fn main() {
    let books = books();
    if books.is_empty() {
        eprintln!(
            "没有可统计的书。仓库里的两本 fixture 都不在（tests/fixtures/ 里一份本地书、\n\
             一份最小中文样本），把它们放回原处或直接传路径。"
        );
        return;
    }
    for path in &books {
        match survey(path) {
            Ok(text) => println!("{text}"),
            Err(e) => eprintln!("{}：读不进去（{e}）", path.display()),
        }
    }
}

/// 命令行给的路径优先，否则用仓库里的两本 fixture。不存在的一律跳过而不是报错 ——
/// `tests/fixtures/local/` 在 `.gitignore` 里，克隆的人手上可能只有一本。
fn books() -> Vec<PathBuf> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        return args.into_iter().map(PathBuf::from).collect();
    }
    [
        "tests/fixtures/local/shejie-mauriac.epub",
        "tests/fixtures/oss/minimal-zh.epub",
    ]
    .into_iter()
    .map(PathBuf::from)
    .filter(|p| p.is_file())
    .collect()
}

fn survey(path: &Path) -> Result<String, String> {
    let doc = folio_core::content::document::open(path).map_err(|e| format!("{e:?}"))?;
    let meta = doc.meta();
    // 这个度量工具只对可重排有意义：定版式没有 CSS 可谈。
    let Some(flow) = doc.flow() else {
        return Err(format!("{}：定版式文档，没有 CSS 落差可量", path.display()));
    };
    let mut out = String::new();
    // 字数可能算不出来（正文不在手上、或 provider 那一趟没读）—— 写成「未算」
    // 而不是 0，免得报表读者以为这本书是空的。
    let chars = flow
        .char_len()
        .map_or_else(|| "未算".to_string(), |n| n.to_string());
    out.push_str(&format!(
        "## {} —— {}（{} 章、{} 字）\n\n",
        path.display(),
        if meta.title.is_empty() {
            "（无书名）"
        } else {
            &meta.title
        },
        flow.chapters.len(),
        chars,
    ));
    // CSS 与正文都走 `Document` 的现取入口，而不是 `flow.css` / `chapter.source()`：
    // 索引路径（懒加载）下 `FlowDoc` 里只有位置清单、正文与样式表都不在手上，
    // 这两处才按清单回容器取。工具若读 `flow.css`，对正经导入的书一律是空串。
    let css = doc.stylesheet();
    let rules = RuleSet::parse(&css, Origin::Author);
    let stats = measure(doc.as_ref(), flow, &rules);
    // 「没东西可接」有两种：清单里一份样式表都没有，和样式表写了但没落到任何元素上。
    // 两种都不该被读成「全部接住」，所以判据放到数完之后 —— 章内 `<style>` 也算作者
    // 样式，只按 `css` 那一份判会把第二种漏掉。
    if stats.declarations == 0 {
        out.push_str(
            "**这本书没有落到任何元素上的作者声明**（清单里那几份样式表与章内 `<style>` \
             都算上），落差为零不是因为接住了，是因为没东西可接。\n",
        );
        return Ok(out);
    }

    let mut rows: Vec<(&String, &Gap)> = stats.gaps.iter().collect();
    // 先按影响的元素数，再按声明数；完全同名时按属性名字典序，保证输出可复现。
    rows.sort_by(|(a_name, a), (b_name, b)| {
        b.elements
            .cmp(&a.elements)
            .then(b.decls.cmp(&a.decls))
            .then(a_name.cmp(b_name))
    });

    out.push_str(&format!(
        "声明总数 {} 条；元素 {} 个，其中 **{} 个**（{:.1}%）身上至少有一条胜出却没接住的声明。\n\
         按状态分：接住 {}、认得但不生效 {}、不认识 {}。样式表与章内 `<style>` 里连规则都拼不出来的块：{} 处。\n\n",
        stats.declarations,
        stats.elements,
        stats.elements_hit,
        if stats.elements == 0 {
            0.0
        } else {
            100.0 * stats.elements_hit as f64 / stats.elements as f64
        },
        stats.applied,
        stats.inert,
        stats.unparsed,
        stats.ignored,
    ));
    out.push_str("| 属性 | 受影响元素 | 胜出声明 | 状态 | 书里的写法 |\n|---|---|---|---|---|\n");
    for (name, gap) in &rows {
        let status = match (gap.inert, gap.unparsed) {
            (_, 0) => "认得，不生效",
            (0, _) => "不认识",
            _ => "两者都有",
        };
        out.push_str(&format!(
            "| `{name}` | {} | {} | {status} | {} |\n",
            gap.elements,
            gap.decls,
            gap.examples.join(" ／ ")
        ));
    }
    if rows.is_empty() {
        out.push_str("| — | 0 | 0 | — | 全部接住 |\n");
    }
    out.push_str(&impact(doc.as_ref(), flow, &rules));
    Ok(out)
}

#[derive(Debug, Default)]
struct Totals {
    gaps: BTreeMap<String, Gap>,
    declarations: usize,
    applied: usize,
    inert: usize,
    unparsed: usize,
    elements: usize,
    elements_hit: usize,
    /// 连规则都拼不出来的块：清单里那几份算一次，每一章自己那块 `<style>` 各算一次。
    ignored: usize,
}

/// 逐章逐元素地数。清单里那几份样式表只解析一次、全书共用，但**每一章**还要并入它自己
/// 写的 `<style>` —— 排版那一趟就是这么拼的（`folio-app` 的 `typeset_job`：`clone` 书级
/// 表再 `extend` `css::chapter_style_rules`）。量具少并这一刀，报的就只是「清单里那几份
/// 接住了」，写在章里的样式整个不在秤上，表尾那句「全部接住」就成了假话。
fn measure(doc: &dyn Document, flow: &FlowDoc, rules: &RuleSet) -> Totals {
    let mut tot = Totals {
        ignored: rules.ignored,
        ..Default::default()
    };
    let base = ComputedStyle::default();
    let root_px = base.font_size;
    for (index, chapter) in flow.chapters.iter().enumerate() {
        let Ok(source) = doc.chapter_source(index) else {
            continue;
        };
        let dom = Dom::parse(source.as_bytes());
        // 内联那一份的基准目录：与界面递给 `cascade` 的同一个数（`ChapterImages` 手里那一个）。
        let base_dir = folio_core::content::format::dir_of(&chapter.spine_href);
        let own = chapter_style_rules(&dom, &base_dir);
        tot.ignored += own.ignored;
        let mut used = rules.clone();
        used.extend(own);
        let styled = cascade(&dom, &used, &base, &base_dir);
        for (id, _) in dom.elements() {
            tot.elements += 1;
            let Some(own) = styled.get(id) else {
                continue;
            };
            // `matching_declarations` 已经按层叠优先级从低到高排好，
            // 所以同一属性留最后一条就是胜出的那条。
            let mut winner: BTreeMap<&str, &folio_core::content::format::css::Declaration> =
                BTreeMap::new();
            // 元素上的 `style="…"` 也是一条声明。**这一条从前整个漏了**：这台秤只遍历样式表
            // 规则，于是哈利波特的 21 处内联整页底图（七张册扉页、五张附录扉页、`toc3`、
            // `authorhis` 那一族）一条也没记，表上只剩 `.oval` 那一处样式表写法，
            // 读表的人会以为这条属性已经见底 —— 量具漏计一个形状，比引擎少接一个属性更坏。
            // 次序照抄 `cascade` 那一刀：作者普通 < 内联普通 < 内联 `!important` < 作者 `!important`。
            let decls = used.matching_declarations(&dom, id);
            let inline = dom
                .attr(id, "style")
                .map(|text| inline_declarations(text, &base_dir))
                .unwrap_or_default();
            for d in decls.iter().filter(|d| !d.important) {
                winner.insert(d.property.as_str(), *d);
            }
            for d in inline.iter().filter(|d| !d.important) {
                winner.insert(d.property.as_str(), d);
            }
            for d in inline.iter().filter(|d| d.important) {
                winner.insert(d.property.as_str(), d);
            }
            for d in decls.iter().filter(|d| d.important) {
                winner.insert(d.property.as_str(), *d);
            }
            let mut hit = false;
            for (name, decl) in winner {
                tot.declarations += 1;
                let status = declaration_status(decl, own, root_px);
                match status {
                    DeclStatus::Applied => tot.applied += 1,
                    DeclStatus::Inert => {
                        tot.inert += 1;
                        hit = true;
                    }
                    DeclStatus::Unparsed => {
                        tot.unparsed += 1;
                        hit = true;
                    }
                }
                if status == DeclStatus::Applied {
                    continue;
                }
                let gap = tot.gaps.entry(name.to_string()).or_default();
                gap.decls += 1;
                gap.elements += 1;
                match status {
                    DeclStatus::Inert => gap.inert += 1,
                    DeclStatus::Unparsed => gap.unparsed += 1,
                    DeclStatus::Applied => {}
                }
                let text = format!("{}: {}", decl.property, decl.value);
                if gap.examples.len() < 2 && !gap.examples.contains(&text) {
                    gap.examples.push(text);
                }
            }
            if hit {
                tot.elements_hit += 1;
            }
        }
    }
    // 不同章的 DOM 各自独立，所以「元素数」是全书累加而不是去重后的集合 ——
    // 章节本来就是彼此独立的文档，跨章不存在同一个元素。
    tot
}

// ---- 排版影响：同一章、同一个假度量，接住与没接住差多少 ----
//
// 「丢掉所有没接住的声明」正是这些属性**没做之前**的行为（`apply` 认不出来就是整条作废，
// 不做半套），所以这两份排版的差就是这次改动的真实视觉差。度量用与 `layout::tests`
// 同一套「汉字 1 em、拉丁 0.5 em、空格 0.25 em」的假度量 —— 不是真字体，但两边同源，
// 差值可比。要的是量级，不是像素级精确。

/// 与测试里同一份 mock 度量。放在这里而不是核心里：只有这个工具有需要。
struct Mock;

impl folio_core::layout::Measure for Mock {
    fn char_width(&self, ch: char, _font: &folio_core::layout::FontKey, font_size: f32) -> f32 {
        if ch == ' ' {
            font_size * 0.25
        } else if (ch as u32) >= 0x2e80 {
            font_size
        } else {
            font_size * 0.5
        }
    }

    fn run_metrics(
        &self,
        text: &str,
        _font: &folio_core::layout::FontKey,
        font_size: f32,
    ) -> folio_core::layout::LineMetrics {
        folio_core::layout::LineMetrics {
            width: text
                .chars()
                .map(|ch| self.char_width(ch, &folio_core::layout::FontKey::default(), font_size))
                .sum(),
            ascent: font_size * 0.8,
            descent: font_size * 0.2,
        }
    }
}

/// 只留下 `Applied` 的声明，等价于「这些属性还没做」时的那张样式表。
fn without_unsupported(rules: &RuleSet) -> RuleSet {
    let base = Default::default();
    let mut kept = Vec::new();
    for rule in &rules.rules {
        let declarations: Vec<_> = rule
            .declarations
            .iter()
            .filter(|decl| {
                matches!(
                    declaration_status(decl, &base, base.font_size),
                    DeclStatus::Applied
                )
            })
            .map(|decl| {
                // 「这些属性还没做时」那一臂 = 把**已接住但曾经空转**的声明折回它的旧行为。
                // `text-align: justify` 就是这一格：`declaration_status` 一直认它 `Applied`，
                // 于是两臂都留着它、差值是 0 —— 而接通前它落 `_ => 0.0`，与左对齐逐像素同解。
                // 折回 `start` 之后，这张表的「排版影响」才看得见两端对齐那笔摊开。
                let mut d = decl.clone();
                if d.property == "text-align" && d.value.trim().eq_ignore_ascii_case("justify") {
                    d.value = "start".into();
                }
                d
            })
            .collect();
        if !declarations.is_empty() {
            kept.push(folio_core::content::format::css::Rule {
                selector: rule.selector.clone(),
                declarations,
                origin: rule.origin,
                order: rule.order,
            });
        }
    }
    RuleSet {
        rules: kept,
        // 内嵌字体清单不参与这份对比：它不落在任何元素上，「丢掉没接住的声明」这一步
        // 与它无关，照原样带过去只会让两份数据的字体数看着有出入。
        font_faces: Vec::new(),
        ignored: rules.ignored,
    }
}

/// 把**全书每一章**都排一遍，返回
/// （行盒总数，正文总高之和，行盒左缘的平均值，行盒墨迹宽的平均值）。
///
/// `strip` = 把没接住的声明整条丢掉再排，等价于「这些属性还没做时」那一臂。
///
/// 为什么累加而不是只取第一章：EPUB 的第一章常常是封面页或扉页（只有图，甚至没有文本），
/// 那种页量出来的「差」是 0，看不出任何事。全书累加才是这本书的排版差。
///
/// **最后一格（墨迹宽）是两端对齐唯一看得见的地方**：它不改行数、不改高、不改左缘
/// （`Justify` 的偏移是 0），只把字在行内摊开 —— 所以量 `ink_width`（撑开的墨迹宽）
/// 才量得到它。没有这一格，`text-align: justify` 在这张表里永远是「差 0」。
fn geometry(
    doc: &dyn Document,
    flow: &FlowDoc,
    rules: &RuleSet,
    strip: bool,
) -> (usize, f32, f32, f32) {
    use folio_core::content::dom::Dom;
    use folio_core::content::format::css::computed::ComputedStyle;
    use folio_core::layout::{LayoutOptions, layout};
    let mut lines = 0usize;
    let mut height = 0.0f32;
    let mut left_sum = 0.0f32;
    let mut ink_sum = 0.0f32;
    for (index, chapter) in flow.chapters.iter().enumerate() {
        let Ok(source) = doc.chapter_source(index) else {
            continue;
        };
        let dom = Dom::parse(source.as_bytes());
        // 基准目录与 `measure`、与界面递给 `cascade` 的那一个同数：章内那块 `<style>`
        // 里的 `url(...)` 按这一章的目录折，递空串就等于没折。
        let base_dir = folio_core::content::format::dir_of(&chapter.spine_href);
        let mut used = rules.clone();
        used.extend(chapter_style_rules(&dom, &base_dir));
        // 「这些属性还没做」那一臂要把没接住的声明从**并好章内样式的那一份**里丢，
        // 只丢书级那一份的话，章内写的落差声明会两臂都留着，排版差量就成了 0。
        let used = if strip {
            without_unsupported(&used)
        } else {
            used
        };
        let styles = cascade(&dom, &used, &ComputedStyle::default(), &base_dir);
        // 没有 `<body>` 的章（手写得很野的老书）从文档根节点排，行为与 app 侧一致。
        let body = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map(|(id, _)| id)
            .unwrap_or(dom.document);
        let laid = layout(
            &dom,
            &styles,
            body,
            LayoutOptions {
                width: 600.0,
                light: false,
                allow_vertical: false,
                page_height: None,
            },
            &Mock,
            &folio_core::layout::NoImages,
        );
        lines += laid.lines.len();
        for line in &laid.lines {
            left_sum += line.x;
            ink_sum += line.ink_width();
        }
        // 每章的正文高就是它最后一行的行底，然后**跨章累加** ——
        // 取 max 只会量出最长那一章，说不了整本书的事。
        height += laid
            .lines
            .last()
            .map(|line| line.y + line.height)
            .unwrap_or(0.0);
    }
    let per_line = |sum: f32| if lines == 0 { 0.0 } else { sum / lines as f32 };
    (lines, height, per_line(left_sum), per_line(ink_sum))
}

/// 把两份排版的差打成一行字，附在统计表后面。
fn impact(doc: &dyn Document, flow: &FlowDoc, rules: &RuleSet) -> String {
    let (lines_now, height_now, left_now, ink_now) = geometry(doc, flow, rules, false);
    let (lines_then, height_then, left_then, ink_then) = geometry(doc, flow, rules, true);
    let page = 900.0;
    let pages_now = (height_now / page).ceil() as usize;
    let pages_then = (height_then / page).ceil() as usize;
    let percent = if height_then > 0.0 {
        100.0 * (height_now - height_then) / height_then
    } else {
        0.0
    };
    format!(
        "\n### 排版影响（全书 {} 章、600px 版心、假度量）\n\
         没接住的声明整条丢掉（= 这些属性还没做时）：{lines_then} 行、正文高 {height_then:.0}px、\
         平均行左缘 {left_then:.1}px、平均行墨迹宽 {ink_then:.1}px、按 900px 页高 {pages_then} 页\n\
         现在接住了：{lines_now} 行、正文高 {height_now:.0}px、平均行左缘 {left_now:.1}px、\
         平均行墨迹宽 {ink_now:.1}px、{pages_now} 页\n\
         差：{} 行、{:+.0}px（{:+.2}%）、平均左缘 {:+.1}px、平均墨迹宽 {:+.1}px\n\
         口径：正文高 = 每章最后一行的行底跨章累加；水平落差（`margin-left`、缩进）不进那个数，\n\
         所以要看「平均行左缘」与「平均行墨迹宽」这两列 —— 只报正文高会把左右边距与两端对齐\n\
         的落差整个漏掉（`text-align: justify` 不改行数/高/左缘，只在行内摊开，只有墨迹宽量得到）。\n",
        flow.chapters.len(),
        lines_now as i64 - lines_then as i64,
        height_now - height_then,
        percent,
        left_now - left_then,
        ink_now - ink_then,
    )
}
