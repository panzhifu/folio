//! 低干扰阅读的全局偏好。
//!
//! 这一节只描述读者想要的样子，不描述窗口此刻的状态：后者属于 app 的服务层，
//! 重启后要重新按平台能力求值。字段走 `serde(default)`，老配置文件缺整节时也读得出来。

use serde::{Deserialize, Serialize};

/// 低干扰显示形态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StealthMode {
    /// 保持今天的阅读界面。
    #[default]
    Off,
    /// 保留自研排版，只收起顶栏底栏等辅助界面，并让窗口背景透明。
    Quiet,
}

impl StealthMode {
    /// 设置界面与配置文件共用的值，按界面顺序。
    pub const ALL: &'static [Self] = &[Self::Off, Self::Quiet];

    /// 配置文件与设置界面用的键名。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Quiet => "quiet",
        }
    }

    /// 认不出的值回落 `Off`：手改坏一个键不该让整节 `Stealth` 退回默认。
    pub fn parse(value: &str) -> Self {
        match value {
            "quiet" => Self::Quiet,
            _ => Self::Off,
        }
    }
}

/// 低干扰阅读偏好。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stealth {
    /// 默认进入低干扰形态。当前只影响运行时初值，不自动接管尚未打开的书。
    pub enabled: bool,
    /// 读者选的形态。
    pub mode: StealthMode,
    /// 窗口内容透明度，0.25–1.0。
    pub opacity: f32,
    /// 请求窗口置顶。平台不支持时只保留偏好，静默降级。
    pub always_on_top: bool,
    /// 老板键用最小化隐藏；不支持原生无痕隐藏的平台走这条。
    pub hide_by_minimize: bool,
    /// 应用内切换快捷键。全局热键尚未实现，不把不可承诺的能力写进默认值。
    pub toggle_key: String,
}

impl Default for Stealth {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: StealthMode::Off,
            opacity: 0.92,
            always_on_top: false,
            hide_by_minimize: true,
            toggle_key: "cmd-alt-s".into(),
        }
    }
}

impl Stealth {
    /// 透明度的下限：再低正文就失去可读性，低干扰不该等于看不见。
    pub const OPACITY_MIN: f32 = 0.25;
    /// 透明度上限：1.0 就是不透明，与今天的窗口行为一致。
    pub const OPACITY_MAX: f32 = 1.0;

    /// 夹住越界值。NaN 与非法字符串交给 serde 的缺省/调用方校验，不在这里扩散。
    pub fn normalized(mut self) -> Self {
        if !self.opacity.is_finite() {
            self.opacity = Self::default().opacity;
        }
        self.opacity = self.opacity.clamp(Self::OPACITY_MIN, Self::OPACITY_MAX);
        self.toggle_key = self.toggle_key.trim().to_string();
        if self.toggle_key.is_empty() {
            self.toggle_key = Self::default().toggle_key;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{Stealth, StealthMode};

    /// 老配置没有这一节时走默认值：不能因此判整份配置损坏。
    #[test]
    fn a_missing_section_is_off() {
        let stealth: Stealth = serde_json::from_str("{}").unwrap();
        assert_eq!(stealth.mode, StealthMode::Off);
        assert!(!stealth.enabled);
    }

    #[test]
    fn opacity_and_toggle_key_are_normalized() {
        let stealth = Stealth {
            opacity: 12.0,
            toggle_key: "  ".into(),
            ..Stealth::default()
        }
        .normalized();
        assert_eq!(stealth.opacity, Stealth::OPACITY_MAX);
        assert!(!stealth.toggle_key.is_empty());
    }

    #[test]
    fn mode_values_round_trip_through_the_settings_keys() {
        for mode in StealthMode::ALL {
            assert_eq!(StealthMode::parse(mode.as_str()), *mode);
        }
        assert_eq!(StealthMode::parse("unknown"), StealthMode::Off);
    }

    #[test]
    fn the_section_round_trips() {
        let stealth = Stealth {
            enabled: true,
            mode: StealthMode::Quiet,
            opacity: 0.5,
            ..Stealth::default()
        };
        let back: Stealth =
            serde_json::from_str(&serde_json::to_string(&stealth).unwrap()).unwrap();
        assert_eq!(back, stealth);
    }
}
