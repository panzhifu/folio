//! 批注的读侧：把库里的锚点落回**当前这一章**的可见文本坐标，再把它们导出成人看得懂的文本。
//!
//! 与存储层的分工：`store::Annotation` 管「存了什么」（坐标 + 指纹 + 笔记 + 颜色），
//! 本模块管「现在落在哪儿」。这件事必须在读的时候重算而不是把坐标写死 ——
//! 库里的坐标是**重定位的依据**，排版、改字号、换版本都不该动它。
//!
//! 为什么放 core 而不是界面：`relocate` 的四级降级是纯函数，分组与导出更是纯文本变换，
//! 三者都能在没有窗口、没有 GPU 的情况下逐条钉死。界面只拿 `start..end` 去
//! [`crate::content::hit::rects_for_range`] 画底色。

use std::collections::BTreeMap;

use crate::content::anchor::{Confidence, TextMap, relocate};
use crate::content::format::{Chapter, DocMeta};
use crate::store::Annotation;

/// 一条锚点落到本章坐标上的结果。界面拿它画底色、列侧栏、标 stale。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAnnotation {
    /// 库里那一条的 id：界面靠它回指原文，写回 stale 时也用它。
    pub id: i64,
    pub start: usize,
    pub end: usize,
    /// 在哪一级找回的。`Stale` 表示没找回，界面要显式标出来而不是画到别处。
    pub confidence: Confidence,
    pub color: String,
    pub note: String,
}

impl ResolvedAnnotation {
    /// 锚点没找回来。等价于 `confidence == Stale`，但读起来就是界面要说的那句话。
    pub fn is_stale(&self) -> bool {
        self.confidence == Confidence::Stale
    }
}

/// 把属于 `chapter` 的批注解到这一章的坐标上，别的章自动滤掉。
///
/// 解不回来的那几条照样返回（`is_stale()` 为真）：界面要能列出「这条找不到了」，
/// 而不是让它从列表里凭空消失 —— 那读者会以为自己的批注被吞了。
/// 库里那条 `stale` 是上次的结论，本次结果以这里的 `confidence` 为准，
/// 由调用方决定要不要回写 [`crate::Store::set_annotation_stale`]。
pub fn resolve_annotations(
    annotations: &[Annotation],
    chapter: u32,
    map: &TextMap,
) -> Vec<ResolvedAnnotation> {
    annotations
        .iter()
        .filter(|a| a.chapter == chapter)
        .map(|a| {
            let at = relocate(&a.anchor, map);
            ResolvedAnnotation {
                id: a.id,
                start: at.start,
                end: at.end,
                confidence: at.confidence,
                color: a.color.clone(),
                note: a.note.clone(),
            }
        })
        .collect()
}

/// 按章分组，章序升序；同一章内保持传入顺序（库按创建时间给的）。
///
/// 回显、批注侧栏、Markdown 导出要的是同一份分组，所以它属于 core 而不是各写一遍。
/// 用 `BTreeMap` 而不是先 `sort` 一遍 `Vec`：章号天然有序键，分组与排序一步到位。
pub fn annotations_by_chapter(annotations: &[Annotation]) -> Vec<(u32, Vec<&Annotation>)> {
    let mut groups: BTreeMap<u32, Vec<&Annotation>> = BTreeMap::new();
    for a in annotations {
        groups.entry(a.chapter).or_default().push(a);
    }
    groups.into_iter().collect()
}

/// 批注导出成 Markdown。纯函数，不碰磁盘 —— 写哪个文件是界面的事。
///
/// 正文用锚点里存的 `exact` 原文，而不是按坐标回切片：导出的时机往往是没有排版、
/// 甚至书已经不在这台机器上的时候，而 `exact` 就在库里那一条里。
///
/// **stale 的要显式标出来**。导出是拿去分享或备份的，把一条「位置已失效」的批注
/// 混在正常批注里，收到的人会以为那段原文真在书上。
pub fn annotations_markdown(
    meta: &DocMeta,
    chapters: &[Chapter],
    annotations: &[Annotation],
) -> String {
    let title = meta.title.trim();
    let title = if title.is_empty() { "未命名" } else { title };
    let mut out = format!("# 《{title}》批注\n\n");
    if !meta.authors.is_empty() {
        out.push_str(&format!("作者：{}\n\n", meta.authors.join("、")));
    }
    if annotations.is_empty() {
        out.push_str("还没有批注。\n");
        return out;
    }
    out.push_str(&format!("共 {} 条批注。\n", annotations.len()));

    for (chapter, group) in annotations_by_chapter(annotations) {
        let heading = chapters
            .get(chapter as usize)
            .map(|c| c.title.trim())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("第 {} 章", chapter + 1));
        out.push_str(&format!("\n## {}. {heading}\n", chapter + 1));

        for a in group {
            out.push('\n');
            let quote = collapse(&a.anchor.exact);
            if quote.is_empty() {
                // 只有坐标、没有选字的那些（比如书签式的点锚点）。
                out.push_str("> （只有位置，没有选中的文字）\n");
            } else {
                out.push_str(&format!("> {quote}\n"));
            }
            if a.stale {
                out.push_str(">\n> ⚠️ 位置已失效，导出的原文来自记录时的快照。\n");
            }
            let note = a.note.trim();
            if !note.is_empty() {
                out.push('\n');
                out.push_str(note);
                out.push('\n');
            }
        }
    }
    out
}

/// 把任意空白（含换行）折成一个空格。`exact` 是可见文本，可能带着排版换行，
/// 直接塞进 `> ` 那一行会把引用块截断。
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space && !out.is_empty() {
            out.push(' ');
        }
        pending_space = false;
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    //! 批注读侧：重定位、按章分组、Markdown 导出。只依赖公开 API，
    //! 排版一律用真 `Dom` + `TextMap` 建，不假造坐标表。

    use crate::content::anchor::{Confidence, TextAnchor, TextMap};
    use crate::content::annotation::{annotations_by_chapter, annotations_markdown, resolve_annotations};
    use crate::content::dom::Dom;
    use crate::content::format::{Chapter, DocMeta, Format};
    use crate::store::Annotation;

    /// 一段正文的可见文本坐标表。`TextMap` 自己持有文本，不借 `Dom`。
    fn map_of(html: &str) -> TextMap {
        let dom = Dom::parse(html.as_bytes());
        let root = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map(|(id, _)| id)
            .unwrap_or(dom.document);
        TextMap::build(&dom, root)
    }

    fn annotation(
        id: i64,
        chapter: u32,
        anchor: TextAnchor,
        note: &str,
        stale: bool,
    ) -> Annotation {
        Annotation {
            id,
            book_id: 1,
            chapter,
            anchor,
            note: note.into(),
            color: "amber".into(),
            stale,
            created_at: id,
            updated_at: id,
        }
    }

    fn meta(title: &str) -> DocMeta {
        DocMeta {
            title: title.into(),
            authors: vec!["作者甲".into(), "作者乙".into()],
            format: Format::Epub,
            size: 1,
        }
    }

    fn chapter(title: &str) -> Chapter {
        Chapter::inline(title, "")
    }

    #[test]
    fn an_intact_anchor_resolves_exactly_and_other_chapters_are_filtered_out() {
        let map = map_of("<body><p>甲乙丙丁戊己庚辛</p></body>");
        let mine = annotation(1, 0, TextAnchor::new(&map, 0, 1, 3), "好句", false);
        let other = annotation(2, 5, TextAnchor::new(&map, 5, 0, 1), "别人的章", false);

        let got = resolve_annotations(&[mine, other], 0, &map);
        assert_eq!(got.len(), 1, "只解本章");
        assert_eq!((got[0].start, got[0].end), (1, 3));
        assert_eq!(got[0].confidence, Confidence::Exact);
        assert_eq!(got[0].color, "amber");
        assert_eq!(got[0].note, "好句");
        assert!(!got[0].is_stale());
    }

    #[test]
    fn an_anchor_survives_the_text_shifting_around_it() {
        let before = map_of("<body><p>开头甲乙丙丁结尾</p></body>");
        let anchor = TextAnchor::new(&before, 0, 2, 6);
        assert_eq!(anchor.exact, "甲乙丙丁");

        // 前面插了一段：坐标变了，指纹还在。
        let after = map_of("<body><p>新插进来的一段话。开头甲乙丙丁结尾</p></body>");
        let got = resolve_annotations(&[annotation(1, 0, anchor, "", false)], 0, &after);
        assert_eq!(got[0].start, 11, "应跟着指纹挪到新位置");
        assert!(!got[0].is_stale());
    }

    /// 找不到就显式 stale，绝不猜一个位置 —— 界面要能看到「这条失效了」。
    #[test]
    fn a_lost_anchor_is_reported_stale_not_guessed() {
        let map = map_of("<body><p>甲乙丙丁</p></body>");
        let anchor = TextAnchor {
            chapter: 0,
            start: 0,
            end: 4,
            exact: "这段话不在书里".into(),
            prefix: "xyzzy".into(),
            suffix: "plugh".into(),
            path: String::new(),
        };
        let got = resolve_annotations(&[annotation(7, 0, anchor, "", true)], 0, &map);
        assert!(got[0].is_stale());
        assert_eq!(got[0].id, 7, "stale 那条也要带着 id 回来，界面才能回写");
    }

    #[test]
    fn grouping_orders_by_chapter_and_keeps_insertion_order_inside() {
        let map = map_of("<body><p>甲乙丙丁</p></body>");
        let a = annotation(1, 2, TextAnchor::new(&map, 2, 0, 1), "", false);
        let b = annotation(2, 0, TextAnchor::new(&map, 0, 0, 1), "", false);
        let c = annotation(3, 2, TextAnchor::new(&map, 2, 1, 2), "", false);

        let list = [a, b, c];
        let grouped = annotations_by_chapter(&list);
        let shape: Vec<(u32, Vec<i64>)> = grouped
            .iter()
            .map(|(ch, group)| (*ch, group.iter().map(|a| a.id).collect()))
            .collect();
        assert_eq!(shape, vec![(0, vec![2]), (2, vec![1, 3])]);
    }

    #[test]
    fn markdown_has_a_header_chapters_quotes_and_notes() {
        let map = map_of("<body><p>甲乙丙丁戊己</p></body>");
        // 换行要折掉（`exact` 可能带排版换行）。
        let mut two_line = TextAnchor::new(&map, 0, 0, 2);
        two_line.exact = "甲乙\n丙丁".into();
        let first = annotation(1, 0, two_line, "第一段的笔记", false);
        let second = annotation(2, 1, TextAnchor::new(&map, 1, 2, 4), "", true);

        let md = annotations_markdown(
            &meta("蛇结"),
            &[chapter("第一章"), chapter("第二章")],
            &[first, second],
        );
        assert!(md.starts_with("# 《蛇结》批注\n"), "{md}");
        assert!(md.contains("作者：作者甲、作者乙"), "{md}");
        assert!(md.contains("共 2 条批注。"), "{md}");
        assert!(md.contains("## 1. 第一章"), "{md}");
        assert!(md.contains("> 甲乙 丙丁"), "换行要折成空格：{md}");
        assert!(md.contains("第一段的笔记"), "{md}");
        assert!(md.contains("## 2. 第二章"), "{md}");
        assert!(
            md.contains("位置已失效"),
            "stale 必须标出来，不然收到的人以为原文真在书上：{md}"
        );
    }

    #[test]
    fn markdown_falls_back_when_a_chapter_title_is_missing() {
        let map = map_of("<body><p>甲乙</p></body>");
        let a = annotation(1, 3, TextAnchor::new(&map, 3, 0, 1), "", false);
        let md = annotations_markdown(&meta("无题"), &[], &[a]);
        assert!(md.contains("## 4. 第 4 章"), "{md}");
    }

    #[test]
    fn markdown_of_a_book_without_annotations_says_so() {
        let md = annotations_markdown(&meta("空书"), &[chapter("一")], &[]);
        assert!(md.contains("还没有批注"));
        assert!(!md.contains("## 1."), "没有批注就不该有章标题：{md}");
    }

    /// 没有选字的点锚点（书签形状）也要导得出来，只是没有引文。
    #[test]
    fn a_point_anchor_exports_without_a_quote() {
        let map = map_of("<body><p>甲乙</p></body>");
        let a = annotation(1, 0, TextAnchor::point(0, 1), "这里记一笔", false);
        let md = annotations_markdown(&meta("书"), &[chapter("一")], &[a]);
        assert!(md.contains("只有位置，没有选中的文字"), "{md}");
        assert!(md.contains("这里记一笔"), "{md}");
        let _ = map;
    }
}
