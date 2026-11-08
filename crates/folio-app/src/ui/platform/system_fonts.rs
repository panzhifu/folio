//! Android 的系统字体**全量**扫描：把 gpui-mobile fork 那份白名单之外的
//! `/system/fonts` 与 `/product/fonts` 字体补注册进文本系统。
//!
//! 为什么必须自己做：fontdb 的 `load_system_fonts()` 在源码里明确排除 Android
//! （`not(any(target_os = "macos", target_os = "android"))`），fork 只好在平台初始化时
//! 手动读 ~24 个硬编码路径。那是一份**允许清单**而不是目录扫描 —— 厂商中文字体、
//! `/product/fonts` 里的、其他语种的字体一律不存在，移动端字体选择器里就只有那二十来个
//! 家族。桌面没有这道坎：fontdb 在那一端本来就全量扫系统目录。
//!
//! 字节按 `Source::Binary` 注册（fork 现有的 `add_fonts_with_families` 只收字节），
//! 注册过的字节会常驻内存。已注册的家族一律跳过（fork 那 24 份全部命中，最贵的
//! `NotoSansCJK-Regular.ttc` 约 19MB 不会重复驻留），额外的常驻量就是白名单之外的那
//! 一部分，完成日志里带字节数，真机上量出来再说。真要压到零常驻，得给 fork 加
//! `Source::File` 那条 mmap 加载 —— 内存实测超标再上。
//!
//! emoji 家族跳过：fork 已经从 APK 注册了 CBDT 版的 Noto Color Emoji；再注册系统的
//! COLRv1 版，两个同名脸会让 gpui resolve 到画不出来的那一份（`book_fonts` 的
//! HarmonyOS 实测注释记过同一类事：`HMOSColorEmojiCompat.ttf` 一进 fontdb，emoji 就画不出）。
//!
//! 模块对**所有 target** 编译（纯函数与测试在桌面也要跑），只有 [`load_all_system_fonts`]
//! 的实体走 Android；iOS / HarmonyOS 的系统字体由各自平台的字体系统注册，桌面由
//! fontdb 全量扫 —— 都不需要这一趟。
//!
//! 为什么不用惊动排版缓存（`book_fonts::FACE_EVENTS` 那一格）：扫描在 `AppShell::boot`
//! 就起跑，那时窗口还没建；等读者能点到任何一本书，扫描早已完成。不存在「开着书等字体
//! 表变宽」的真实窗口，排版缓存的 `fonts_rev` 保持原值是对的 —— 开书那一次排版拿到的
//! 就是全量的字体表。

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::{App, AppContext as _, TextSystem};

use folio_core::text as font_primary;
use folio_core::text::{LatinCoverage, latin_by_family};

/// 扫描哪些目录。`/product/fonts` 在不少设备上装着厂商字体（有的 CJK 衬线就在那里）。
const FONT_DIRS: &[&str] = &["/system/fonts", "/product/fonts"];

/// 一批注册的字节数上限：到了就先注册再继续。批太大会把峰值内存顶上去
/// （批次字节 + fontdb 复制的那一份同时在场），批太小就退回 `register_batch`
/// 注释里说的「逐份注册付 2 × 份数次枚举」的老路。
const BATCH_BYTES: usize = 8 * 1024 * 1024;

/// 一批最多几份：小文件别凑出一张过长的清单。
const BATCH_FILES: usize = 4;

/// 扫描只跑一次。Activity 重建再进 `android_main` 时直接让路 —— 二次扫描什么也
/// 注册不进来（名字全撞已注册），白付一遍目录枚举。
static STARTED: AtomicBool = AtomicBool::new(false);

/// 把这台设备的系统字体补全。启动时调一次（`shell` 建窗前）。
///
/// 重活跑在 `background_spawn` 上（字节从闪存读、name 表解析都在后台），完成后回
/// 主线程推 [`font_primary::bump_families_revision`] 并 `refresh_windows`，让已经建好
/// 的字体选择器重取名单。一台字体全部在白名单里的设备什么也不会注册 —— 那就不推版本。
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
            "system_fonts: 枚举 {}，注册 {} 份 {} 字节；已注册跳过 {}，emoji 跳过 {}，读不出 {}",
            summary.enumerated,
            summary.registered_files,
            summary.registered_bytes,
            summary.known_skipped,
            summary.emoji_skipped,
            summary.unreadable,
        );
        if summary.registered_files > 0 {
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
    registered_files: usize,
    registered_bytes: usize,
    known_skipped: usize,
    emoji_skipped: usize,
    unreadable: usize,
}

/// 一份待注册的字体：解析出的覆盖与字节。
struct Pending {
    coverage: LatinCoverage,
    bytes: Vec<u8>,
    len: usize,
}

/// 扫描目录并把白名单外的字体注册进 `texts`。在后台线程上跑
/// （`all_font_names` / `add_fonts_with_families` 都是 `&self`，度量 worker 那条路
/// 已经这么用了）。
fn scan_and_register(texts: &TextSystem) -> ScanSummary {
    let mut known: HashSet<String> = texts.all_font_names().into_iter().collect();
    let mut summary = ScanSummary::default();
    let mut batch: Vec<Pending> = Vec::new();
    let mut batch_bytes = 0usize;

    for dir in FONT_DIRS {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) => {
                tracing::info!("system_fonts: {dir} 读不了：{err}");
                continue;
            }
        };
        // 先收齐再排序：注册顺序确定，日志在两台设备上才对得上。
        let mut files: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_file() && folio_core::text::scan::is_font_file(path))
            .collect();
        files.sort();
        summary.enumerated += files.len();

        for path in files {
            let Ok(bytes) = std::fs::read(&path) else {
                summary.unreadable += 1;
                continue;
            };
            let coverage = latin_by_family(&bytes);
            let families: Vec<&str> = coverage.families().collect();
            if families.is_empty() {
                summary.unreadable += 1;
                continue;
            }
            match folio_core::text::scan::scan_verdict(&families, &known) {
                folio_core::text::scan::ScanVerdict::AlreadyRegistered => {
                    summary.known_skipped += 1;
                    continue;
                }
                folio_core::text::scan::ScanVerdict::Emoji => {
                    summary.emoji_skipped += 1;
                    continue;
                }
                folio_core::text::scan::ScanVerdict::Keep => {}
            }
            // 记进 known：同一字体出现在两个目录（或一份拆成多张）时，第二份走跳过。
            known.extend(families.iter().map(|name| (*name).to_string()));
            tracing::debug!("system_fonts: 注册 {} → {:?}", path.display(), families);
            let len = bytes.len();
            batch.push(Pending {
                coverage,
                bytes,
                len,
            });
            batch_bytes += len;
            if batch.len() >= BATCH_FILES || batch_bytes >= BATCH_BYTES {
                batch_bytes = 0;
                register_batch(texts, std::mem::take(&mut batch), &mut summary);
            }
        }
    }
    register_batch(texts, batch, &mut summary);
    summary
}

/// 注册一批。家族名以 `add_fonts_with_families` 报回来的为准（fontdb 解出的那张
/// name 表，与 `book_fonts::register_batch` 同一条口径），「无拉丁 m」的照旧进
/// [`font_primary::note_face`] 的登记表，选择器的闸自己会挡。
fn register_batch(texts: &TextSystem, batch: Vec<Pending>, summary: &mut ScanSummary) {
    if batch.is_empty() {
        return;
    }
    let total: usize = batch.iter().map(|pending| pending.len).sum();
    // 字节在 `add_fonts_with_families` 里被拿走所有权，覆盖要**先**抽出来留着 ——
    // 名字与「有没有 m」的配对得等注册成功、按返回的家族名逐个记。
    let mut coverages = Vec::with_capacity(batch.len());
    let mut bytes = Vec::with_capacity(batch.len());
    for pending in batch {
        coverages.push(pending.coverage);
        bytes.push(Cow::Owned(pending.bytes));
    }
    let family_sets = match texts.add_fonts_with_families(bytes) {
        Ok(family_sets) => family_sets,
        Err(err) => {
            // 整批失败：那份字节就当没存在过。下一轮启动还会再试。
            tracing::warn!("system_fonts: 一批 {total} 字节注册失败：{err}");
            return;
        }
    };
    let files = coverages.len();
    for (coverage, families) in coverages.into_iter().zip(family_sets) {
        for name in &families {
            font_primary::note_face(name, coverage.covers(name));
        }
    }
    summary.registered_files += files;
    summary.registered_bytes += total;
}
