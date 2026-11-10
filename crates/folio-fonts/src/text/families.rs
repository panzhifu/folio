//! 进程级「没有拉丁 `m` 的家族名」登记表 —— **字体选择器的闸**。
//! （2026-11-09 从 `folio_app::ui::foundation::font_primary` 搬来：全部是
//! `HashSet` / `AtomicU64`，0 个 gpui 类型，而消费方除选择器外还有 core 的加载管线。）
//!
//! 从前它还兼管「能不能当 gpui primary」：gpui 的 `CosmicTextSystem::load_family`
//! 会把无 `m` 的脸从 primary 候选里剔掉，剔空后 `resolve_font` 在 Android 上直接
//! SIGABRT。那一格随 gpui-folio be92574 删除（过滤器没了，primary 未覆盖的字符走
//! 补缺链），这张表的剩余职责只有一条：**emoji 与符号字体不该出现在读者的正文字体
//! 可选列表里** —— 它们没有拉丁字形，选了当正文，数字与英文全靠回退兜。
//!
//! 随书内嵌字体当 primary 不再问这张表（`text::load` 的 `BookFonts::primary_usable`）；
//! 字体选择器与 host 的兜底家族仍先问这一句。
//!
//! 这里另有两份进程级状态，都是给字体选择器用的：
//!
//! - **随书家族登记表**（`note_book_family` / `is_book_family`）：哪几个家族名来自随书
//!   注册，选择器靠它把「书籍字体」从「系统字体」里拆出来。
//! - **读者自装家族登记表**（`note_user_family` / `is_user_family`）：哪几个家族名来自
//!   下载或导入（`platform::user_fonts`），选择器靠它们单独立一组「我的字体」。
//! - **可选字体表版本号**（`families_revision`）：字体表变了（随书注册、移动端系统
//!   字体扫描完成、自装字体装上）就推一格，选择器对账后重取名单。
//!
//! 设备字体（没有随书字节、没走过注册）不在登记表里：它们按可用处理，与没有这道判断时
//! 同一条路。表是进程级的，因为字体注册进 `TextSystem` 本身就是进程级的。

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// 子集字体家族名的标记（见 `text::load::rename_subset_family`）。这些名字只属于某一本
/// 书的某个字符集，不能出现在「读者可选的正文家族」里。
pub const SUBSET_MARKER: &str = "#folio";

fn registry() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 进程级「哪些家族名来自随书注册」登记表 —— 字体选择器分组的依据。
///
/// `text::load::register_batch` 每注册成功一份字体就记一次（子集字体除外：它们带
/// [`SUBSET_MARKER`]，本来就被 [`usable`] 挡在选择器外，记进来只会造成假分组）。表是
/// 进程级的、关书不清 —— gpui 的字体注册本身就不撤，撤了登记反而和现实对不上。
fn book_families() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 记一个「来自随书注册」的家族名。带 [`SUBSET_MARKER`] 的名字直接忽略。
pub fn note_book_family(name: &str) {
    if name.contains(SUBSET_MARKER) {
        return;
    }
    book_families()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(name.to_string());
}

/// 这个家族名是不是随书注册进来的。
pub fn is_book_family(name: &str) -> bool {
    book_families()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(name)
}

/// 进程级「哪些家族名是读者自己装的」登记表 —— 选择器「我的字体」组的依据。
///
/// app 侧 `platform::user_fonts` 每注册成功一份下载 / 导入的字体就记一次。与随书表同一条
/// 生命周期：进程级、不撤 —— gpui 的注册撤不回来，登记跟着现实走。
fn user_families() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 进程级「已删除（退休）的家族名」。gpui 的 fontdb 没有注销：删掉的字体字节驻留到
/// 重启，`all_font_names()` 里它还在。配置里那条已经摘了，选择器不能等重启才消失
/// —— 退休表把 [`usable`] 挡住，让它在场也选不到。
fn retired_families() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 记一个「读者自己装的」家族名。同名先前被退休过（删了又重装）时顺手撤销退休 ——
/// 字节重新注册进来了，就该重新可选。
pub fn note_user_family(name: &str) {
    if name.contains(SUBSET_MARKER) {
        return;
    }
    user_families()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(name.to_string());
    retired_families()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(name);
}

/// 这个家族名是不是读者自己装的。
pub fn is_user_family(name: &str) -> bool {
    user_families()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(name)
}

/// 退休一个家族：删除自装字体后调用。字节驻留到重启（fontdb 无注销），但选择器
/// 与 host 的兜底家族从这一刻起都看不见它。
pub fn retire_family(name: &str) {
    retired_families()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(name.to_string());
}

/// 可选字体表的变化版本号。字体选择器拿它对账：数字变了就重取
/// `all_font_names()` 重建分组列表，不用自己盯着字体系统。
///
/// 两处会推它：随书注册（`text::load::register_batch`）与移动端的系统字体全量扫描
/// （`system_fonts`）。走原子而不是 gpui Global，因为 register_batch 在排版 worker
/// 线程上跑，够不着主线程的 `App`。
fn revision() -> &'static AtomicU64 {
    static REV: AtomicU64 = AtomicU64::new(0);
    &REV
}

pub fn families_revision() -> u64 {
    revision().load(Ordering::Relaxed)
}

pub fn bump_families_revision() {
    revision().fetch_add(1, Ordering::Relaxed);
}

/// 记一份字体报出来的家族名与它能不能当 primary。同名后来出现带 `m` 的脸时撤销。
pub fn note_face(name: &str, latin: bool) {
    let mut set = registry().lock().unwrap_or_else(|e| e.into_inner());
    if latin {
        set.remove(name);
    } else {
        set.insert(name.to_string());
    }
}

/// 这个家族名能不能进**字体选择器**（读者可选的正文家族）。没登记过（设备字体）按
/// 可用；子集字体（带 [`SUBSET_MARKER`]）与已退休的（删掉的自装字体，字节还驻留）不进。
pub fn usable(name: &str) -> bool {
    if name.contains(SUBSET_MARKER) {
        return false;
    }
    if retired_families()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(name)
    {
        return false;
    }
    let set = registry().lock().unwrap_or_else(|e| e.into_inner());
    !set.iter().any(|bad| bad.eq_ignore_ascii_case(name))
}

/// 过滤掉注册时发现没有拉丁 `m` 的家族；顺序不变。
pub fn usable_names(names: Vec<String>) -> Vec<String> {
    names.into_iter().filter(|name| usable(name)).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        bump_families_revision, families_revision, is_book_family, is_user_family,
        note_book_family, note_face, note_user_family, retire_family, usable, usable_names,
    };

    /// 没有 `m` 的名字被登记后要从候选里消失；同名后来出现带 `m` 的脸时要能撤销。
    /// 名字取一个只在本测试里用的串，避免与并行测试互相影响。
    #[test]
    fn a_no_latin_name_is_held_back_until_a_latin_face_appears() {
        let name = "Folio Test No Latin Face";
        assert!(usable(name), "没登记过按可用");
        note_face(name, false);
        assert!(!usable(name));
        assert!(
            usable_names(vec![name.to_string(), "Noto Sans CJK SC".to_string()])
                .iter()
                .all(|kept| kept != name),
            "字体选择器不该列出它"
        );
        note_face(name, true);
        assert!(usable(name), "同名出现带 m 的脸后要恢复");
        assert!(
            !usable("Book Face#folio0001"),
            "书内子集字体不能出现在读者可选的家族里"
        );
    }

    /// 随书注册的家族名进了登记表就能被认出来；子集字体（带 `#folio`）不收。
    #[test]
    fn a_book_registered_family_is_known_and_subsets_are_ignored() {
        let name = "Folio Test Book Family";
        assert!(!is_book_family(name), "没登记过不算");
        note_book_family(name);
        assert!(is_book_family(name));
        note_book_family("Folio Test Subset#folio0007");
        assert!(
            !is_book_family("Folio Test Subset#folio0007"),
            "子集字体不该被当成可选的书籍字体"
        );
    }

    /// 版本号推一下就变。用偶数倍的推法免得和并行测试互踩。
    #[test]
    fn bumping_moves_the_revision() {
        let before = families_revision();
        bump_families_revision();
        bump_families_revision();
        assert!(families_revision() >= before + 2);
    }

    /// 读者自装的家族：登记后认得出；退休后从可选名单里消失（字节还驻留在
    /// 文本系统里，但选不到）；重新装上要撤销退休。
    #[test]
    fn a_user_family_can_be_retired_and_unretired() {
        let name = "Folio Test User Family";
        assert!(!is_user_family(name), "没登记过不算");
        note_user_family(name);
        assert!(is_user_family(name));
        assert!(usable(name));

        retire_family(name);
        assert!(!usable(name), "退休后选择器要立刻看不见它，不能等重启");
        assert!(
            !usable_names(vec![name.to_string()])
                .iter()
                .any(|kept| kept == name)
        );

        note_user_family(name);
        assert!(usable(name), "删了又重装要恢复可选");
    }
}
