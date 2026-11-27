//! Android 的系统字体**全量**扫描：把 gpui-mobile fork 那份白名单之外的
//! `/system/fonts` 与 `/product/fonts` 字体补注册进文本系统。
//!
//! 为什么必须自己做：fontdb 的 `load_system_fonts()` 在源码里明确排除 Android
//! （`not(any(target_os = "macos", target_os = "android"))`），fork 只好在平台初始化时
//! 手动注册一批字体。那是一份**允许清单**而不是目录扫描 —— 厂商中文字体、
//! `/product/fonts` 里的、其他语种的字体一律不存在，移动端字体选择器里就只有那二十来个
//! 家族。桌面没有这道坎：fontdb 在那一端本来就全量扫系统目录。
//!
//! 注册走 fork 的 `add_font_files`（**路径**注册）：数据库里只存 face 元数据，字节
//! 留在盘上、首次用到才映射 —— 与桌面 fontdb 的 `Source::File` 同一条懒加载路。
//! 常驻的只剩每张脸几 KB 的元数据，注册再多的系统字体也不吃内存。fork 白名单
//! 那批也改成了路径注册（`gpui-mobile` 的 `platform.rs`）：字节常驻从注册那一刻
//! 就不存在了。
//!
//! 名字与判重的分层（见 `folio_core::text::scan` 的模块注释）：
//!
//! - **跨源**的重复（白名单 / 用户字体已注册）由 fork 的路径守卫挡：路径已在库里的
//!   只回名字不再灌；
//! - **本轮之内**的重复（同一字体躺在两个目录）由 [`scan_verdict`] 按归一名字挡，
//!   种子是 `all_font_names()` 加本轮已处理的名字 —— fontdb 每张脸只报第一个家族
//!   名的口径缺口，由 name 表的**全部本地化名**补齐：中文名、英文名、连字符写法
//!   都归一后比对；
//! - emoji 家族永远跳过（COLRv1 与 CBDT 两份同名脸的事故）。
//!
//! 系统自己的账本（`/system/etc/fonts.xml` 与 `font_fallback.xml`）经
//! [`folio_core::text::system_xml`] 尽力解析：family/别名 → 文件的映射进进程别名
//! 索引（`font_primary::note_family_aliases`），读者叫 `arial`、叫本地化中文名都
//! 能归到设备上真实存在的写法。账本打不开就退回纯 name 表口径，不拦启动。
//!
//! 模块对**所有 target** 编译（纯函数与测试在桌面也要跑），只有 [`load_all_system_fonts`]
//! 的实体走 Android；iOS / HarmonyOS 的系统字体由各自平台的字体系统注册，桌面由
//! fontdb 全量扫 —— 都不需要这一趟。
//!
//! 为什么不用惊动排版缓存（`book_fonts::FACE_EVENTS` 那一格）：扫描在 `AppShell::boot`
//! 就起跑，那时窗口还没建；等读者能点到任何一本书，扫描早已完成。不存在「开着书等字体
//! 表变宽」的真实窗口，排版缓存的 `fonts_rev` 保持原值是对的 —— 开书那一次排版拿到的
//! 就是全量的字体表。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::{App, AppContext as _, TextSystem};

use folio_core::text as font_primary;
use folio_core::text::alias::normalize_family;
use folio_core::text::latin_by_family;
use folio_core::text::names::LatinCoverage;
use folio_core::text::scan::{ScanVerdict, is_font_file, scan_verdict};
use folio_core::text::system_xml::{self, SystemFontConfig};

/// 扫描哪些目录。`/product/fonts` 在不少设备上装着厂商字体（有的 CJK 衬线就在那里）。
const FONT_DIRS: &[&str] = &["/system/fonts", "/product/fonts"];

/// 系统字体账本的位置。`font_fallback.xml` 是新平台要求厂商放变体字体配置的地方，
/// 两份都在就合并；都不在（老设备 / 奇怪 ROM）就退回纯 name 表口径。
const FONT_CONFIG_PATHS: &[&str] = &["/system/etc/fonts.xml", "/system/etc/font_fallback.xml"];

/// 扫描只跑一次。Activity 重建再进 `android_main` 时直接让路 —— 二次扫描什么也
/// 注册不进来（路径守卫全命中），白付一遍目录枚举。
static STARTED: AtomicBool = AtomicBool::new(false);

/// 把这台设备的系统字体补全。启动时调一次（`shell` 建窗前）。
///
/// 重活跑在 `background_spawn` 上（目录枚举、name 表解析、覆盖计算都在后台），
/// 完成后回主线程推 [`font_primary::bump_families_revision`] 并 `refresh_windows`，
/// 让已经建好的字体选择器重取名单。一台字体全部在白名单里的设备什么也不会新注册
/// —— 那就不推版本。
pub(crate) fn load_all_system_fonts(cx: &mut App) {
    if !cfg!(target_os = "android") {
        return;
    }
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let texts: Arc<TextSystem> = cx.text_system().clone();
    let work = cx.background_spawn(async move { scan_and_register(&texts) });
    cx.spawn(async move |cx| {
        let summary = work.await;
        tracing::info!(
            "system_fonts: 枚举 {}，处理 {} 份（路径已在库的含在内）；emoji 跳过 {}，本轮重复 {}，读不出 {}",
            summary.enumerated,
            summary.processed_files,
            summary.emoji_skipped,
            summary.duplicate_skipped,
            summary.unreadable,
        );
        if summary.processed_files > 0 {
            cx.update(|cx| {
                font_primary::bump_families_revision();
                cx.refresh_windows();
            });
        }
    })
    .detach();
}

#[derive(Default)]
struct ScanSummary {
    enumerated: usize,
    processed_files: usize,
    emoji_skipped: usize,
    duplicate_skipped: usize,
    unreadable: usize,
}

/// 一份待处理字体：路径、name 表与账本合并出的候选名、拉丁覆盖。
struct Pending {
    path: PathBuf,
    names: Vec<String>,
    coverage: LatinCoverage,
}

/// 读两份系统账本并合并。尽力解析：读不了 / 解析不了的那份当空账本。
fn system_config() -> SystemFontConfig {
    let mut merged = SystemFontConfig::default();
    for path in FONT_CONFIG_PATHS {
        let Ok(xml) = std::fs::read_to_string(path) else {
            continue;
        };
        let config = system_xml::parse_font_config(&xml);
        for (file, mut names) in config.names_by_file {
            let entry = merged.names_by_file.entry(file).or_default();
            for name in names.drain(..) {
                if !entry.contains(&name) {
                    entry.push(name);
                }
            }
        }
        for (alias, target) in config.alias_targets {
            merged.alias_targets.insert(alias, target);
        }
    }
    merged
}

/// 扫描目录并把白名单外的字体按**路径**注册进 `texts`。在后台线程上跑
/// （`all_font_names` / `add_font_files` 都是 `&self`，度量 worker 那条路
/// 已经这么用了）。
fn scan_and_register(texts: &TextSystem) -> ScanSummary {
    let config = system_config();
    let mut summary = ScanSummary::default();
    // 判重的种子：库里现报的全部家族名（白名单、用户字体都算）+ 本轮已处理的。
    // 比较走 [`normalize_family`] —— fontdb 每张脸只报第一个家族名的口径缺口，
    // 由待判字体 name 表的全部本地化名补齐。
    let mut seen: HashSet<String> = texts
        .all_font_names()
        .iter()
        .map(|name| normalize_family(name))
        .collect();

    let mut pending: Vec<Pending> = Vec::new();
    for dir in FONT_DIRS {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) => {
                tracing::info!("system_fonts: {dir} 读不了：{err}");
                continue;
            }
        };
        // 先收齐再排序：处理顺序确定，日志在两台设备上才对得上。
        let mut files: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_file() && is_font_file(path))
            .collect();
        files.sort();
        summary.enumerated += files.len();

        for path in files {
            // 字节只做覆盖与名字解析，读完就放 —— 注册走路径，不驻留。
            let Ok(raw) = std::fs::read(&path) else {
                summary.unreadable += 1;
                continue;
            };
            let coverage = latin_by_family(&raw);
            drop(raw);
            let mut names: Vec<String> = coverage.families().map(str::to_string).collect();
            if names.is_empty() {
                summary.unreadable += 1;
                continue;
            }
            if let Some(file) = path.file_name().and_then(|name| name.to_str()) {
                for name in config.families_of(file) {
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
            let references: Vec<&str> = names.iter().map(String::as_str).collect();
            match scan_verdict(&references, &seen) {
                ScanVerdict::Emoji => {
                    summary.emoji_skipped += 1;
                    continue;
                }
                ScanVerdict::Duplicate => {
                    summary.duplicate_skipped += 1;
                    continue;
                }
                ScanVerdict::Keep => {}
            }
            pending.push(Pending {
                path,
                names,
                coverage,
            });
        }
    }
    if pending.is_empty() {
        return summary;
    }
    // 一趟注册：路径守卫在 fork 里按路径挡白名单与上轮的重复，返回每份路径的
    // 家族名（已在库的也回）。名字、覆盖与别名按返回值记账。
    let paths: Vec<PathBuf> = pending.iter().map(|pending| pending.path.clone()).collect();
    let per_path = match texts.add_font_files(paths) {
        Ok(per_path) => per_path,
        Err(err) => {
            tracing::warn!("system_fonts: 一批 {} 份路径注册失败：{err}", pending.len());
            return summary;
        }
    };
    for (pending, families) in pending.into_iter().zip(per_path) {
        if families.is_empty() {
            summary.unreadable += 1;
            continue;
        }
        // 「无拉丁 m」的进选择器的闸；家族名以 fontdb 报回的为准。
        for name in &families {
            font_primary::note_face(name, pending.coverage.covers(name));
        }
        // 别名归一：单家族文件把 name 表与账本的全部写法归到注册报回的规范名；
        // 多家族文件分不清哪个本地化名属于哪张脸，只做自映射。
        if families.len() == 1 {
            font_primary::note_family_aliases(&families[0], &pending.names);
        } else {
            for name in &families {
                font_primary::note_family_aliases(name, std::slice::from_ref(name));
            }
        }
        for name in families.iter().chain(pending.names.iter()) {
            seen.insert(normalize_family(name));
        }
        summary.processed_files += 1;
    }
    summary
}
