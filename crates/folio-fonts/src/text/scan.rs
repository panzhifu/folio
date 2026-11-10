//! 系统字体目录枚举的**策略**：哪些文件算字体、哪一份注册不注册。
//!
//! 2026-11-09 从 `folio_app::ui::platform::system_fonts` 搬来 —— 判据本身是纯的
//! （一个扩展名表、两条排序规则），只有「注册」那一步要 `TextSystem`，留在 app。
//! 目录清单（`/system/fonts` 等）与批大小是**平台**参数，也留在 app：鸿蒙与桌面的
//! 路径不一样，塞进 core 就得给 cfg 开洞。

use std::collections::HashSet;
use std::path::Path;

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

/// 一份字体**注册不注册**。判据按「最想写进日志的原因」排先后：
/// emoji 先（撞上 COLRv1 与 CBDT 两份同名脸的事故），已注册次之，其余保留。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanVerdict {
    Keep,
    AlreadyRegistered,
    Emoji,
}

/// 这一台设备上已经注册着的家族名，与解析出的这份字节的全部家族名，一起判定。
///
/// fontdb 报的名字可能是本地化那一条，所以解析出的**任何一个**名字命中都算注册过。
pub fn scan_verdict(families: &[&str], known: &HashSet<String>) -> ScanVerdict {
    if families
        .iter()
        .any(|name| name.to_ascii_lowercase().contains("emoji"))
    {
        return ScanVerdict::Emoji;
    }
    if families.iter().any(|name| known.contains(*name)) {
        return ScanVerdict::AlreadyRegistered;
    }
    ScanVerdict::Keep
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;

    use super::{ScanVerdict, is_font_file, scan_verdict};

    fn known(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
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

    /// emoji 名字优先于「已注册」判：已经从 APK 注册过 CBDT 版的设备上，系统的
    /// COLRv1 版两判都命中，日志里要看到的是「为什么跳」里最具体的那一个。
    #[test]
    fn an_emoji_family_is_skipped_even_when_already_registered() {
        assert!(matches!(
            scan_verdict(&["Noto Color Emoji"], &known(&["Noto Color Emoji"])),
            ScanVerdict::Emoji
        ));
        assert!(matches!(
            scan_verdict(&["HMOSColorEmojiCompat"], &known(&[])),
            ScanVerdict::Emoji
        ));
    }

    /// fork 白名单里那批文件二次扫描时必须全部命中「已注册」——19MB 的 CJK ttc
    /// 不能因为换个判法就驻留两份。
    #[test]
    fn a_family_already_on_the_device_is_skipped() {
        assert!(matches!(
            scan_verdict(&["Roboto", "Roboto Condensed"], &known(&["Roboto"])),
            ScanVerdict::AlreadyRegistered
        ));
        assert!(matches!(
            // fontdb 报回来的可能是本地化那一条，任何语言的命中都算。
            scan_verdict(&["小米兰亭 Pro"], &known(&["小米兰亭 Pro"])),
            ScanVerdict::AlreadyRegistered
        ));
        assert!(matches!(
            scan_verdict(&["Some Vendor Serif"], &known(&["Roboto", "Noto Sans"])),
            ScanVerdict::Keep
        ));
    }
}
