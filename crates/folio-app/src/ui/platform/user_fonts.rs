//! 读者自己装的字体（下载 / 导入）：开机重注册进文本系统，与装完一份后的当场注册。
//!
//! 为什么开机要**重**注册：字体注册是进程级的（gpui 的 fontdb 不落盘），读者昨天装的
//! 字体今天重启就没了。配置里记着 `<fonts 根>/<id>/<file>`（真相源在 config，盘上文件
//! 只是它的附件 —— 与词典同一条），开机照着清单把字节再灌一遍。与 [`system_fonts`] 的
//! 差别就在真相源：那份扫**系统目录**，盘上有什么注册什么；这份只认配置里点过名的，
//! 删掉的 id 不会再灌。
//!
//! 失败的处置是「跳过，别拦启动」：一份解不开的字体只少一个家族，不能少一次开机。
//! 失败的份数进日志，设置页的已装列表按配置画 —— 读者能看到它还在列表里但正文排
//! 不出来，删掉重装是现成的出路。
//!
//! woff / woff2 按魔数先解回 sfnt（[`woff`] / [`woff2`]，与随书字体同一脚），ttf / otf /
//! ttc 原样递；都不是的当坏文件跳过。注册成功后的两记失效（`book_fonts` 的字体表
//! 版本计数与 [`font_primary::families_revision`]）：开机早于任何排版，bump 无成本；
//! 会话内装完一份时书可能正开着，行宽是按旧字体表算的，**必须** bump。
//!
//! [`woff`]: folio_core::content::format::woff
//! [`woff2`]: folio_core::content::format::woff2

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use folio_core::content::format::{woff, woff2};
use gpui_kit::{App, AppContext as _, TextSystem};

use crate::ui::features::reader::note_external_registration;
use crate::ui::foundation::font_names::{LatinCoverage, latin_by_family};
use crate::ui::foundation::font_primary;
use crate::ui::services::config::ConfigHolder;

/// 开机把配置里记着的自装字体再注册一遍。在 `AppShell::boot` 里调，紧挨
/// [`system_fonts::load_all_system_fonts`]；重活在 `background_spawn` 上跑，
/// 完成有注册成功就刷新窗口，让已经建好的字体选择器重取名单。
pub(crate) fn load_user_fonts(cx: &mut App) {
    // 清单在界面线程上收齐（配置是主线程的 Global），字节读写与注册在后台。
    let installed: Vec<PathBuf> = ConfigHolder::fonts(cx)
        .installed
        .iter()
        .filter(|entry| !entry.file.is_empty())
        .map(|entry| folio_core::fonts_dir().join(&entry.id).join(&entry.file))
        .collect();
    if installed.is_empty() {
        return;
    }
    let texts: Arc<TextSystem> = cx.text_system().clone();
    let work = cx.background_spawn(async move { register_all(&texts, &installed) });
    cx.spawn(async move |cx| {
        let summary = work.await;
        tracing::info!(
            "user_fonts: 清单 {} 份，注册 {}，不在盘上 {}，解不开 {}",
            summary.listed,
            summary.registered_files,
            summary.missing,
            summary.unreadable
        );
        if summary.registered_files > 0 {
            let _ = cx.update(|cx| cx.refresh_windows());
        }
    })
    .detach();
}

/// 一趟重注册的账，进日志。
#[derive(Default)]
struct Summary {
    listed: usize,
    registered_files: usize,
    missing: usize,
    unreadable: usize,
}

fn register_all(texts: &TextSystem, installed: &[PathBuf]) -> Summary {
    let mut summary = Summary {
        listed: installed.len(),
        ..Summary::default()
    };
    for path in installed {
        if !path.is_file() {
            summary.missing += 1;
            continue;
        }
        match register_file(texts, path) {
            Some(families) => {
                summary.registered_files += 1;
                tracing::debug!("user_fonts: 注册 {} → {:?}", path.display(), families);
            }
            None => summary.unreadable += 1,
        }
    }
    summary
}

/// 这份文件的字节是哪一种字体。
enum Kind {
    /// WOFF 1.0，注册前解回 sfnt。
    Woff,
    /// WOFF 2.0，同上。
    Woff2,
    /// sfnt 一族（TTF / OTF / TTC），原样注册。
    Sfnt,
    /// 不是认得的字体。
    Unknown,
}

/// 按头四个字节的魔数认。判据与 `folio_core::content::format::woff(2)::is_*` 同源：
/// 那两份只认自己的魔数，这里把五类收齐 —— 注册入口只该有一个。
fn kind_of(bytes: &[u8]) -> Kind {
    if bytes.len() < 4 {
        return Kind::Unknown;
    }
    match &bytes[..4] {
        b"wOFF" => Kind::Woff,
        b"wOF2" => Kind::Woff2,
        b"ttcf" | b"OTTO" | [0x00, 0x01, 0x00, 0x00] => Kind::Sfnt,
        _ => Kind::Unknown,
    }
}

/// 把一份字体文件注册进文本系统，交回报出来的家族名。
///
/// 会话内下载 / 导入完成后与开机扫描共用这一脚。注册成功顺手做三件事：
/// 「无拉丁 m」的进选择器的闸（[`font_primary::note_face`]）、家族名进「我的字体」
/// 登记表、推两记版本号让排版缓存与选择器对账。失败回 `None` —— 调用方记一笔，
/// 别的字体照走。
pub(crate) fn register_file(texts: &TextSystem, path: &Path) -> Option<Vec<String>> {
    let raw = std::fs::read(path).ok()?;
    let bytes = match kind_of(&raw) {
        Kind::Sfnt => raw,
        Kind::Woff => woff::decode(&raw)?,
        Kind::Woff2 => woff2::decode(&raw)?,
        Kind::Unknown => {
            tracing::warn!("user_fonts: {} 不是认得的字体文件，跳过", path.display());
            return None;
        }
    };
    // 覆盖要在注册**前**读：add_fonts 拿走字节的所有权。
    let coverage: LatinCoverage = latin_by_family(&bytes);
    let family_sets = texts.add_fonts_with_families(vec![Cow::Owned(bytes)]).ok()?;
    let families = family_sets.into_iter().next()?;
    for name in &families {
        font_primary::note_face(name, coverage.covers(name));
        font_primary::note_user_family(name);
    }
    note_external_registration();
    font_primary::bump_families_revision();
    Some(families)
}

#[cfg(test)]
mod tests {
    use super::kind_of;

    /// 魔数认五类：woff / woff2 要解，ttf / otf / ttc 原样，其余不收。
    #[test]
    fn the_magic_sniffer_sorts_the_five_kinds() {
        assert!(matches!(kind_of(b"wOF2\x00"), super::Kind::Woff2));
        assert!(matches!(kind_of(b"wOFF\x00"), super::Kind::Woff));
        assert!(matches!(kind_of(b"ttcf\x00"), super::Kind::Sfnt));
        assert!(matches!(kind_of(b"OTTO\x00"), super::Kind::Sfnt));
        assert!(matches!(kind_of(&[0x00, 0x01, 0x00, 0x00]), super::Kind::Sfnt));
        assert!(matches!(kind_of(b"PK\x03\x04"), super::Kind::Unknown), "zip 不收");
        assert!(matches!(kind_of(b"nope"), super::Kind::Unknown));
        assert!(matches!(kind_of(b"abc"), super::Kind::Unknown), "不足四字节不收");
    }
}
