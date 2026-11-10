//! 架构契约：**`config::Reader` 的每个字段都进排版失效键**。
//!
//! 排版键对偏好只问「整份序列化的哈希」（机制在 folio-render 的 `layout::key`），所以
//! 给 `Reader` 加字段自动进键 —— 这份清单钉的就是那张字段表本身：加字段而没在这里
//! 补一行，最后那条断言会红。接线前那条手挑 `prefs_hash` 漏了 `mono_font_family`
//! （等宽泛族名是折行的输入），症状是「改了设置版式不动」，屏幕上看不出来。
//!
//! 它住在 folio-core 而不是 folio-render：字段表是 `config::Reader` 的字段表，
//! 跟着配置类型走。

use folio_core::layout::{LayoutInputs, LayoutKey, content_hash};

/// 「动一下某项偏好」的写法：字段名 + 一个改它的函数。
///
/// 起个名字而不是把类型写在常量上：那个元组 + 函数指针的类型复杂到 clippy 会拦，
/// 而这张清单读起来比类型重要。
type PrefTweak = (&'static str, fn(&mut folio_core::Reader));

/// 每项读者偏好的「动一下它」写法。清单与 `Reader` 的字段表一一对应
/// （下面那条断言守着），加字段时照着补一行。
const PREF_TWEAKS: [PrefTweak; 18] = [
    ("font_size", |p| p.font_size += 1.0),
    ("line_height", |p| p.line_height += 0.1),
    ("paragraph_gap_em", |p| p.paragraph_gap_em += 0.1),
    ("indent_em", |p| p.indent_em += 0.5),
    ("letter_spacing_em", |p| p.letter_spacing_em += 0.01),
    ("max_measure", |p| p.max_measure += 10.0),
    ("margin_x", |p| p.margin_x += 4.0),
    ("margin_y", |p| p.margin_y += 4.0),
    ("justify", |p| p.justify = !p.justify),
    ("font_weight", |p| p.font_weight += 100),
    ("text_direction", |p| {
        p.text_direction = folio_core::TextDirection::Vertical
    }),
    ("font_family", |p| p.font_family = "serif".into()),
    ("mono_font_family", |p| p.mono_font_family = "mono2".into()),
    ("toc_density", |p| {
        p.toc_density = folio_core::TocDensity::Compact
    }),
    ("toc_max_title_lines", |p| p.toc_max_title_lines += 1),
    ("toc_dim_read", |p| p.toc_dim_read = !p.toc_dim_read),
    // 图集的字节上限：它不改一行几何，但**跟着整份 `Reader` 进键**（这一格的口径是
    // 「序列化整份取哈希」，没有逐字段豁免）。消费它的代码在 `ui::features::reader::
    // model::image_atlas`，那里有一条测试钉住覆盖生效。
    ("image_cache_bytes", |p| p.image_cache_bytes += 1),
    // 图片缩放：进键的理由与上面那条同一句（整份序列化取哈希，没有逐字段豁免）。
    // 生效的判据不在这里 —— 那一头的三档各有各的门：
    // `a_wide_image_is_capped_by_the_screen_rather_than_the_column`、
    // `the_fills_the_screen_rule_keyed_on_object_fit_and_size`、
    // `a_block_image_follows_the_readers_scale_tier`。
    //
    // 这一份必须挑**与默认不同**的那一档：默认是 `Screen`（铺满，2026-10-31 从 `Follow`
    // 换过来），还写 `Screen` 就是拿默认当变化，这条门当场空转。
    ("image_scale", |p| {
        p.image_scale = folio_core::ImageScale::Follow
    }),
];

#[test]
fn every_reader_preference_changes_the_key() {
    let base = key_for("<p>偏好</p>", 600.0);
    for (name, tweak) in PREF_TWEAKS {
        let mut changed = folio_core::Reader::default();
        tweak(&mut changed);
        let key = LayoutKey::compose(
            LayoutInputs::compute("", &changed),
            content_hash("<p>偏好</p>"),
            600.0,
            1,
            None,
            0,
            0,
        );
        assert_ne!(base.prefs_hash, key.prefs_hash, "偏好 {name} 没进键");
    }

    // 清单与 `Reader` 的序列化字段表必须一模一样（serde 的 derive 就是那张表本身）。
    let value = serde_json::to_value(folio_core::Reader::default()).unwrap();
    let mut fields: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
    let mut listed: Vec<&str> = PREF_TWEAKS.iter().map(|(name, _)| *name).collect();
    listed.sort_unstable();
    fields.sort_unstable();
    assert_eq!(
        listed, fields,
        "这张清单没跟上 `Reader` 的字段表：新增偏好字段要在这里补一行"
    );
}

/// 测试用的键：只给章文本与栏宽，其余各格取「没有别的输入」那一档。
fn key_for(source: &str, width: f32) -> LayoutKey {
    LayoutKey::compose(
        LayoutInputs::compute("", &folio_core::Reader::default()),
        content_hash(source),
        width,
        1,
        None,
        0,
        0,
    )
}
