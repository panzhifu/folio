use crate::infra::config::{Config, Layer, Membership, MembershipTier, WatchDir};
use crate::infra::i18n::Locale;
use crate::{AnnotationStyle, Appearance, Library, Mode, PageTurn, Provenance, Reader};
use std::collections::BTreeSet;

fn load(text: &str) -> Config {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, text).unwrap();
    Config::load_from(&path)
}

/// 一处类型写错的字段只该毁掉**它自己那一节**：读者手改 `"font_size":"big"` 之后，
/// 不该看到主题名、书库目录一起消失，而且没有任何提示。
#[test]
fn one_broken_section_does_not_take_the_others_down() {
    let got = load(
        r#"{"reader":{"font_size":"big","line_height":2.5},
        "appearance":{"mode":"dark","light":"Gruvbox"},
        "library":{"dir":"~/Books"},
        "language":{"locale":"ja"}}"#,
    );
    assert_eq!(
        got.reader,
        Reader::default(),
        "坏的是 reader 一节，就只有这一节退回默认"
    );
    assert_eq!(got.appearance.mode, Mode::Dark);
    assert_eq!(got.appearance.light.as_deref(), Some("Gruvbox"));
    assert_eq!(got.library.dir, "~/Books");
    assert_eq!(got.language.locale, Locale::Ja);
}

/// 补救的粒度是「一节」而不是「一个键」：`line_height` 没写错，但它的同节邻居错了，
/// 于是整节一起退回。serde 的字段级默认只在整节解成功时才有机会生效，
/// 这里把它明确成预期行为，而不是让人以为是 bug。
#[test]
fn the_good_keys_in_a_broken_section_fall_back_too() {
    let got = load(r#"{"reader":{"font_size":"big","line_height":2.5}}"#);
    assert_eq!(got.reader.line_height, Reader::default().line_height);
}

/// 连 JSON 本身都解析不了（截断、括号不配对）时没有「节」可以逐节尝试，
/// 仍然整份退回默认值。
#[test]
fn unparseable_json_still_falls_back_as_a_whole() {
    assert_eq!(load(r#"{"reader":{"font_size":21.0}"#), Config::default());
}

/// 手改过的越界值在读入时要被夹住（否则排版会算出零高或无限宽）。
#[test]
fn out_of_range_values_are_clamped_on_load() {
    let got = load(
        r#"{"reader":{"font_size":9000,"line_height":0,"indent_em":-5,"margin_x":1e9,"letter_spacing_em":5,"margin_y":-20,"font_weight":9999},"language":{"locale":"ja"}}"#,
    );
    assert_eq!(got.reader.font_size, 72.0);
    assert_eq!(got.reader.line_height, 1.0);
    assert_eq!(got.reader.indent_em, 0.0);
    assert_eq!(got.reader.margin_x, 400.0);
    assert_eq!(got.reader.letter_spacing_em, Reader::LETTER_SPACING_MAX);
    assert_eq!(got.reader.margin_y, 0.0);
    assert_eq!(got.reader.font_weight, 900);
    assert_eq!(
        got.language.locale,
        Locale::Ja,
        "夹取只管排版数值，不碰别的节"
    );
}

/// 空白主题名被当成「没设」；0 的 max_measure 是合法的铺满，不能被夹成别的值。
#[test]
fn blank_theme_names_are_dropped_and_fill_width_survives_clamping() {
    let got = load(r#"{"reader":{"max_measure":0},"appearance":{"light":"   ","dark":"Molokai"}}"#);
    assert_eq!(
        got.appearance.light, None,
        "全空白的主题名不该存成一个显示不出来的选项"
    );
    assert_eq!(got.appearance.dark.as_deref(), Some("Molokai"));
    assert_eq!(got.reader.max_measure, 0.0, "0 = 随窗口铺满");
}

/// 手改进配置的字重（100–900 之间任何数）在设置界面要落回最近的一档，
/// 否则下拉按钮上没有可念的字。
#[test]
fn a_hand_edited_weight_snaps_to_the_nearest_preset() {
    assert_eq!(Reader::nearest_font_weight(400), 400);
    assert_eq!(Reader::nearest_font_weight(600), 700, "600 离加粗更近");
    assert_eq!(
        Reader::nearest_font_weight(100),
        300,
        "比最细还细的落到最细"
    );
    assert_eq!(
        Reader::nearest_font_weight(900),
        700,
        "比最粗还粗的落到最粗"
    );
}

/// 亮度这一格只有「不接管」一个顶格写法，而越界的数夹住而不是整格丢掉。
/// 界面那条滑条的两端读的就是这两个数（`Appearance::BRIGHTNESS_MIN` 与 `1.0`），
/// 所以夹取规则钉在这里：滑条与配置之间不许有第二套「最大亮度」的表示法。
#[test]
fn the_brightness_top_end_is_the_same_thing_as_following_the_system() {
    for written in ["1.0", "3.5"] {
        let got = load(&format!(r#"{{"appearance":{{"brightness":{written}}}}}"#));
        assert_eq!(
            got.appearance.brightness, None,
            "{written} 是推到顶，该归成「跟随系统」而不是留一个 1.0"
        );
    }
    for written in ["0.0", "0.001", "-2.0"] {
        let got = load(&format!(r#"{{"appearance":{{"brightness":{written}}}}}"#));
        assert_eq!(
            got.appearance.brightness,
            Some(Appearance::BRIGHTNESS_MIN),
            "{written} 越界了，夹到下限而不是丢掉整格"
        );
    }
    assert_eq!(
        load(r#"{"appearance":{"brightness":0.6}}"#)
            .appearance
            .brightness,
        Some(0.6),
        "区间内的数原样留着"
    );
    assert_eq!(
        load(r#"{"appearance":{"mode":"dark"}}"#)
            .appearance
            .brightness,
        None,
        "没写过这一格就是交还系统"
    );
}

/// 翻页时长那一条滑条的两端与默认档就是这里这三个数，所以夹取规则钉在同一格：
/// 界面与配置之间不许有第二套「最慢/最快」的表示法，而 `NaN` 退回**默认档**而不是
/// 某个端点 —— 坏数据不该被读成读者的一个选择。
#[test]
fn the_turn_duration_clamps_to_the_slider_ends() {
    for (written, want) in [
        ("-1.0", Appearance::TURN_DURATION_MIN),
        ("60.0", Appearance::TURN_DURATION_MIN),
        ("300.0", 300.0),
        ("1800.0", Appearance::TURN_DURATION_MAX),
    ] {
        let got = load(&format!(
            r#"{{"appearance":{{"turn_duration":{written}}}}}"#
        ));
        assert_eq!(
            got.appearance.turn_duration, want,
            "{written} 该读成 {want}，实得 {}",
            got.appearance.turn_duration
        );
    }
    // 缺键走容器的 `#[serde(default)]`：取整份 `Default`，于是没写过这一格就是默认档，
    // 而不是零时长（那等于纸当场落定）。
    let got = load(r#"{"appearance":{"mode":"dark"}}"#);
    assert_eq!(
        got.appearance.turn_duration,
        Appearance::TURN_DURATION_DEFAULT,
        "没写过这一格就是默认档"
    );
    // 非有限值 → 退回默认档而不是夹到最慢那一头。这一条不走 JSON：`NaN` 不是合法
    // JSON 字面量，而 `1e400` 在 serde_json 那侧先炸成读不出键，测不到这一格。
    let nan = Appearance {
        turn_duration: f32::NAN,
        ..Default::default()
    }
    .normalized();
    assert_eq!(
        nan.turn_duration,
        Appearance::TURN_DURATION_DEFAULT,
        "NaN 该退回默认档，而不是被当成「推到最慢」"
    );
}

/// 认不出的 mode 字符串不该让配置读失败或直接变成深色。
#[test]
fn an_unknown_mode_falls_back_to_following_the_system() {
    assert_eq!(
        load(r#"{"appearance":{"mode":"nonsense"}}"#)
            .appearance
            .mode,
        Mode::System
    );
    assert_eq!(Mode::parse("dark"), Mode::Dark);
    assert_eq!(Mode::parse("ink"), Mode::Ink);
    assert_eq!(Mode::default(), Mode::System);
}

/// 认不出的 `page_turn` 回落**初始那一档**（不走动画），而且**同一节里别的键一个都不许
/// 跟着没**。
/// 这一格的存在理由是：`PageTurn` 的 `Deserialize` 是自己实现的（同 [`Mode`]、
/// [`TocDensity`] 那两条），derive 在这里会让整节 `appearance` 退回默认 ——
/// 浅、深两侧选过的主题名就成了一个拼错的动画键的代价。
#[test]
fn an_unknown_page_turn_falls_back_without_taking_the_section_with_it() {
    let got = load(r#"{"appearance":{"mode":"dark","light":"Gruvbox","page_turn":"spin"}}"#);
    assert_eq!(got.appearance.page_turn, PageTurn::None);
    assert_eq!(got.appearance.mode, Mode::Dark);
    assert_eq!(got.appearance.light.as_deref(), Some("Gruvbox"));
    // 认得出的档各自落对：兜底不许把认得出的那几个吃掉。`horizontal` / `vertical` 是
    // 这一档只有两格时写进配置的旧键名，换过版本不该让读者的选项忽然回到默认。
    for (text, want) in [
        ("slide", PageTurn::Slide),
        ("slide_up", PageTurn::SlideUp),
        ("cover", PageTurn::Cover),
        ("curl", PageTurn::Curl),
        ("scroll", PageTurn::Scroll),
        ("none", PageTurn::None),
        ("horizontal", PageTurn::Slide),
        ("vertical", PageTurn::SlideUp),
    ] {
        assert_eq!(
            load(&format!(r#"{{"appearance":{{"page_turn":"{text}"}}}}"#))
                .appearance
                .page_turn,
            want,
            "配置里的 {text:?} 落错了档"
        );
    }
    assert_eq!(PageTurn::default(), PageTurn::None);
}

/// 认不出的 `highlight_style` 回落默认那档（底色带），而**同一节里别的键一个都不许跟着没**。
/// 这条与上面那条 `page_turn` 是同一条规矩的第二次发生：`AnnotationStyle` 的
/// `Deserialize` 也是自己实现的，derive 会让一个拼错的画法键吃掉浅深两侧选过的主题名。
///
/// 顺带钉住那个**两个默认值不许互相串**的分工：配置里缺这一格时读成 `highlight`
/// （「往后的新条目」），而库里那一列缺值时读成 `line`（「v21 之前那些条」），
/// 见 `store::annotations` 的 `a_row_written_before_the_style_column_reads_back_as_line`。
#[test]
fn an_unknown_highlight_style_falls_back_without_taking_the_section_with_it() {
    let got =
        load(r#"{"appearance":{"mode":"dark","light":"Gruvbox","highlight_style":"zigzag"}}"#);
    assert_eq!(got.appearance.highlight_style, AnnotationStyle::Highlight);
    assert_eq!(got.appearance.mode, Mode::Dark);
    assert_eq!(got.appearance.light.as_deref(), Some("Gruvbox"));
    for (text, want) in [
        ("highlight", AnnotationStyle::Highlight),
        ("line", AnnotationStyle::Line),
        ("wavy", AnnotationStyle::Wavy),
    ] {
        assert_eq!(
            load(&format!(
                r#"{{"appearance":{{"highlight_style":"{text}"}}}}"#
            ))
            .appearance
            .highlight_style,
            want,
            "配置里的 {text:?} 落错了档"
        );
    }
    // 一份没有这一格的旧配置：读成默认档，而不是让整节 appearance 退回默认。
    let old = load(r#"{"appearance":{"light":"Gruvbox"}}"#);
    assert_eq!(old.appearance.highlight_style, AnnotationStyle::default());
    assert_eq!(old.appearance.light.as_deref(), Some("Gruvbox"));
}

/// 阅读页的浅深是**独立一格**：缺省是「跟随软件外观」，写了它也不许改到全局 `mode`。
///
/// 这条门存在是因为这一格与 `mode` 长得太像 —— 一旦实现图省事把夜间写回 `mode`，
/// 症状是「读完书关掉，整个应用的浅深跟着变了，而且下次开书之前还是那样」。
/// 认不出的值按 `Mode::parse` 回落「跟随系统」，同一节里的主题名仍要留着。
#[test]
fn the_reader_mode_is_optional_and_never_touches_the_app_mode() {
    let old = load(r#"{"appearance":{"mode":"light"}}"#);
    assert_eq!(
        old.appearance.reading_mode, None,
        "旧配置没有这一格时该跟软件外观走"
    );

    let pinned = load(r#"{"appearance":{"mode":"light","reading_mode":"dark"}}"#);
    assert_eq!(pinned.appearance.reading_mode, Some(Mode::Dark));
    assert_eq!(
        pinned.appearance.mode,
        Mode::Light,
        "阅读页那一格不该改到全局浅深"
    );

    let broken = load(r#"{"appearance":{"light":"Gruvbox","reading_mode":"sepia"}}"#);
    assert_eq!(broken.appearance.reading_mode, Some(Mode::System));
    assert_eq!(
        broken.appearance.light.as_deref(),
        Some("Gruvbox"),
        "一个坏掉的阅读浅深不该把同一节的主题名一起吃掉"
    );
}

/// 四层各自有名字，而**被一本书盖过的那一格要在来源表里念 `book`**。
///
/// 这一档不是装饰：诊断 dump 打的是生效值，而「我在设置屏改了字号、这本书怎么不动」
/// 是这张表要回答的那类事故。少了 `book`，那一格会被报成来自 `file` —— 恰好是错的。
#[test]
fn a_book_override_is_attributable_in_the_provenance_table() {
    let mut provenance = Provenance::default();
    provenance.mark("reader.font_size", Layer::File);
    provenance.mark("reader.font_size", Layer::Book);
    assert_eq!(provenance.layer_of("reader.font_size"), Layer::Book);
    assert_eq!(
        provenance.entries().collect::<Vec<_>>(),
        vec![("reader.font_size", Layer::Book)],
        "同一格被两层盖过时，表里只留最高那一档"
    );
    assert_eq!(Layer::Book.as_str(), "book");
}

/// 「跟随软件」那一档在**界面值与 `Option<Mode>`** 之间往返，五格互不相同。
///
/// 下拉与菜单的选项只有字符串一种形状，而配置里 `None` 表示「没选过」—— 这一对
/// 函数是两者之间唯一的翻译。翻错的症状是「选了跟随软件，落盘却钉成跟随系统」。
#[test]
fn the_reading_mode_round_trips_through_its_interface_value() {
    assert_eq!(Mode::reading_value(None), Mode::FOLLOW_APP);
    assert_eq!(Mode::parse_reading(Mode::FOLLOW_APP), None);
    for mode in [Mode::System, Mode::Light, Mode::Dark, Mode::Ink] {
        assert_eq!(
            Mode::parse_reading(Mode::reading_value(Some(mode))),
            Some(mode),
            "{mode:?} 那一档过不去"
        );
    }
    // 认不出的值与配置文件同一口径：落到「跟随系统」，不许变成「跟随软件」。
    assert_eq!(Mode::parse_reading("sepia"), Some(Mode::System));
    // 五格的界面值互不相同，否则下拉里会有两行指向同一个选择（多一档就要多一格，
    // 少一格就是那一档在选择器里根本摆不出来）。
    let values = [
        None,
        Some(Mode::System),
        Some(Mode::Light),
        Some(Mode::Dark),
        Some(Mode::Ink),
    ]
    .map(Mode::reading_value);
    assert_eq!(
        values.iter().collect::<BTreeSet<_>>().len(),
        5,
        "界面值撞了：{values:?}"
    );
}

#[test]
fn an_empty_font_family_falls_back_instead_of_nuking_the_text() {
    let got = load(r#"{"reader":{"font_family":"   "}}"#);
    assert_eq!(got.reader.font_family, Reader::default().font_family);
}

/// 监听目录为空、或就是书库目录自己：前者没得盯、后者会自我循环
/// （书库里出现的书就是刚从那里拷进来的），normalize 都当没开。
#[test]
fn the_watch_section_needs_a_distinct_directory() {
    let config = Config {
        library: Library {
            dir: "/books".into(),
            watch: Some(WatchDir {
                dir: "/books".into(),
                interval_secs: 30,
            }),
        },
        ..Default::default()
    };
    assert!(config.normalized().library.watch.is_none());

    let config = Config {
        library: Library {
            dir: "/books".into(),
            watch: Some(WatchDir {
                dir: "  ".into(),
                interval_secs: 30,
            }),
        },
        ..Default::default()
    };
    assert!(config.normalized().library.watch.is_none());

    // 节奏越界夹回界内；正经目录原样保留。
    let config = Config {
        library: Library {
            dir: "/books".into(),
            watch: Some(WatchDir {
                dir: " /inbox ".into(),
                interval_secs: 1,
            }),
        },
        ..Default::default()
    };
    let watch = config.normalized().library.watch.expect("正经目录该留着");
    assert_eq!(watch.dir, "/inbox");
    assert_eq!(watch.interval_secs, 5);
}

#[test]
fn the_library_section_holds_its_dir() {
    let config = Config {
        library: Library {
            dir: "~/Books".into(),
            watch: None,
        },
        ..Default::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    config.save_to(&path).unwrap();
    assert_eq!(Config::load_from(&path).library.dir, config.library.dir);
}

/// 写文件是「临时文件 + rename」：中途失败不该留下半份坏 JSON。
/// 这里能验证的部分是目标目录不存在时会自动建出来。
#[test]
fn saving_creates_the_parent_directory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deep/nested/config.json");
    Config::default().save_to(&path).unwrap();
    assert!(path.exists());
}

/// 听书那一节：越界值夹回语义区间、缺节用默认（老配置文件没有这一节），
/// 关掉的 auto_scroll 原样读回。
#[test]
fn the_speech_section_clamps_and_falls_back_to_defaults() {
    let got = load(r#"{"speech":{"rate":9.0,"pitch":0.1,"auto_scroll":false}}"#);
    assert_eq!(
        got.speech.rate, 2.0,
        "语速上限是语义区间的 2.0，不是引擎档的 10.0"
    );
    assert_eq!(got.speech.pitch, 0.5);
    assert!(!got.speech.auto_scroll);

    let missing = load(r#"{"reader":{"font_size":21.0}}"#);
    assert_eq!(
        missing.speech,
        crate::Speech::default(),
        "老文件没有这一节，整节退默认"
    );

    // 整节类型不对（手滑写成字符串）：整节退默认，其余各节照旧。
    let broken = load(r#"{"speech":"快一点"}"#);
    assert_eq!(broken.speech, crate::Speech::default());
    assert!(broken.speech.auto_scroll, "退默认时自动翻页是开着的");
}

/// 老配置里根本没有 `membership` 这一节：按「从没开过」读回来，而不是当成坏文件。
/// 靠 `#[serde(default)]` 兜住，所以缺节与缺字段都不该牵连别的节。
#[test]
fn a_config_without_the_membership_section_reads_as_never_activated() {
    let got = load(r#"{}"#);
    assert_eq!(got.membership, Membership::default());
    assert_eq!(got.membership.until_day, 0);
    assert_eq!(got.membership.tier_on(20_500), MembershipTier::Standard);
    assert_eq!(
        got.language.locale,
        Locale::default(),
        "缺 membership 不该把别的节一起抹掉"
    );
    // 节在而字段缺，同样逐格兜住。
    assert_eq!(load(r#"{"membership":{}}"#).membership.until_day, 0);
    // 这一句是上面两条的**对照**：值认得回来，才说明 `load` 没有在 JSON 读不进时
    // 静默回落成默认值 —— 回落的样子与「从没开过」在屏上一模一样。
    assert_eq!(
        load(r#"{"membership":{"until_day":20500}}"#)
            .membership
            .until_day,
        20_500
    );
}

/// 到期日**含当天**：那一天还算会员，第二天就不算了。界面上那句「到期日」全靠这一条。
#[test]
fn the_expiry_day_still_counts_and_the_day_after_does_not() {
    let member = Membership {
        until_day: 20_500,
        ..Membership::default()
    };
    assert_eq!(member.tier_on(20_499), MembershipTier::Premium);
    assert_eq!(
        member.tier_on(20_500),
        MembershipTier::Premium,
        "最后一天仍算数"
    );
    assert_eq!(member.tier_on(20_501), MembershipTier::Standard);
    // 手改出来的负数不当「永远有效」：那只是过去的一天。
    assert_eq!(
        Membership {
            until_day: -5,
            ..Membership::default()
        }
        .tier_on(0),
        MembershipTier::Standard
    );
}
