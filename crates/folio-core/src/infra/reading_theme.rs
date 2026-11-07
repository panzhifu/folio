//! 正文主题：阅读界面（纸底 / 正文 / 链接）的配色，与 UI 壳的主题**分离**。
//!
//! 壳（导航、设置、弹层）的配色走 gpui-kit 的 `Theme` Global，读者在设置里挑的是
//! 一整本主题；正文纸色是另一回事 —— 想要「羊皮纸的正文 + 白色的设置页」时，换壳
//! 主题不该牵着纸一起变。所以正文配色自己立一份**纯数据**的模型：这里只有类型、
//! 内置色表和亮度换算，不认识 GPUI，也没有「选哪套」的状态（那一格在
//! [`crate::infra::config::Appearance::reading_theme`]，按书差量与全局默认各管各的）。
//!
//! 每套主题**日 / 夜配对**（readest 的做法）：读者点「夜间」翻的是同一套主题的深侧，
//! 而不是跳出这套主题另选一套深色 —— 配色是一个整体，日夜各给一份，翻面不散架。
//! 水墨那一档依旧不是一套主题：它取当前主题的**浅侧**再整体去饱和（与壳那条同一
//! 口径），所以这里不存第三份颜色。
//!
//! 链接色是表里的一格而不是运行时派生：书面 CSS 十有八九不给链接上色，那一格只能
//! 由阅读器给（`body.rs` 取色处的注释）；哪一格是「点得出来的」与纸色强相关，米黄
//! 纸配青蓝、黑纸配琥珀，各侧各写，不在代码里算。

/// 一格颜色：RGBA，各 0–255。
///
/// 自立一份而不借用引擎的 `css::computed::Color`：这一区（`infra`）服务所有业务线，
/// 不反过来依赖排版那侧的类型。字段摊开而不是数组：表里的色值一眼读得出通道。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba8 {
    /// 不透明的一格。
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// 感知亮度（Rec. 709 系数）压成的灰，alpha 原样留着。
    ///
    /// 与壳那边 `theme.rs::desaturate_hex`、`style.rs::desaturate` 同一把尺：水墨档
    /// 只拿掉色相，不把深底变成浅底、也不让浅字在灰底上失掉对比。
    pub fn luma(self) -> Self {
        let gray =
            (0.2126 * f32::from(self.r) + 0.7152 * f32::from(self.g) + 0.0722 * f32::from(self.b))
                .round() as u8;
        Self {
            r: gray,
            g: gray,
            b: gray,
            a: self.a,
        }
    }
}

/// 一侧（日或夜）的正文配色：纸底、正文墨色、链接。
///
/// 页眉页脚那些**淡墨**不进表：运行时把 `fg` 压一层 alpha 就是它（底是同一张纸，
/// alpha 混出来的灰阶永远协调），多一格表反而多一处两格不同步的机会。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadingColors {
    pub bg: Rgba8,
    pub fg: Rgba8,
    pub link: Rgba8,
}

/// 一套正文主题：名字 + 日夜两侧。名字是配置与按书差量里存的那一格，稳定不译；
/// 界面显示名走文案表（`locales/*.toml` 的 `reading_theme.<name>`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadingTheme {
    pub name: &'static str,
    pub light: ReadingColors,
    pub dark: ReadingColors,
}

/// 内置正文主题。色值参照 readest 的成套实测（日夜两侧各是一体的，别单格调）；
/// 墨夜深侧取 anx-reader 的夜读色：纯黑底、米黄字。
///
/// 表的顺序就是选择器里的顺序：纸白在前当默认档，随后纸色由浅入深，深色收尾。
pub const BUILT_IN: &[ReadingTheme] = &[
    ReadingTheme {
        name: "paper",
        light: ReadingColors {
            bg: Rgba8::rgb(0xff, 0xff, 0xff),
            fg: Rgba8::rgb(0x17, 0x17, 0x17),
            link: Rgba8::rgb(0x00, 0x66, 0xcc),
        },
        dark: ReadingColors {
            bg: Rgba8::rgb(0x22, 0x22, 0x22),
            fg: Rgba8::rgb(0xe0, 0xe0, 0xe0),
            link: Rgba8::rgb(0x77, 0xbb, 0xee),
        },
    },
    ReadingTheme {
        name: "sepia",
        light: ReadingColors {
            bg: Rgba8::rgb(0xf1, 0xe8, 0xd0),
            fg: Rgba8::rgb(0x5b, 0x46, 0x36),
            link: Rgba8::rgb(0x00, 0x8b, 0x8b),
        },
        dark: ReadingColors {
            bg: Rgba8::rgb(0x34, 0x2e, 0x25),
            fg: Rgba8::rgb(0xff, 0xd5, 0x95),
            link: Rgba8::rgb(0x48, 0xd1, 0xcc),
        },
    },
    ReadingTheme {
        name: "gray",
        light: ReadingColors {
            bg: Rgba8::rgb(0xe0, 0xe0, 0xe0),
            fg: Rgba8::rgb(0x22, 0x22, 0x22),
            link: Rgba8::rgb(0x44, 0x88, 0xcc),
        },
        dark: ReadingColors {
            bg: Rgba8::rgb(0x44, 0x44, 0x44),
            fg: Rgba8::rgb(0xc6, 0xc6, 0xc6),
            link: Rgba8::rgb(0x88, 0xcc, 0xee),
        },
    },
    ReadingTheme {
        name: "grass",
        light: ReadingColors {
            bg: Rgba8::rgb(0xd7, 0xdb, 0xbd),
            fg: Rgba8::rgb(0x23, 0x2c, 0x16),
            link: Rgba8::rgb(0x17, 0x7b, 0x4d),
        },
        dark: ReadingColors {
            bg: Rgba8::rgb(0x33, 0x36, 0x27),
            fg: Rgba8::rgb(0xd8, 0xde, 0xba),
            link: Rgba8::rgb(0xa6, 0xd6, 0x08),
        },
    },
    ReadingTheme {
        name: "sky",
        light: ReadingColors {
            bg: Rgba8::rgb(0xce, 0xde, 0xf5),
            fg: Rgba8::rgb(0x26, 0x2d, 0x48),
            link: Rgba8::rgb(0x2d, 0x53, 0xe5),
        },
        dark: ReadingColors {
            bg: Rgba8::rgb(0x28, 0x2e, 0x47),
            fg: Rgba8::rgb(0xba, 0xbe, 0xe1),
            link: Rgba8::rgb(0x88, 0xcc, 0xee),
        },
    },
    ReadingTheme {
        name: "night",
        light: ReadingColors {
            bg: Rgba8::rgb(0xff, 0xff, 0xff),
            fg: Rgba8::rgb(0x17, 0x17, 0x17),
            link: Rgba8::rgb(0x00, 0x66, 0xcc),
        },
        dark: ReadingColors {
            bg: Rgba8::rgb(0x00, 0x00, 0x00),
            fg: Rgba8::rgb(0xfe, 0xff, 0xeb),
            link: Rgba8::rgb(0xc9, 0xa8, 0x6a),
        },
    },
    ReadingTheme {
        name: "gruvbox",
        light: ReadingColors {
            bg: Rgba8::rgb(0xfb, 0xf1, 0xc7),
            fg: Rgba8::rgb(0x3c, 0x38, 0x36),
            link: Rgba8::rgb(0x07, 0x66, 0x78),
        },
        dark: ReadingColors {
            bg: Rgba8::rgb(0x28, 0x28, 0x28),
            fg: Rgba8::rgb(0xeb, 0xdb, 0xb2),
            link: Rgba8::rgb(0x83, 0xa5, 0x98),
        },
    },
    ReadingTheme {
        name: "nord",
        light: ReadingColors {
            bg: Rgba8::rgb(0xec, 0xef, 0xf4),
            fg: Rgba8::rgb(0x2e, 0x34, 0x40),
            link: Rgba8::rgb(0x5e, 0x81, 0xac),
        },
        dark: ReadingColors {
            bg: Rgba8::rgb(0x2e, 0x34, 0x40),
            fg: Rgba8::rgb(0xd8, 0xde, 0xe9),
            link: Rgba8::rgb(0x88, 0xc0, 0xd0),
        },
    },
];

/// 按配置里的名字查一套内置主题。查不到回 `None`：手改坏的名字回落「跟随软件」，
/// 与壳那侧主题名认不出时回落框架默认是同一条思路 —— 让偏好继续管用比让界面整齐要紧。
pub fn find(name: &str) -> Option<&'static ReadingTheme> {
    BUILT_IN.iter().find(|theme| theme.name == name)
}

#[cfg(test)]
mod tests {
    use crate::infra::reading_theme::{BUILT_IN, Rgba8, find};

    fn luma_of(color: Rgba8) -> f32 {
        0.2126 * f32::from(color.r) + 0.7152 * f32::from(color.g) + 0.0722 * f32::from(color.b)
    }

    /// 配置里存的名字必须查得到：改名或挪表时这里先红，不会静默回落「跟随软件」。
    #[test]
    fn every_built_in_name_is_findable() {
        for theme in BUILT_IN {
            assert_eq!(find(theme.name).map(|found| found.name), Some(theme.name));
        }
        assert_eq!(find("没有这套"), None);
        assert_eq!(find(""), None);
    }

    /// 日夜配对的第一条底线：每套主题浅侧纸比深侧纸亮。反了的那套会在「点夜间」时
    /// 让纸变亮、字变暗，读者读到的是「翻面失败」。
    #[test]
    fn every_theme_has_a_lighter_light_side_than_dark_side() {
        for theme in BUILT_IN {
            assert!(
                luma_of(theme.light.bg) > luma_of(theme.dark.bg),
                "主题 {} 的浅侧纸不比深侧亮",
                theme.name
            );
            assert!(
                luma_of(theme.light.fg) < luma_of(theme.dark.fg),
                "主题 {} 的浅侧墨不比深侧深",
                theme.name
            );
        }
    }

    /// 纸与墨的对比两边都得够读：单侧再好看，压不出对比就不是阅读配色。
    #[test]
    fn every_side_keeps_readable_contrast() {
        for theme in BUILT_IN {
            for (side, colors) in [("浅", &theme.light), ("深", &theme.dark)] {
                let gap = luma_of(colors.bg) - luma_of(colors.fg);
                assert!(
                    gap.abs() > 80.0,
                    "主题 {} 的{}侧纸墨对比只剩 {gap}",
                    theme.name,
                    side
                );
            }
        }
    }

    /// 亮度只拿色相，不动明暗：黑白两端与纯灰原样过。
    #[test]
    fn luma_keeps_brightness_and_drops_hue() {
        assert_eq!(Rgba8::rgb(0, 0, 0).luma(), Rgba8::rgb(0, 0, 0));
        assert_eq!(Rgba8::rgb(255, 255, 255).luma(), Rgba8::rgb(255, 255, 255));
        assert_eq!(
            Rgba8::rgb(0x80, 0x80, 0x80).luma(),
            Rgba8::rgb(0x80, 0x80, 0x80)
        );
        // 纯红过尺 = Rec. 709 的红系数，四舍五入到 54。
        assert_eq!(Rgba8::rgb(255, 0, 0).luma(), Rgba8::rgb(54, 54, 54));
        // alpha 是透明度不是亮度，原样留着。
        assert_eq!(
            Rgba8 {
                r: 255,
                g: 0,
                b: 0,
                a: 128
            }
            .luma()
            .a,
            128
        );
    }
}
