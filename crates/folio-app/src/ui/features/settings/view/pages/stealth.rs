//! 「低干扰」页：只在阅读界面里收起辅助层、把纸面调淡。
//!
//! 这一页只写配置；窗口透明、最小化与恢复由 `services::stealth` 按平台能力兑现。
//! 快捷键这里只显示当前值：运行时重绑需要替换整张键位表，当前实现仍用固定键位，
//! 不摆一个「看着能改、其实没生效」的输入框。

use super::super::*;
use folio_core::StealthMode;

pub(in crate::ui::features::settings) fn stealth_page(cx: &App) -> Page {
    page(
        tr(cx, "page.stealth"),
        IconName::EyeOff,
        vec![
            group(
                tr(cx, "group.stealth_mode"),
                vec![
                    Row::toggle(
                        tr(cx, "field.stealth_enabled"),
                        |cx: &App| ConfigHolder::stealth(cx).enabled,
                        |val, cx| ConfigHolder::edit_stealth(cx, |stealth| stealth.enabled = val),
                    )
                    .description(tr(cx, "field.stealth_enabled.desc"))
                    .keywords(&["stealth", "quiet", "低干扰", "摸鱼", "开关"]),
                    Row::dropdown(
                        tr(cx, "field.stealth_mode"),
                        StealthMode::ALL
                            .iter()
                            .copied()
                            .map(|mode| {
                                (
                                    SharedString::from(mode.as_str()),
                                    tr(cx, &format!("stealth_mode.{}", mode.as_str())),
                                )
                            })
                            .collect(),
                        false,
                        |cx: &App| SharedString::from(ConfigHolder::stealth(cx).mode.as_str()),
                        |val: SharedString, cx| {
                            ConfigHolder::edit_stealth(cx, |stealth| {
                                stealth.mode = StealthMode::parse(&val)
                            })
                        },
                    )
                    .description(tr(cx, "field.stealth_mode.desc"))
                    .keywords(&["mode", "quiet", "模式", "低干扰"]),
                ],
            ),
            group(
                tr(cx, "group.stealth_appearance"),
                vec![number(
                    cx,
                    NumberSpec {
                        title: "field.stealth_opacity",
                        description: "field.stealth_opacity.desc",
                        keywords: &["opacity", "alpha", "透明", "淡", "低干扰"],
                        min: folio_core::Stealth::OPACITY_MIN,
                        max: folio_core::Stealth::OPACITY_MAX,
                        step: 0.05,
                    },
                    |cx: &App| ConfigHolder::stealth(cx).opacity,
                    |val, cx| ConfigHolder::edit_stealth(cx, |stealth| stealth.opacity = val),
                )],
            ),
            group(
                tr(cx, "group.stealth_keys"),
                vec![
                    Row::info(
                        tr(cx, "field.stealth_toggle_key"),
                        SharedString::from(ConfigHolder::stealth(cx).toggle_key.clone()),
                    )
                    .description(tr(cx, "field.stealth_toggle_key.desc"))
                    .keywords(&["key", "shortcut", "快捷键", "切换"]),
                    Row::info(tr(cx, "field.stealth_hide_key"), "cmd-alt-h / ctrl-alt-h")
                        .description(tr(cx, "field.stealth_hide_key.desc"))
                        .keywords(&["boss", "key", "快捷键", "老板键", "隐藏"]),
                ],
            ),
        ],
    )
}
