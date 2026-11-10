//! `.txt` 小说的分章规则：出厂那十几条模式、读者自己加的、以及四条否决门的阈值。
//!
//! 这一屏服务的是一句具体的话：「我手里这本书的写法它认不出来」。出厂那张表是简中的十几条
//! （第X章／回／篇／幕、编号、楔子番外那一族），而网文与英译本可能写 `Chapter 1`、`【01】`、
//! `序·一` —— 那些写法不在表里，整本就是一整篇一章。
//!
//! ## 改完要重启才对，这一屏不装样子
//!
//! 生效时机写在 [`ConfigHolder::edit_novel`] 与 `novel::rules::install` 的文档上，理由不是
//! 偷懒：分章跑在导入与重扫的工作线程里，一本书要过几十几百个文件，中途换表会切出
//! 「前半本按旧表、后半本按新表」的章表，而盖在索引上的 `rules_hash` 只有一份、判失效时它
//! 对得上 —— 于是没有任何东西回来修它。所以这一屏最后一组把「重启之后」念在明面上，
//! 而不是让读者以为按下开关就换了一套章。
//!
//! ## 摆的顺序
//!
//! 出厂那一组**行数恒定**（关掉的也留在列里，否则没法再打开）；阈值一组也恒定；自定义那一组
//! 排最后并只往**尾上加** —— 两端的输入框状态按（页、组、行）键控，插在前面会把正打字那一格
//! 错位到另一条规则上（与 `translation.rs` 那一条同一份顾虑）。

use folio_core::content::format::novel::rules::{self, Edits, MatchKind, NumberingStyle};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{Sizable as _, h_flex};
use gpui_kit::{IntoElement as _, Styled as _};

use super::super::*;

pub(in crate::ui::features::settings) fn txt_rules_page(cx: &App) -> Page {
    let edits = ConfigHolder::novel(cx).edits;
    page(
        tr(cx, "page.txt_rules"),
        IconName::FileText,
        vec![
            shipped_group(cx, &edits),
            gate_group(cx, &edits),
            table_group(cx),
            custom_group(cx, &edits),
        ],
    )
}

/// 出厂那十几条：一行一条，标题就是它自己的名字（数据里的中文，不是界面文案）。
///
/// 关掉的条目**仍然摆在这一列里**，只是开关在关的那头 —— 列里没了它，读者就没法再把它打开。
/// 说明那一格（只有桌面渲染）写的是它到底匹配什么，因为「章」这一个字答不了「它认哪些行」。
fn shipped_group(cx: &App, edits: &Edits) -> Group {
    let rows = rules::rows(edits)
        .into_iter()
        .filter(|row| !row.custom)
        .map(|row| {
            let name = row.name.clone();
            let (get_name, set_name) = (name.clone(), name.clone());
            let summary = if row.kind == MatchKind::Regex {
                row.pattern.clone()
            } else {
                row.text.clone()
            };
            Row::toggle(
                SharedString::from(name),
                move |cx: &App| marker_on(&ConfigHolder::novel(cx).edits, &get_name),
                move |on, cx| set_marker(cx, &set_name, on),
            )
            .description(SharedString::from(summary))
            .keywords(&["txt", "rule", "chapter", "分章", "规则"])
        })
        .collect();
    group(tr(cx, "group.rules_shipped"), rows)
}

/// 四条否决门加标题上限，共五个数。每一格的范围都在规格里，两端的夹取才不各说一套。
///
/// 写回的时候**等于出厂那个数就当没改过**（存 `None`）：差量里留一条与出厂相同的值没有
/// 意义，却会让以后出厂调那一格时被这份旧数盖回去。
fn gate_group(cx: &App, edits: &Edits) -> Group {
    let base = rules::builtin_table().gates.clone();
    let current = rules::merged(edits).gates;
    let rows = vec![
        gate_row(
            cx,
            NumberSpec {
                title: "field.rules_min_chapters",
                description: "field.rules_min_chapters.desc",
                keywords: &["min chapters", "最少章节"],
                min: 0.0,
                max: 50.0,
                step: 1.0,
            },
            current.min_chapters,
            base.min_chapters,
            |gates| gates.min_chapters,
            |novel, value| novel.edits.gates.min_chapters = value,
        ),
        gate_row(
            cx,
            NumberSpec {
                title: "field.rules_title_max",
                description: "field.rules_title_max.desc",
                keywords: &["title length", "标题长度"],
                min: 8.0,
                max: 200.0,
                step: 1.0,
            },
            current.title_max_chars,
            base.title_max_chars,
            |gates| gates.title_max_chars,
            |novel, value| novel.edits.gates.title_max_chars = value,
        ),
        gate_row(
            cx,
            NumberSpec {
                title: "field.rules_median_chars",
                description: "field.rules_median_chars.desc",
                keywords: &["median", "中位"],
                min: 0.0,
                max: 5000.0,
                step: 50.0,
            },
            current.min_median_chapter_chars,
            base.min_median_chapter_chars,
            |gates| gates.min_median_chapter_chars,
            |novel, value| novel.edits.gates.min_median_chapter_chars = value,
        ),
        gate_row(
            cx,
            NumberSpec {
                title: "field.rules_toc_run",
                description: "field.rules_toc_run.desc",
                keywords: &["contents", "目录页"],
                min: 2.0,
                max: 100.0,
                step: 1.0,
            },
            current.toc_min_run,
            base.toc_min_run,
            |gates| gates.toc_min_run,
            |novel, value| novel.edits.gates.toc_min_run = value,
        ),
        gate_row(
            cx,
            NumberSpec {
                title: "field.rules_toc_gap",
                description: "field.rules_toc_gap.desc",
                keywords: &["gap", "间隔"],
                min: 0.0,
                max: 2000.0,
                step: 10.0,
            },
            current.toc_max_gap_chars,
            base.toc_max_gap_chars,
            |gates| gates.toc_max_gap_chars,
            |novel, value| novel.edits.gates.toc_max_gap_chars = value,
        ),
    ];
    group(tr(cx, "group.rules_gates"), rows)
}

/// 一行阈值。取值闭包**现读配置**（与其它页同一立场：规格每帧重建，捕获进来就是旧账），
/// 写回时等于出厂那个数就存 `None` —— 差量里留一条相同的值没意义，却会让以后出厂调那一格
/// 被这份旧数盖回去。
fn gate_row(
    cx: &App,
    spec: NumberSpec,
    _shown: usize,
    builtin: usize,
    read: impl Fn(&rules::Gates) -> usize + 'static,
    write: impl Fn(&mut folio_core::Novel, Option<usize>) + 'static,
) -> Row {
    number(
        cx,
        spec,
        move |cx| read(&rules::merged(&ConfigHolder::novel(cx).edits).gates) as f32,
        move |value, cx| {
            let bumped = value.round() as usize;
            ConfigHolder::edit_novel(cx, |novel| {
                write(novel, (bumped != builtin).then_some(bumped))
            })
        },
    )
}

/// 这一张表现在是哪一份：生效时机 + 生效表的内容哈希。
///
/// 哈希那一格是给「改了却没换」这句话找证据用的：读者改完、重启、开一本 `.txt`，
/// 要是哈希还是原来那 12 位，问题就说清了（没落盘 / 没重启 / 改动压根不影响分章）。
/// 只在这张表真的会作用到分章时才摆 —— 出厂那份与读者那份撞出同一个哈希也是可能的。
fn table_group(cx: &App) -> Group {
    let reset = Row::custom(|_, cx| {
        Button::new("rules-reset")
            .ghost()
            .small()
            .icon(IconName::RotateCcw)
            .label(tr(cx, "action.rules_reset").to_string())
            .accessibility_label(tr(cx, "action.rules_reset"))
            .on_click(|_, _, cx| ConfigHolder::edit_novel(cx, |novel| *novel = Default::default()))
            .into_any_element()
    });
    group(
        tr(cx, "group.rules_table"),
        vec![
            Row::info(
                tr(cx, "field.rules_apply"),
                tr(cx, "field.rules_apply.value"),
            )
            .keywords(&["restart", "重启", "生效"]),
            Row::info(tr(cx, "field.rules_hash"), rules::effective_hash())
                .description(tr(cx, "field.rules_hash.desc")),
            reset,
        ],
    )
}

/// 读者自己加的那几条。每条五格：开关、名字、匹配方式、模式（或整行文本）、编号怎么读，
/// 最后是一颗删除。加一条只在尾巴上追加。
fn custom_group(cx: &App, edits: &Edits) -> Group {
    let mut rows: Vec<Row> = Vec::new();
    // 哪几条被自己的正反例或坏模式作废了，问一次就好（`rows` 每次都合成一遍表）。
    let live = rules::rows(edits);
    for (index, marker) in edits.extra.iter().enumerate() {
        let name = marker.name.clone();
        // 自检没过的那条先在头顶说一句，免得读者对着一个开着的开关纳闷「为什么没切章」。
        if live.iter().any(|row| row.name == name && row.rejected) {
            rows.push(Row::info(
                SharedString::from(name.clone()),
                tr(cx, "field.rules_rejected.value"),
            ));
        }
        let (get_name, set_name) = (name.clone(), name.clone());
        rows.push(Row::toggle(
            tr(cx, "field.rules_on"),
            move |cx: &App| marker_on(&ConfigHolder::novel(cx).edits, &get_name),
            move |on, cx| set_marker(cx, &set_name, on),
        ));
        rows.push(Row::text(
            tr(cx, "field.rules_name"),
            move |cx: &App| shared_name(cx, index),
            move |value, cx| {
                ConfigHolder::edit_novel(cx, |novel| {
                    if let Some(marker) = novel.edits.extra.get_mut(index) {
                        marker.name = value.to_string();
                    }
                })
            },
        ));
        rows.push(Row::choice(
            tr(cx, "field.rules_kind"),
            vec![
                (
                    SharedString::from("literal"),
                    tr(cx, "field.rules_kind.literal"),
                ),
                (
                    SharedString::from("regex"),
                    tr(cx, "field.rules_kind.regex"),
                ),
            ],
            false,
            move |cx| SharedString::from(kind_key(marker_kind(cx, index))),
            move |value, cx| {
                ConfigHolder::edit_novel(cx, |novel| {
                    if let Some(marker) = novel.edits.extra.get_mut(index) {
                        marker.kind = match value.as_str() {
                            "regex" => MatchKind::Regex,
                            _ => MatchKind::Literal,
                        };
                    }
                })
            },
        ));
        rows.push(Row::text(
            tr(cx, "field.rules_pattern"),
            move |cx| shared_pattern(cx, index),
            move |value, cx| {
                ConfigHolder::edit_novel(cx, |novel| {
                    if let Some(marker) = novel.edits.extra.get_mut(index) {
                        // 整行相等那一档填的是 `text`，正则那一档填的是 `pattern`：
                        // 一格里存两种意思，下一帧换了匹配方式就会拿着老字符串去比。
                        if marker.kind == MatchKind::Regex {
                            marker.pattern = Some(value.to_string());
                            marker.text = None;
                        } else {
                            marker.text = Some(value.to_string());
                            marker.pattern = None;
                        }
                    }
                })
            },
        ));
        rows.push(Row::choice(
            tr(cx, "field.rules_numbering"),
            vec![
                (
                    SharedString::from("either"),
                    tr(cx, "field.rules_numbering.either"),
                ),
                (
                    SharedString::from("chinese"),
                    tr(cx, "field.rules_numbering.chinese"),
                ),
                (
                    SharedString::from("arabic"),
                    tr(cx, "field.rules_numbering.arabic"),
                ),
                (
                    SharedString::from("none"),
                    tr(cx, "field.rules_numbering.none"),
                ),
            ],
            false,
            move |cx| SharedString::from(numbering_key(marker_numbering(cx, index))),
            move |value, cx| {
                ConfigHolder::edit_novel(cx, |novel| {
                    if let Some(marker) = novel.edits.extra.get_mut(index) {
                        marker.numbering = match value.as_str() {
                            "chinese" => NumberingStyle::Chinese,
                            "arabic" => NumberingStyle::Arabic,
                            "none" => NumberingStyle::None,
                            _ => NumberingStyle::Either,
                        };
                    }
                })
            },
        ));
        rows.push(Row::custom(move |_, cx| {
            Button::new(format!("rules-drop-{index}"))
                .ghost()
                .small()
                .icon(IconName::Trash)
                .label(tr(cx, "action.delete").to_string())
                .accessibility_label(tr(cx, "action.rules_delete"))
                .on_click(move |_, _, cx| {
                    ConfigHolder::edit_novel(cx, |novel| {
                        if novel.edits.extra.len() > index {
                            novel.edits.extra.remove(index);
                        }
                    })
                })
                .into_any_element()
        }));
    }
    rows.push(Row::custom(|_, cx| {
        h_flex()
            .w_full()
            .child(
                Button::new("rules-add")
                    .ghost()
                    .small()
                    .icon(IconName::Plus)
                    .label(tr(cx, "action.rules_add").to_string())
                    .accessibility_label(tr(cx, "action.rules_add"))
                    .on_click(|_, _, cx| {
                        ConfigHolder::edit_novel(cx, |novel| {
                            let next = novel.edits.extra.len() + 1;
                            novel.edits.extra.push(rules::Marker {
                                name: format!("规则 {next}"),
                                kind: MatchKind::Literal,
                                pattern: None,
                                text: Some(String::new()),
                                level: 1,
                                numbering: NumberingStyle::Either,
                                examples: Default::default(),
                            });
                        })
                    }),
            )
            .into_any_element()
    }));
    group(tr(cx, "group.rules_custom"), rows)
}

// ---- 读写那几格的小助手：两端共用这一份，不在渲染器里各写一遍 ----

/// 这一条开着吗（没被这一层关掉）。
fn marker_on(edits: &Edits, name: &str) -> bool {
    !edits.off.iter().any(|off| off == name)
}

/// 开与关都只动 `off` 那一列：出厂的条目不复制进 `extra`，自定义的也不删 ——
/// 「关掉了但还留着」正是这一屏要说清的那件事。
fn set_marker(cx: &mut App, name: &str, on: bool) {
    let name = name.to_string();
    ConfigHolder::edit_novel(cx, move |novel| {
        if on {
            novel.edits.off.retain(|off| *off != name);
        } else if !novel.edits.off.iter().any(|off| off == &name) {
            novel.edits.off.push(name);
        }
    });
}

fn shared_name(cx: &App, index: usize) -> SharedString {
    SharedString::from(
        ConfigHolder::novel(cx)
            .edits
            .extra
            .get(index)
            .map(|marker| marker.name.clone())
            .unwrap_or_default(),
    )
}

fn shared_pattern(cx: &App, index: usize) -> SharedString {
    let marker = ConfigHolder::novel(cx).edits.extra.get(index).cloned();
    SharedString::from(
        marker
            .and_then(|marker| {
                if marker.kind == MatchKind::Regex {
                    marker.pattern
                } else {
                    marker.text
                }
            })
            .unwrap_or_default(),
    )
}

fn marker_kind(cx: &App, index: usize) -> MatchKind {
    ConfigHolder::novel(cx)
        .edits
        .extra
        .get(index)
        .map(|marker| marker.kind)
        .unwrap_or_default()
}

fn marker_numbering(cx: &App, index: usize) -> NumberingStyle {
    ConfigHolder::novel(cx)
        .edits
        .extra
        .get(index)
        .map(|marker| marker.numbering)
        .unwrap_or_default()
}

fn kind_key(kind: MatchKind) -> &'static str {
    match kind {
        MatchKind::Regex => "regex",
        MatchKind::Literal => "literal",
    }
}

fn numbering_key(numbering: NumberingStyle) -> &'static str {
    match numbering {
        NumberingStyle::None => "none",
        NumberingStyle::Chinese => "chinese",
        NumberingStyle::Arabic => "arabic",
        NumberingStyle::Either => "either",
    }
}
