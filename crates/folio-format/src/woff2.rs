//! WOFF2 → sfnt:[`woff2-patched`](https://crates.io/crates/woff2-patched) 的薄包装,与
//! [`super::woff`] 同一条街 —— `@font-face` 收到 WOFF2 时先在这里解回 sfnt,再走
//! 注册那条路。
//!
//! 为什么引依赖而不是手写:WOFF2 的载荷是 brotli 压缩加**表变换**——glyf/loca 的
//! 逐字形重排、hmtx 的比例/等宽压扁,重建规则写满 W3C 规范一整节;crate 是现成的
//! 移植,自己养那份代码不值。选 `woff2-patched` 而不是它 fork 的 `woff2` 0.3:后者
//! 按 2022 年的 rustc 写的,现在的编译器直接拒编(见本 crate `Cargo.toml` 那条);
//! 前者是同一 API 的修复分支。它要 brotli ^7,与锁里 hayro-cmap 的 8.x 并存两份
//! 主版本。
//!
//! 解出的是**整份 sfnt**(TTC 集合原样回来):子集化、注册、latin 判定都在
//! `folio_app` 那一侧,TTC 走「整份注册、不切子集」的既有口径。

use std::io::Cursor;

/// WOFF2 的魔数:小写 `wOF2`,与 WOFF 的 `wOFF` 一字之差。
const MAGIC: &[u8; 4] = b"wOF2";

/// 这份字节是 WOFF2 吗。清单里的 media-type 是声明,实际字节以它自己的魔数为准。
pub fn is_woff2(data: &[u8]) -> bool {
    data.len() >= 4 && &data[..4] == MAGIC
}

/// 解回 sfnt。头不认、flavor 不是 TrueType/CFF/集合、或变换重建失败都回 `None`
/// —— 调用方按「这份字体不存在」继续找下一份,与 WOFF 解不开同一条降级。
pub fn decode(data: &[u8]) -> Option<Vec<u8>> {
    if !is_woff2(data) {
        return None;
    }
    woff2_patched::convert_woff2_to_ttf(&mut Cursor::new(data)).ok()
}

#[cfg(test)]
mod tests {
    use super::{decode, is_woff2};
    use std::path::Path;

    fn fixture_ttf() -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/glyf-min.ttf"),
        )
        .expect("仓库里的合成 TTF")
    }

    fn fixture_woff2() -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/oss/fonts/glyf-min.woff2"),
        )
        .expect("ttf2woff2 切出来的 WOFF2")
    }

    #[test]
    fn only_the_magic_qualifies() {
        assert!(is_woff2(b"wOF2"));
        assert!(!is_woff2(b"wOFF"), "那是 WOFF,差一字");
        assert!(!is_woff2(b"\0\x01\0\0"), "sfnt 的魔数不是它");
        assert!(!is_woff2(b"wOF"), "短于魔数不认");
        assert!(!is_woff2(b""), "空字节不认");
    }

    #[test]
    fn garbage_decodes_to_none_without_panicking() {
        let mut junk = b"wOF2".to_vec();
        junk.extend(std::iter::repeat_n(0u8, 64));
        assert_eq!(decode(&junk), None, "魔数对上、正文是垃圾:解不开回 None");
        assert_eq!(decode(b"\0\x01\0\0not a font at all"), None);
        assert_eq!(decode(b""), None);
    }

    /// 真实往返:fixture 是 ttf2woff2 从 `glyf-min.ttf` 切出来的,解回后与**原字体**
    /// 逐项对 —— 字形数、name 表、cmap 全表。glyf/loca 在 WOFF2 里是按规范变换过的,
    /// 字节不是逐位照抄,比对必须落在解析结果上;字符集取原字体的 cmap 全集,
    /// 一个映射都不漏。
    #[test]
    fn the_fixture_decodes_back_to_the_original_font() {
        let packed = fixture_woff2();
        assert!(is_woff2(&packed));
        let decoded = decode(&packed).expect("fixture 该解得回 sfnt");
        assert_eq!(
            &decoded[..4],
            &[0, 0x01, 0, 0],
            "glyf-min 是 TrueType,flavor 原样回来"
        );

        let original = fixture_ttf();
        let a = ttf_parser::Face::parse(&original, 0).expect("原字体可解析");
        let b = ttf_parser::Face::parse(&decoded, 0).expect("解出的 sfnt 可解析");
        assert_eq!(a.number_of_glyphs(), b.number_of_glyphs(), "字形数一致");
        assert_eq!(
            a.names().into_iter().count(),
            b.names().into_iter().count(),
            "name 表记录数一致"
        );

        let cmap = a.tables().cmap.expect("原字体有 cmap");
        let mut codepoints: Vec<u32> = Vec::new();
        for st in cmap.subtables.into_iter() {
            st.codepoints(|cp| codepoints.push(cp));
        }
        let mut checked = 0usize;
        for cp in codepoints {
            let Some(ch) = char::from_u32(cp) else {
                continue;
            };
            assert_eq!(
                a.glyph_index(ch),
                b.glyph_index(ch),
                "{ch:?} 的字形号在解出的 sfnt 里要一个不差"
            );
            checked += 1;
        }
        assert!(checked > 0, "cmap 全空说明 fixture 选错了");
    }
}
