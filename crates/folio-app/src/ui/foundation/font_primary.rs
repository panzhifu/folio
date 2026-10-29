//! 进程级「能当 gpui primary 的家族名」登记表。
//!
//! gpui 的 `CosmicTextSystem::load_family` 会把没有拉丁 `m` 的脸从 primary 候选里
//! 剔掉（`charmap().map('m') == 0`）；剔空后 `resolve_font` 只试全局回退栈，而
//! Android 上那串名字一个都不存在，直接 SIGABRT。这里记下每份内嵌字体注册时报出来的
//! 家族名与「有没有 m」，字体选择器、host 的兜底家族与 `gpui_font` 的读者链都先问
//! 这一句，避免把一个 gpui 用不了的家族交出去。
//!
//! 设备字体（没有随书字节、没走过注册）不在这里：它们按可用处理，与没有这道判断时
//! 同一条路。表是进程级的，因为字体注册进 `TextSystem` 本身就是进程级的。

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

/// 子集字体家族名的标记（见 `book_fonts::rename_subset_family`）。这些名字只属于某一本
/// 书的某个字符集，不能出现在「读者可选的正文家族」里。
pub const SUBSET_MARKER: &str = "#folio";

fn registry() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
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

/// 这个家族名能不能交给 gpui 当 primary。没登记过（设备字体）按可用；子集字体
/// （带 [`SUBSET_MARKER`]）永远不进读者可选的家族表。
pub fn usable(name: &str) -> bool {
    if name.contains(SUBSET_MARKER) {
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
    use super::{note_face, usable, usable_names};

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
}
