//! 系统字体目录枚举的**策略**：哪些文件算字体、哪一份本轮要注册。
//!
//! 2026-11-09 从 `folio_app::ui::platform::system_fonts` 搬来 —— 判据本身是纯的
//! （一个扩展名表、两条排序规则），只有「注册」那一步要 `TextSystem`，留在 app。
//! 目录清单（`/system/fonts` 等）与批大小是**平台**参数，也留在 app：鸿蒙与桌面的
//! 路径不一样，塞进 core 就得给 cfg 开洞。
//!
//! 判重分两层，各有各的归属：
//!
//! - **跨源**（fork 白名单注册过的、上一轮已注册的路径）由 fork 的
//!   `add_font_files` 按路径守卫 —— `Source::File` 自带路径，注册前查一遍现库，
//!   已在的只回名字不再灌。名字比对干不了这件事：fontdb 那边每张脸只报第一个
//!   家族名，而 name 表的本地化名是全集，两边口径永远对不齐。
//! - **本轮之内**（同一字体躺在 `/system/fonts` 与 `/product/fonts` 两个目录、或
//!   一份拆成多张脸）由 [`scan_verdict`] 按名字挡 —— 解析出的任何一个名字在
//!   本轮已见过就算重复，比较按 [`normalize_family`](crate::text::alias::normalize_family)
//!   归一，大小写与连字符不构成两份字体。
//!
//! emoji 家族永远跳过：fork 已从 APK 注册了 CBDT 版，再注册系统的 COLRv1 版，
//! 两个同名脸会让 gpui resolve 到画不出的那一份。

use std::collections::HashSet;
use std::path::Path;

use crate::text::alias::normalize_family;

/// 只认这三种扩展名。fontdb 解析不了的反正会被 [`latin_by_family`](crate::text::names::latin_by_family)
/// 拦下，这里只是省得把目录里偶见的杂项也读进来。
pub const FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "ttc"];

/// 这个文件按扩展名算不算字体。大小写不敏感。
pub fn is_font_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());
    matches!(ext.as_deref(), Some(ext) if FONT_EXTENSIONS.contains(&ext))
}

/// 一份字体**本轮注册不注册**。判据按「最想写进日志的原因」排先后：
/// emoji 先（撞上 COLRv1 与 CBDT 两份同名脸的事故），本轮重复次之，其余保留。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanVerdict {
    Keep,
    /// 本轮扫描已经见过这批名字：同一字体在两个目录、或一份拆成多文件。
    Duplicate,
    Emoji,
}

/// 解析出的这份字节的全部家族名，与**本轮已见**的名字集合，一起判定。
pub fn scan_verdict(families: &[&str], seen: &HashSet<String>) -> ScanVerdict {
    if families
        .iter()
        .any(|name| name.to_ascii_lowercase().contains("emoji"))
    {
        return ScanVerdict::Emoji;
    }
    if families
        .iter()
        .any(|name| seen.contains(&normalize_family(name)))
    {
        return ScanVerdict::Duplicate;
    }
    ScanVerdict::Keep
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;

    use super::{ScanVerdict, is_font_file, scan_verdict};
    use crate::text::alias::normalize_family;

    fn seen(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| normalize_family(name)).collect()
    }

    /// 只有三类扩展名过闸，大小写不论；其余（xml、无扩展名）不读。
    #[test]
    fn only_font_extensions_pass_the_gate() {
        assert!(is_font_file(Path::new("/system/fonts/Roboto-Regular.ttf")));
        assert!(is_font_file(Path::new(
            "/system/fonts/NotoSansCJK-Regular.TTC"
        )));
        assert!(is_font_file(Path::new("/product/fonts/MiSans.otf")));
        assert!(!is_font_file(Path::new("/system/fonts/fonts.xml")));
        assert!(!is_font_file(Path::new("/system/fonts/README")));
    }

    /// emoji 名字优先于「本轮重复」判：日志里要看到的是「为什么跳」里最具体的那一个。
    #[test]
    fn an_emoji_family_is_skipped_even_when_seen() {
        assert!(matches!(
            scan_verdict(&["Noto Color Emoji"], &seen(&["Noto Color Emoji"])),
            ScanVerdict::Emoji
        ));
        assert!(matches!(
            scan_verdict(&["HMOSColorEmojiCompat"], &seen(&[])),
            ScanVerdict::Emoji
        ));
    }

    /// 本轮去重按归一口径比：同一字体在两个目录、或大小写/连字符写法不同，都算
    /// 重复；名字集合里的任何语言命中都算（fontdb 报的可能是本地化那条）。
    #[test]
    fn a_family_seen_this_round_is_a_duplicate() {
        assert!(matches!(
            scan_verdict(&["Roboto", "Roboto Condensed"], &seen(&["Roboto"])),
            ScanVerdict::Duplicate
        ));
        assert!(matches!(
            scan_verdict(&["小米兰亭 Pro"], &seen(&["小米兰亭 Pro"])),
            ScanVerdict::Duplicate
        ));
        assert!(matches!(
            scan_verdict(&["Mi-Sans"], &seen(&["MiSans"])),
            ScanVerdict::Duplicate,
        ));
        assert!(matches!(
            scan_verdict(&["Some Vendor Serif"], &seen(&["Roboto", "Noto Sans"])),
            ScanVerdict::Keep
        ));
    }
}
