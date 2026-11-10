//! 字体字节的名字表解析：一份 ttf/otf/ttc 里有哪些家族名、各带不带拉丁 `m`。
//!
//! 纯字节解析，不碰文件系统、不碰 `TextSystem` —— 消费方有两处：
//! `text::load`（随书注册后按家族名记「能不能当 primary」）与 app 侧的系统字体
//! 全量扫描（`platform::system_fonts`，判「这份字体是不是已经注册过」）。
//! 2026-11-09 从 `folio_app::ui::foundation::font_names` 搬来：它谁也不认识，
//! 而两处消费方都在问同一个问题。

use std::collections::HashMap;

/// 一份字体字节里每个家族名带不带拉丁 `m`。
///
/// 名字按 name 表 1（family）与 16（typographic family）的记录找，**任何语言**都算 ——
/// fontdb 报回来的可能是本地化那一条。一条都对不上时退回「这份字体里有没有任何一张脸
/// 带 m」：比对失败不该把作者带来的字体整个丢掉，而单脸的字体（真书里的常态）两种问法
/// 是同一个答案。
pub struct LatinCoverage {
    by_family: HashMap<String, bool>,
    any: bool,
}

impl LatinCoverage {
    pub fn covers(&self, family: &str) -> bool {
        self.by_family.get(family).copied().unwrap_or(self.any)
    }

    /// 这份字体里有没有**任何一张**脸带拉丁 `m`。
    pub fn any(&self) -> bool {
        self.any
    }

    /// 解析出的全部家族名（name 表 1/16 的记录，任何语言都算）。
    ///
    /// 扫描用它判「已注册则跳过」——fontdb 报回来的名字可能是本地化那一条，所以
    /// 这个集合里的任何一个命中都算注册过。
    pub fn families(&self) -> impl Iterator<Item = &str> {
        self.by_family.keys().map(String::as_str)
    }
}

/// 解析一份字体字节。解析不出来的脸（坏表、不认识的轮廓格式）逐张跳过；
/// 一张都解析不出时返回空覆盖，调用方据此整份放弃。
pub fn latin_by_family(bytes: &[u8]) -> LatinCoverage {
    let mut coverage = LatinCoverage {
        by_family: HashMap::new(),
        any: false,
    };
    let count = ttf_parser::fonts_in_collection(bytes).unwrap_or(1);
    for index in 0..count {
        let Ok(face) = ttf_parser::Face::parse(bytes, index) else {
            continue;
        };
        let has_m = face.glyph_index('m').is_some();
        coverage.any |= has_m;
        for name in face.names() {
            if !matches!(name.name_id, 1 | 16) {
                continue;
            }
            let Some(text) = name.to_string() else {
                continue;
            };
            coverage
                .by_family
                .entry(text)
                .and_modify(|known| *known |= has_m)
                .or_insert(has_m);
        }
    }
    coverage
}

#[cfg(test)]
mod tests {
    use super::latin_by_family;

    /// 解析不出来的字节（不是字体）：覆盖为空，`covers` 落到 `any = false`，
    /// `families` 一个名字都没有。真字节的覆盖走 `book_fonts` 的测试。
    #[test]
    fn unparseable_bytes_yield_an_empty_coverage() {
        let coverage = latin_by_family(b"not a font file");
        assert!(!coverage.covers("Anything"));
        assert_eq!(coverage.families().count(), 0);
    }
}
