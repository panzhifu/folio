//! 每本书一份的阅读设置**差量**：与全局默认不同的那几格，以及怎么盖回去。
//!
//! 数据半边（[`BookSettings`] 结构与库列编解码）已下沉 folio-model（2026-11-12
//! 阶段二，快照的 `SettingsRecord` 要带着它走）；这里留的是**咬着配置的行为**——
//! 它逐格比对走 serde，字段的名字、类型和区间住在 [`crate::infra::config::Reader`]
//! 里，而 settings 层按文档口径留在 core —— 经 `BookSettingsExt` 扩展 trait 挂回
//! 那个类型，与 folio-format::ext 同一条画法。调用方把 trait 引进作用域，方法
//! 语法不变。
//!
//! **逐格比对走 serde，不在这里手写 `Reader` 的字段清单**：这里再抄一份就是第二个
//! 真相源 —— 新增一格时抄的那份必然漏，症状是「这一格在全局有用、在这本书里没反应」。
//! 现在的写法是：默认值序列化成对象 → 差量盖上去 → 解回来，新增字段天然可覆盖。
//!
//! 唯一的例外是 [`APPEARANCE_GLOBAL_ONLY`] 那五格。它们不是「还没抄进来」，而是**语义上
//! 不该给单本书覆盖**（见那条常量的文档），所以用一张排除表挡住；有测试盯着这张表里的
//! 名字都还在，以及「除它们以外全部可覆盖」。
//!
//! 库里那一列怎么读、怎么写见 [`crate::store::Store::book_settings`]；编解码本身在
//! folio-model（`to_column` / `from_column`），这一节只管值与配置的换算，不碰 SQLite。

use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

use crate::infra::config::{Appearance, Reader};

// 数据半边：结构与库列编解码住 folio-model（旧路径沿用）。
pub use folio_model::reading::BookSettings;

/// `Appearance` 里**只归全局**、单本书不许覆盖的那几格。
///
/// - [`mode`](Appearance::mode) —— 软件外观的浅深。这一格管的是书架、设置、弹窗；把它
///   交给一本书，等于「读过这一本之后整个应用被钉在夜里」，而读者要的只是这一本夜里读。
///   阅读页自己的浅深走 [`reading_mode`](Appearance::reading_mode)。
/// - `light` / `dark` —— 两套主题的名字。它们与「这一本书」没有对应关系：选的是应用画
///   哪套配色，不是这一本的字怎么排。
/// - [`reading_mode`](Appearance::reading_mode) / [`reading_theme`](Appearance::reading_theme)
///   —— 阅读页的浅深与纸色。曾经可按书覆盖，但「每本书一个样式」让读者在两处各答一遍
///   同一个问题，还得多记一层折叠规则（本书差量盖全局默认），2026-11-07 起收归全局：
///   夜里看书、白天刷架、读什么纸，软件里只有一份答案。库里旧差量若还存着这两格，
///   会被 [`overlay`] 挡掉，安静失效、不需要迁移。
///
/// 反过来，`Appearance` 里其余的格子（亮度、翻页方式与时长、划词默认样式）
/// **默认就可覆盖** —— 新增一格时不用回来改这里，除非它像上面五格一样不归单本书。
/// 有测试钉住这条口径，见本文件底部。
pub const APPEARANCE_GLOBAL_ONLY: &[&str] =
    &["mode", "light", "dark", "reading_mode", "reading_theme"];

/// 咬着 `Reader` / `Appearance` 的那半行为：差量怎么盖回默认、生效值怎么记回差量。
///
/// 数据与行为分住两个 crate 后，这四个方法经 trait 回到 [`BookSettings`] 上 ——
/// 调用方（store、同步测试、app 的书内设置）引一次 trait，方法语法照旧。
pub trait BookSettingsExt {
    /// 这一本的排版输入：`base` 被差量盖一层，再按 [`Reader::clamped`] 夹住。
    ///
    /// 夹这一刀不能省：差量可能来自手改的库、旧版本或别的设备，越界值会让排版算出
    /// 零高一行或无限宽版心 —— 与磁盘上读回来的全局值走的是同一个入口。
    fn effective_reader(&self, base: &Reader) -> Reader;

    /// 这一本的外观输入：同上，按 [`Appearance::normalized`] 归一遍。
    fn effective_appearance(&self, base: &Appearance) -> Appearance;

    /// 记下「这一本现在的排版输入是 `effective`」。
    ///
    /// 与 `base` 逐格比，只留不同的：调回与默认同值的那一格会**自动从差量里消失**，
    /// 所以「恢复默认」不需要一格格推回去，抹掉整份就够了。
    fn record_reader(&mut self, base: &Reader, effective: &Reader);

    /// 记下这一本的外观输入；只归软件的那几格即使被改了也**不进差量**。
    fn record_appearance(&mut self, base: &Appearance, effective: &Appearance);
}

impl BookSettingsExt for BookSettings {
    fn effective_reader(&self, base: &Reader) -> Reader {
        overlay(base, &self.reader, &[]).map_or_else(|| base.clone(), Reader::clamped)
    }

    fn effective_appearance(&self, base: &Appearance) -> Appearance {
        overlay(base, &self.appearance, APPEARANCE_GLOBAL_ONLY)
            .map_or_else(|| base.clone(), Appearance::normalized)
    }

    fn record_reader(&mut self, base: &Reader, effective: &Reader) {
        self.reader = diff(base, effective, &[]);
    }

    fn record_appearance(&mut self, base: &Appearance, effective: &Appearance) {
        self.appearance = diff(base, effective, APPEARANCE_GLOBAL_ONLY);
    }
}

/// `base` 被 `patch` 盖一层，得到一个新的 `T`。`excluded` 里的字段名整格忽略。
///
/// **快路径**一次序列化 + 一次反序列化；只有整体解不动时才走**慢路径**逐格试 ——
/// 一处坏值不该把同一节里其它格子一起吃掉（与 `config.rs` 里
/// 「认不出的高亮样式回落但不吃掉同节的主题名」那条规矩同一个理由）。
/// 逐格试过之后还是解不动才交回 `None`，由调用方退回默认 —— 半套几何比整套默认更难查。
fn overlay<T>(base: &T, patch: &BTreeMap<String, Value>, excluded: &[&str]) -> Option<T>
where
    T: Serialize + serde::de::DeserializeOwned,
{
    let mut object = into_object(base)?;
    let merged = apply(object.clone(), patch, excluded);
    if let Ok(value) = serde_json::from_value::<T>(Value::Object(merged)) {
        return Some(value);
    }
    for (name, value) in patch {
        if excluded.contains(&name.as_str()) {
            continue;
        }
        object.insert(name.clone(), value.clone());
        if serde_json::from_value::<T>(Value::Object(object.clone())).is_err() {
            // 这一格落下去整个结构就解不开：丢掉它，先前落下的格子留着。
            object.remove(name);
        }
    }
    serde_json::from_value::<T>(Value::Object(object)).ok()
}

/// 把差量里**不在排除表**的那些格子盖到 `object` 上。
fn apply(
    mut object: Map<String, Value>,
    patch: &BTreeMap<String, Value>,
    excluded: &[&str],
) -> Map<String, Value> {
    for (name, value) in patch {
        if !excluded.contains(&name.as_str()) {
            object.insert(name.clone(), value.clone());
        }
    }
    object
}

/// `written` 相对 `base` 不同的那些格子。`base` 里没有的名字不认（差量不许凭空造格子）。
fn diff<A: Serialize, B: Serialize>(
    base: &A,
    written: &B,
    excluded: &[&str],
) -> BTreeMap<String, Value> {
    let (Some(mut base), Some(written)) = (into_object(base), into_object(written)) else {
        return BTreeMap::new();
    };
    for name in excluded {
        base.remove(*name);
    }
    let mut patch = BTreeMap::new();
    for (name, default) in base {
        if let Some(value) = written.get(&name).filter(|value| **value != default) {
            patch.insert(name, value.clone());
        }
    }
    patch
}

/// 序列化成对象。`T` 是结构体时必成，不是对象（或序列化失败）交回 `None`。
fn into_object<T: Serialize>(value: &T) -> Option<Map<String, Value>> {
    match serde_json::to_value(value) {
        Ok(Value::Object(object)) => Some(object),
        Ok(_) => None,
        Err(err) => {
            tracing::warn!("book_settings: 默认值序列化成不了对象：{err}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{APPEARANCE_GLOBAL_ONLY, BookSettings, BookSettingsExt, diff, into_object};
    use crate::infra::config::{
        Appearance, ImageScale, Mode, PageTurn, Reader, TextDirection, TocDensity,
    };
    use folio_model::types::AnnotationStyle;
    use serde_json::{Value, json};
    use std::collections::{BTreeMap, BTreeSet};

    /// 一份**每一格都不是默认值**的 `Reader`，用来逐格验证「这一格真的能被单本书覆盖」。
    ///
    /// 它不是默认值的第二份抄写，而是这一节唯一需要的样件：底部那条
    /// `the_sample_reader_differs_from_the_default_on_every_field` 会盯着它 ——
    /// 往 `Reader` 加新格子而没在这里填上非默认值，那条门就红，于是「新格子的可覆盖性
    /// 还没验过」不会被读成「已经验过了」。
    fn sample_reader() -> Reader {
        Reader {
            // 「与默认不同」是这一份样件的立身之本：默认档现在是 `Screen`（铺满），
            // 所以这一格取 `Follow`。底部那条门会盯着它。
            image_scale: ImageScale::Follow,
            font_size: 21.0,
            line_height: 2.25,
            paragraph_gap_em: 1.5,
            indent_em: 3.0,
            letter_spacing_em: 0.08,
            max_measure: 640.0,
            margin_x: 32.0,
            margin_y: 16.0,
            justify: false,
            font_weight: 700,
            text_direction: TextDirection::Vertical,
            font_family: "Noto Serif".into(),
            mono_font_family: "JetBrains Mono".into(),
            toc_density: TocDensity::Compact,
            toc_max_title_lines: 3,
            toc_dim_read: false,
            // 图集字节上限也允许一本书单独压一档（扫描本那类「一页就是 12 MB」的想让
            // 眼前这一页装得下时，不必把整机的预算抬上去）。默认是 0 = 跟随设备。
            image_cache_bytes: 64 * 1024 * 1024,
        }
    }

    /// 外观那半的同一条规矩：**连只归全局的五格也填成非默认值**。
    ///
    /// 只有它们的值与默认不同，「一本书改不动它们」才测得到东西 —— 样件里留默认值的话，
    /// 「被排除」和「值本来就一样」这两种情况在断言里长得一模一样。
    fn sample_appearance() -> Appearance {
        Appearance {
            mode: Mode::Dark,
            reading_mode: Some(Mode::Light),
            light: Some("Gruvbox".into()),
            dark: Some("Molokai".into()),
            // 正文主题名同样只归全局；这里填非默认值，为的是「被排除」测得出。
            reading_theme: Some("sepia".into()),
            page_turn: PageTurn::Cover,
            auto_advance_speed: 24.0,
            turn_duration: 500.0,
            highlight_style: AnnotationStyle::Wavy,
            brightness: Some(0.4),
        }
    }

    /// 单格差量的构造：名字 → 样件里那一格的值。
    fn one(name: &str, value: &Value) -> BTreeMap<String, Value> {
        BTreeMap::from([(name.to_string(), value.clone())])
    }

    /// 把值序列化成对象，只为**比对**用。
    ///
    /// 生效值一律由生产函数（[`BookSettingsExt`] 那两个 `effective_*`）算出来 —— 测试里不
    /// 再实现第二套「盖格子」的逻辑，否则量具与生产路径分叉，测过的就不是要交出去的那份。
    fn object<T: serde::Serialize>(value: &T) -> Value {
        serde_json::to_value(value).unwrap()
    }

    /// 差量里每一格都真落上了，没进差量的每一格都仍等于默认。
    fn assert_patch_lands(base: &Value, effective: &Value, patch: &BTreeMap<String, Value>) {
        let base_obj = base.as_object().expect("默认值该是对象");
        let effective_obj = effective.as_object().expect("生效值该是对象");
        for (name, value) in patch {
            assert_eq!(
                &effective_obj[name], value,
                "{name} 在差量里却没落到生效值上"
            );
        }
        for (name, value) in base_obj {
            if !patch.contains_key(name) {
                assert_eq!(&effective_obj[name], value, "{name} 不在差量里却偏离了默认");
            }
        }
    }

    /// 整份默认 → 整份样件 → 差量 → 盖回默认，逐格对得上。
    ///
    /// 这一条是这一节的主干：两头的换算不许各自演化出一套字段清单。
    #[test]
    fn a_full_override_round_trips_through_the_diff() {
        let mut settings = BookSettings::default();
        settings.record_reader(&Reader::default(), &sample_reader());
        settings.record_appearance(&Appearance::default(), &sample_appearance());

        let effective_reader = settings.effective_reader(&Reader::default());
        assert_eq!(
            effective_reader,
            sample_reader(),
            "排版那一节整份都该盖回去"
        );
        assert_patch_lands(
            &object(&Reader::default()),
            &object(&effective_reader),
            &settings.reader,
        );
        assert_patch_lands(
            &object(&Appearance::default()),
            &object(&settings.effective_appearance(&Appearance::default())),
            &settings.appearance,
        );

        // 排版那一节的每一格都进了差量（样件就是按「每格都不同」写的）；
        // 外观那一节只进可覆盖的那四格。
        assert_eq!(
            settings.reader.len(),
            into_object(&Reader::default()).unwrap().len(),
            "样件每一格都该进排版差量"
        );
        let overridden: BTreeSet<&str> = settings.appearance.keys().map(String::as_str).collect();
        assert_eq!(
            overridden,
            BTreeSet::from([
                "auto_advance_speed",
                "brightness",
                "highlight_style",
                "page_turn",
                "turn_duration"
            ]),
            "只归全局的那五格不该出现在外观差量里"
        );
    }

    /// 逐格自动覆盖：对序列化出来的**每一个**字段单独盖一次，断言「生效值认得到这一格」
    /// 且「记回来的差量只有这一格」。
    ///
    /// 循环的输入来自 serde，不是手写的名字清单 —— 新增一格会被这条门自动带走，
    /// 不需要回来加测试。
    #[test]
    fn every_field_is_overridable_on_its_own() {
        let reader_defaults = into_object(&Reader::default()).unwrap();
        let reader_sample = into_object(&sample_reader()).unwrap();
        for name in reader_defaults.keys() {
            let settings = BookSettings {
                reader: one(name, &reader_sample[name]),
                appearance: BTreeMap::new(),
            };
            let effective = settings.effective_reader(&Reader::default());
            assert_ne!(
                object(&effective)[name],
                reader_defaults[name],
                "盖了 {name} 而生效值没变 —— 这一格落不进单本书的差量"
            );

            let mut recorder = BookSettings::default();
            recorder.record_reader(&Reader::default(), &effective);
            assert_eq!(
                recorder.reader.keys().collect::<Vec<_>>(),
                vec![name],
                "只盖了 {name}，记回来的差量却多了格子"
            );
        }

        let appearance_defaults = into_object(&Appearance::default()).unwrap();
        let appearance_sample = into_object(&sample_appearance()).unwrap();
        for name in appearance_defaults.keys() {
            let settings = BookSettings {
                reader: BTreeMap::new(),
                appearance: one(name, &appearance_sample[name]),
            };
            let effective = settings.effective_appearance(&Appearance::default());

            if APPEARANCE_GLOBAL_ONLY.contains(&name.as_str()) {
                assert_eq!(
                    object(&effective)[name],
                    appearance_defaults[name],
                    "{name} 只归软件，一本书的差量不该改到它"
                );
                continue;
            }
            assert_ne!(
                object(&effective)[name],
                appearance_defaults[name],
                "盖了 {name} 而生效值没变 —— 这一格落不进单本书的差量"
            );
            let mut recorder = BookSettings::default();
            recorder.record_appearance(&Appearance::default(), &effective);
            assert_eq!(
                recorder.appearance.keys().collect::<Vec<_>>(),
                vec![name],
                "只盖了 {name}，记回来的差量却多了格子"
            );
        }
    }

    /// 排除表里的名字必须真的还在结构里：改名或删格子时这里先红。
    #[test]
    fn the_exclusion_list_names_fields_that_still_exist() {
        let fields = into_object(&Appearance::default()).unwrap();
        for name in APPEARANCE_GLOBAL_ONLY {
            assert!(
                fields.contains_key(*name),
                "排除表里的 {name} 在 Appearance 里已经找不到了"
            );
        }
    }

    /// 浅深、两套主题名与阅读页自己的浅深 / 纸色，都不许被一本书写进差量，也不许被差量
    /// 盖到生效值上。
    #[test]
    fn the_global_only_fields_are_ignored_in_both_directions() {
        let base = Appearance::default();
        let mut settings = BookSettings::default();
        settings.record_appearance(&base, &sample_appearance());
        assert_eq!(
            settings
                .appearance
                .keys()
                .filter(|name| APPEARANCE_GLOBAL_ONLY.contains(&name.as_str()))
                .count(),
            0,
            "记差量时混进了只归全局的格子：{:?}",
            settings.appearance.keys().collect::<Vec<_>>()
        );

        // 反向：手改库塞进来的那五格也不许落到生效值上。
        for (name, value) in [
            ("mode", json!("dark")),
            ("light", json!("Gruvbox")),
            ("dark", json!("Molokai")),
            ("reading_mode", json!("dark")),
            ("reading_theme", json!("sepia")),
        ] {
            settings.appearance.insert(name.into(), value);
        }
        let effective = settings.effective_appearance(&base);
        assert_eq!(effective.mode, base.mode, "一本书的差量改到全局浅深了");
        assert_eq!(effective.light, base.light);
        assert_eq!(effective.dark, base.dark);
        assert_eq!(
            effective.reading_mode, base.reading_mode,
            "阅读浅深只归全局"
        );
        assert_eq!(
            effective.reading_theme, base.reading_theme,
            "正文主题只归全局"
        );
        assert_eq!(
            effective.brightness,
            Some(0.4),
            "那五格坏值不该把同节里可覆盖的那几格一起吃掉"
        );
        assert_eq!(effective.page_turn, PageTurn::Cover);
    }

    /// 调回与默认同值 = 那一格自己从差量里消失，不需要「恢复默认」那一刀。
    #[test]
    fn a_field_pushed_back_to_the_default_drops_out_of_the_patch() {
        let base = Reader::default();
        let mut settings = BookSettings::default();

        let mut big = base.clone();
        big.font_size = 26.0;
        settings.record_reader(&base, &big);
        assert_eq!(settings.reader.len(), 1);

        settings.record_reader(&base, &base);
        assert!(settings.is_empty(), "生效值退回默认之后差量该整份空掉");
    }

    /// 一处坏值只坏那一格：同节里别的格子照落。
    #[test]
    fn a_broken_field_does_not_take_the_whole_section_with_it() {
        let base = Reader::default();
        let mut patch = BTreeMap::new();
        patch.insert("font_size".into(), json!("十八"));
        patch.insert("line_height".into(), json!(2.5));
        let settings = BookSettings {
            reader: patch,
            appearance: BTreeMap::new(),
        };

        let effective = settings.effective_reader(&base);
        assert_eq!(effective.font_size, base.font_size, "坏值该回落");
        assert_eq!(effective.line_height, 2.5, "同节里那一格好值该留着");

        // 认不出的名字也只是没作用，不该把整节吃掉。
        let mut with_stray = settings.clone();
        with_stray.reader.insert("font_color".into(), json!("#fff"));
        assert_eq!(
            with_stray.effective_reader(&base).line_height,
            2.5,
            "凭空造的格子不该吃掉别的格子"
        );
    }

    /// 每一格单独都落不下去时，退回整套默认值而不是半套几何。
    #[test]
    fn a_wholly_unusable_patch_falls_back_to_the_defaults() {
        let base = Reader::default();
        let mut patch = BTreeMap::new();
        patch.insert("font_weight".into(), json!([1, 2, 3]));
        patch.insert("justify".into(), json!("not-a-bool"));
        let settings = BookSettings {
            reader: patch,
            appearance: BTreeMap::new(),
        };

        assert_eq!(settings.effective_reader(&base), base);
    }

    /// 越界值走的是全局那条 [`Reader::clamped`]，不在这里另夹一套。
    #[test]
    fn an_out_of_range_value_from_the_column_gets_clamped() {
        let base = Reader::default();
        let mut patch = BTreeMap::new();
        patch.insert("font_size".into(), json!(4000.0));
        let settings = BookSettings {
            reader: patch,
            appearance: BTreeMap::new(),
        };

        assert_eq!(
            settings.effective_reader(&base).font_size,
            Reader::FONT_SIZE_MAX
        );
    }

    /// `clear` 是「恢复本书默认」那一刀：抹完之后生效值 = 默认值。
    /// （列的编解码本身在 folio-model 那侧测；这里钉的是 clear 与生效值的联动。）
    #[test]
    fn clearing_the_book_settings_returns_it_to_the_defaults() {
        let base = Reader::default();
        let mut wanted = base.clone();
        wanted.margin_x = 48.0;
        let mut settings = BookSettings::default();
        settings.record_reader(&base, &wanted);
        assert!(!settings.is_empty());

        settings.clear();
        assert!(settings.is_empty());
        assert_eq!(settings.effective_reader(&base), base);
    }

    /// 盯住样件本身：`Reader` 每一格都得在样件里被填成非默认值，否则上面那逐格的门
    /// 会把「这格子的可覆盖性还没验过」当成「验过了」。
    #[test]
    fn the_sample_reader_differs_from_the_default_on_every_field() {
        let defaults = into_object(&Reader::default()).unwrap();
        let sample = into_object(&sample_reader()).unwrap();
        assert_eq!(
            defaults.keys().collect::<BTreeSet<_>>(),
            sample.keys().collect::<BTreeSet<_>>(),
            "样件与 `Reader` 的字段集不一致 —— 加格子时要把样件补上"
        );
        for name in defaults.keys() {
            assert_ne!(
                defaults[name], sample[name],
                "样件里 {name} 与默认同值，逐格覆盖那条门测不到它"
            );
        }
    }

    /// `diff` 只认 `base` 里已有的名字：差量不许凭空造格子。
    #[test]
    fn diff_ignores_names_the_base_does_not_have() {
        let base = Reader::default();
        let mut written = base.clone();
        written.font_size = 22.0;

        let patch = diff(&base, &written, &[]);
        assert_eq!(patch.len(), 1, "只该有一格不同");
    }
}
