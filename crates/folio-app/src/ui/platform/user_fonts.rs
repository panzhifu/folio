//! 读者自己装的字体（下载 / 导入）：开机重注册进文本系统，与装完一份后的当场注册。
//!
//! 为什么开机要**重**注册：字体注册是进程级的（gpui 的 fontdb 不落盘），读者昨天装的
//! 字体今天重启就没了。配置里记着 `<fonts 根>/<id>/<file>`（真相源在 config，盘上文件
//! 只是它的附件 —— 与词典同一条），开机照着清单把 face 再灌一遍。与 [`system_fonts`]
//! 的差别就在真相源：那份扫**系统目录**，盘上有什么注册什么；这份只认配置里点过名的，
//! 删掉的 id 不会再灌。
//!
//! 注册按魔数分流（[`folio_core::fonts::font_kind`]，只读头四个字节）：sfnt 一族
//! （ttf / otf / ttc）走 fork 的 `add_font_files` **路径**注册 —— 字节留在盘上，
//! 首次用到才映射，装了 25MB 的霞鹜文楷也不再常驻；woff / woff2 解回 sfnt 后按
//! 字节注册（解出来的 sfnt 没有文件路径可挂，量少、解完就是压缩前的小字体，可
//! 接受）。都不是的当坏文件跳过。
//!
//! 失败的处置是「跳过，别拦启动」：一份解不开的字体只少一个家族，不能少一次开机。
//! 失败的份数进日志，设置页的已装列表按配置画 —— 读者能看到它还在列表里但正文排
//! 不出来，删掉重装是现成的出路。
//!
//! 覆盖（有没有拉丁 `m`）要**先**读字节才算：路径注册不经过我们手里的字节，而
//! `add_fonts_with_families` 拿走所有权。覆盖算完字节就放 —— 峰值是单份文件，
//! 不再是整份清单。注册成功后的两记失效（`book_fonts` 的字体表版本计数与
//! [`font_primary::families_revision`]）：开机早于任何排版，bump 无成本；会话内
//! 装完一份时书可能正开着，行宽是按旧字体表算的，**必须** bump。
//!
//! [`system_fonts`]: crate::ui::platform::system_fonts

use std::borrow::Cow;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::{App, AppContext as _, TextSystem};

use crate::ui::services::config::ConfigHolder;
use folio_core::fonts::{self, FontFileKind};
use folio_core::text as font_primary;
use folio_core::text::load::note_external_registration;
use folio_core::text::names::LatinCoverage;
use folio_core::text::latin_by_family;

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
            cx.update(|cx| cx.refresh_windows());
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

/// 把一份字体文件注册进文本系统，交回报出来的家族名。
///
/// 会话内下载 / 导入完成后与开机扫描共用这一脚。按魔数分流：sfnt 走路径注册
/// （懒加载），woff / woff2 解回 sfnt 走字节注册。注册成功顺手记三笔账（见
/// [`note_registered`]）；失败回 `None` —— 调用方记一笔，别的字体照走。
pub(crate) fn register_file(texts: &TextSystem, path: &Path) -> Option<Vec<String>> {
    match magic_kind(path)? {
        // sfnt 一族：字节只做覆盖解析就放，注册交路径 —— 内存零驻留。
        FontFileKind::Sfnt => {
            let raw = std::fs::read(path).ok()?;
            let coverage = latin_by_family(&raw);
            let table_names: Vec<String> = coverage.families().map(str::to_string).collect();
            drop(raw);
            if table_names.is_empty() {
                return None;
            }
            let families = texts
                .add_font_files(vec![path.to_path_buf()])
                .ok()?
                .into_iter()
                .next()?;
            if families.is_empty() {
                return None;
            }
            note_registered(&families, &coverage, &table_names);
            Some(families)
        }
        // woff / woff2：解回 sfnt 才注册得上，解出来的字节没有路径可挂。
        FontFileKind::Woff | FontFileKind::Woff2 => {
            let raw = std::fs::read(path).ok()?;
            let bytes = fonts::decode_font_file(&raw)?;
            // 覆盖要在注册**前**读：add_fonts_with_families 拿走字节的所有权。
            let coverage = latin_by_family(&bytes);
            let table_names: Vec<String> = coverage.families().map(str::to_string).collect();
            let family_sets = texts
                .add_fonts_with_families(vec![Cow::Owned(bytes)])
                .ok()?;
            let families = family_sets.into_iter().next()?;
            if families.is_empty() {
                return None;
            }
            note_registered(&families, &coverage, &table_names);
            Some(families)
        }
        FontFileKind::Unknown => None,
    }
}

/// 头四个字节的魔数。只开文件读四字节，不为判类别把 25MB 拉进内存。
fn magic_kind(path: &Path) -> Option<FontFileKind> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 4];
    file.read_exact(&mut head).ok()?;
    Some(fonts::font_kind(&head))
}

/// 注册成功后的三笔账：选择器的闸（[`font_primary::note_face`]）、「我的字体」
/// 登记表、别名归一 —— 单家族文件把 name 表的全部本地化写法归到注册报回的规范名
/// （中文名与英文名从此互通），多家族文件分不清哪个写法属于哪张脸，只做自映射。
/// 最后两记版本号让排版缓存与选择器对账。
fn note_registered(families: &[String], coverage: &LatinCoverage, table_names: &[String]) {
    for name in families {
        font_primary::note_face(name, coverage.covers(name));
        font_primary::note_user_family(name);
    }
    if families.len() == 1 {
        font_primary::note_family_aliases(&families[0], table_names);
    } else {
        for name in families {
            font_primary::note_family_aliases(name, std::slice::from_ref(name));
        }
    }
    note_external_registration();
    font_primary::bump_families_revision();
}
