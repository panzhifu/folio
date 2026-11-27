//! 进程级「别名 → 规范家族名」索引 —— 家族名交给文本系统**之前**的最后一道归一。
//!
//! fork 的 `load_family` 按**字符串精确相等**在 fontdb 的脸里挑家族，任何一个维度
//! 对不上都静默回退 —— 名字表里的本地化中文名与英文名、大小写、连字符与空格，都
//! 是对不上的维度。这正是「读者选了字体、正文没生效」的主因：选择器列出的名字来自
//! fontdb（可能是本地化名），配置里存的名字可能来自上一台设备或清单（英文名）。
//!
//! 这张表把「同一张脸的全部叫法」归到设备上真实存在的**规范名**：系统字体扫描
//! （fonts.xml 的 family/alias + name 表）、读者自装注册、host 兜底家族表三个入口
//! 都往里记；消费方在名字进 gpui 之前问一句 [`resolve`]。随书字体**不进**这张表 ——
//! 声明名 → 内部家族的映射是每本书自己的事（[`BookFonts`](crate::text::load::BookFonts)），
//! 子集名带 [`SUBSET_MARKER`](crate::text::families::SUBSET_MARKER) 本来就不进选择器。
//!
//! 与 [`families`](crate::text::families) 的分工：那边管「哪些名字**可以选**」
//! （闸与分组），这边管「读者叫的名字**对应设备上哪张脸**」（归一）。两边都是
//! 进程级的 —— 字体注册进 `TextSystem` 本身就是进程级的。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// 名字比较的归一口径：只留字母数字、转小写。`MiSans` / `mi sans` / `Mi-Sans`
/// 在这套口径下是同一个名字；存储仍用原始名，交回给 gpui 的必须是设备上
/// 真实存在的那个写法。
pub fn normalize_family(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn alias_map() -> &'static Mutex<HashMap<String, String>> {
    static MAP: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 记一个规范家族名与它的全部别名（规范名自身也在 `aliases` 里，调用方不必去重）。
///
/// 同一个归一后别名指向两个规范名（两台设备的账本混进来一份重名）时，**先到的
/// 留下**，后来的记一笔日志 —— 归一表要求一个别名只映射一个答案，不然「resolve
/// 有时对有时错」比「resolve 不中」更难排查。
pub fn note_family_aliases(canonical: &str, aliases: &[String]) {
    if canonical.contains(crate::text::families::SUBSET_MARKER) {
        return;
    }
    let mut map = alias_map().lock().unwrap_or_else(|e| e.into_inner());
    let target_key = normalize_family(canonical);
    for alias in std::iter::once(canonical).chain(aliases.iter().map(String::as_str)) {
        let key = normalize_family(alias);
        if key.is_empty() {
            continue;
        }
        match map.get(&key) {
            // 同名不同写法的两条规范名（归一键相同）也算冲突：先到的留下。
            Some(existing) if normalize_family(existing) != target_key => {
                tracing::debug!(
                    "font_alias: 别名 {:?} 已归到 {:?}，忽略后来的 {:?}",
                    alias,
                    existing,
                    canonical
                );
            }
            _ => {
                map.insert(key, canonical.to_string());
            }
        }
    }
}

/// 把一个可能不精确的名字归一到已知的规范家族名。没登记过的名字（设备随机名、
/// 泛族名）回 `None`，调用方原样放行 —— 归一表是补丁，不是闸。
pub fn resolve(name: &str) -> Option<String> {
    let key = normalize_family(name);
    if key.is_empty() {
        return None;
    }
    alias_map()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned()
}

/// 归一并落到 `known` 名单里的设备写法：名字已在名单里原样返回；索引认得就取
/// 规范名，规范名也要在名单里才算数 —— 归一的落点必须是设备上真实存在的
/// 那张脸（fontdb 每张脸只报一个家族名，名单与索引的写法可能差一截）。
pub fn resolve_in(name: &str, known: &[String]) -> Option<String> {
    if known.iter().any(|n| n.eq_ignore_ascii_case(name)) {
        return Some(name.to_string());
    }
    let canonical = resolve(name)?;
    known
        .iter()
        .find(|n| n.eq_ignore_ascii_case(canonical.as_str()))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::{normalize_family, note_family_aliases, resolve};

    /// 规范名与别名（含大小写/连字符/空格变体）都归到同一个规范名；没登记过的
    /// 名字回 None。
    #[test]
    fn an_alias_resolves_to_the_canonical_family() {
        let canonical = "Folio Test Alias MiSans";
        note_family_aliases(canonical, &["MiSans".into(), "小米兰亭 Pro".into()]);
        assert_eq!(resolve("MiSans"), Some(canonical.to_string()));
        assert_eq!(resolve("mi sans"), Some(canonical.to_string()));
        assert_eq!(resolve("MI-SANS"), Some(canonical.to_string()));
        assert_eq!(resolve("小米兰亭 Pro"), Some(canonical.to_string()));
        assert_eq!(resolve("Folio Test Alias MiSans"), Some(canonical.to_string()));
        assert_eq!(resolve("Folio Test Alias Not Registered"), None);
    }

    /// 同一别名只落第一个规范名：后来的记日志、不改表 —— resolve 必须是确定性的。
    #[test]
    fn the_first_canonical_wins_a_contested_alias() {
        let first = "Folio Test Alias Contested First";
        let second = "Folio Test Alias Contested Second";
        note_family_aliases(first, &["Contested Name".into()]);
        note_family_aliases(second, &["Contested Name".into()]);
        assert_eq!(resolve("Contested Name"), Some(first.to_string()));
    }

    /// 子集名（带 `#folio`）不进表：那是某本书某个字符集的私有名字。
    #[test]
    fn a_subset_name_is_not_recorded() {
        note_family_aliases("Book Face#folio0003", &["Book Face#folio0003".into()]);
        assert_eq!(resolve("Book Face#folio0003"), None);
    }

    /// 归一口径：只留字母数字转小写；纯标点的名字归空。
    #[test]
    fn normalization_keeps_alphanumerics_only() {
        assert_eq!(normalize_family("Droid Sans Fallback"), "droidsansfallback");
        assert_eq!(normalize_family("小米兰亭-Pro.v2"), "小米兰亭prov2");
        assert_eq!(normalize_family("---"), "");
    }
}
