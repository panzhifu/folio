use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeName {
    #[default]
    Light,
    Dark,
    Sepia,
}

/// 阅读偏好。全部是「排版输入」，不含任何 UI 类型，
/// 因此同一份 JSON 既能在桌面端用，也能给将来的移动端宿主用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub font_size: f32,
    pub line_height: f32,
    pub paragraph_gap_em: f32,
    pub indent_em: f32,
    pub max_measure: f32,
    pub margin_x: f32,
    pub justify: bool,
    pub theme: ThemeName,
    pub font_family: String,
    pub mono_font_family: String,
    pub library_dirs: Vec<String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            font_size: 17.0,
            line_height: 1.75,
            paragraph_gap_em: 0.75,
            indent_em: 2.0,
            max_measure: 760.0,
            margin_x: 48.0,
            justify: true,
            theme: ThemeName::Light,
            font_family: "LXGW WenKai".into(),
            mono_font_family: "monospace".into(),
            library_dirs: Vec::new(),
        }
    }
}

impl Prefs {
    /// 从磁盘读回来的值不可信（手改过、旧版本、别的机器同步来的），
    /// 越界值会让排版算出零高或无限宽，所以入库前一律夹住。
    pub fn clamped(mut self) -> Self {
        self.font_size = self.font_size.clamp(9.0, 72.0);
        self.line_height = self.line_height.clamp(1.0, 4.0);
        self.paragraph_gap_em = self.paragraph_gap_em.clamp(0.0, 4.0);
        self.indent_em = self.indent_em.clamp(0.0, 6.0);
        self.max_measure = self.max_measure.clamp(280.0, 2400.0);
        self.margin_x = self.margin_x.clamp(0.0, 400.0);
        if self.font_family.trim().is_empty() {
            self.font_family = Prefs::default().font_family;
        }
        self
    }

    pub fn from_json(json: &str) -> Self {
        serde_json::from_str::<Self>(json).unwrap_or_default().clamped()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_json() {
        let p = Prefs {
            theme: ThemeName::Sepia,
            justify: false,
            ..Default::default()
        };
        assert_eq!(Prefs::from_json(&p.to_json()), p);
    }

    #[test]
    fn missing_keys_fall_back_to_defaults() {
        let p = Prefs::from_json(r#"{"font_size":21.0,"theme":"dark"}"#);
        assert_eq!(p.font_size, 21.0);
        assert_eq!(p.theme, ThemeName::Dark);
        assert_eq!(p.line_height, 1.75);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let p = Prefs::from_json(r#"{"font_size":9000,"line_height":0,"indent_em":-5,"margin_x":1e9}"#);
        assert_eq!(p.font_size, 72.0);
        assert_eq!(p.line_height, 1.0);
        assert_eq!(p.indent_em, 0.0);
        assert_eq!(p.margin_x, 400.0);
    }

    #[test]
    fn garbage_json_does_not_panic() {
        assert_eq!(Prefs::from_json("not json at all"), Prefs::default());
        assert_eq!(Prefs::from_json(""), Prefs::default());
    }
}
