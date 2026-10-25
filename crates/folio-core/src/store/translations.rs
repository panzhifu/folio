//! 翻译缓存：同一段字、同一对语言、同一个引擎的答案不查第二次。
//!
//! 与批注 / 书签不同，这张表**不描述读者做过的任何事**，只是外部请求的备忘 —— 所以
//! 它没有时间线（「最近」无意义）、没有跨书查询（答案与书无关）、也不进「笔记」那一屏。
//! 唯一的读法是「这条键命不命中」，唯一的写法是「把答案按键放回去」。
//!
//! 表结构与键的取法见 `schema.rs` 的 v24。上限见 [`TRANSLATIONS_MAX`]：缓存是有上限的
//! 资源，写满就挤最旧的 —— 不设上限的话，这张表会随读书量无界地长，而它里面没有一行
//! 是不能再生成的。

use rusqlite::{OptionalExtension, params};

use super::{Store, now};
use crate::Result;

/// 缓存条数上限。2000 条按「一段 500 字、每条连键带答案约 1 KB」估，全表约 2 MB ——
/// 对一本几 MB 的书而言是可以忽略的量级，而对「反复看同一段译文」的读者足够用很久。
pub const TRANSLATIONS_MAX: i64 = 2000;

impl Store {
    /// 按键取缓存。没命中给 `None` —— 调用方该出网了。
    pub fn cached_translation(
        &self,
        provider: &str,
        source_lang: &str,
        target_lang: &str,
        source_hash: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let text: Option<String> = conn
            .query_row(
                "SELECT target_text FROM translations
                 WHERE provider = ?1 AND source_lang = ?2 AND target_lang = ?3 AND source_hash = ?4",
                params![provider, source_lang, target_lang, source_hash],
                |r| r.get(0),
            )
            .optional()?;
        Ok(text)
    }

    /// 把一条答案按键放回，并顺手把超额的最旧几条挤出去。
    ///
    /// 同键重写用 `REPLACE`：答案理论上不会变，但引擎悄悄改版（或者我们修了解析）
    /// 之后重翻一条旧文，新答案该顶掉旧答案 —— 缓存永远装的是「最近一次问到的」。
    pub fn save_translation(
        &self,
        provider: &str,
        source_lang: &str,
        target_lang: &str,
        source_hash: &str,
        target_text: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT OR REPLACE INTO translations
             (provider, source_lang, target_lang, source_hash, target_text, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![provider, source_lang, target_lang, source_hash, target_text, now()],
        )?;
        // 修剪放在写的那一趟而不是后台作业：这张表只在翻译落笔时才长，就地清一次
        // 是一条小 DELETE，省掉一套定时任务的复杂度。
        conn.execute(
            "DELETE FROM translations WHERE id IN (
                 SELECT id FROM translations
                 WHERE id NOT IN (
                     SELECT id FROM translations ORDER BY created_at DESC, id DESC LIMIT ?1
                 )
             )",
            params![TRANSLATIONS_MAX],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! 翻译缓存：键的四元组精确命中、同键重写、超额修剪。
    //! 与 `annotations` / `bookmarks` 那两组测试同一套办法。

    use crate::Store;

    fn store() -> Store {
        Store::in_memory().unwrap()
    }

    #[test]
    fn a_translation_round_trips_under_its_own_key() {
        let store = store();
        store
            .save_translation("google", "auto", "zh-CN", "hash-1", "于是我们奋力向前划")
            .unwrap();

        let hit = store
            .cached_translation("google", "auto", "zh-CN", "hash-1")
            .unwrap()
            .expect("刚存的该命中");
        assert_eq!(hit, "于是我们奋力向前划");

        // 四元组任何一格不同都算另一条 —— 尤其是引擎与目标语，换了它们答案就该换。
        assert!(
            store
                .cached_translation("google", "auto", "zh-CN", "hash-2")
                .unwrap()
                .is_none(),
            "另一段原文不该命中"
        );
        assert!(
            store
                .cached_translation("google", "auto", "en", "hash-1")
                .unwrap()
                .is_none(),
            "另一种目标语不该命中"
        );
        assert!(
            store
                .cached_translation("deepl", "auto", "zh-CN", "hash-1")
                .unwrap()
                .is_none(),
            "另一个引擎不该命中"
        );
    }

    /// 同键重写：新答案顶掉旧答案，表里不多一行。
    #[test]
    fn saving_again_replaces_rather_than_appends() {
        let store = store();
        store
            .save_translation("google", "auto", "zh-CN", "hash-1", "旧答案")
            .unwrap();
        store
            .save_translation("google", "auto", "zh-CN", "hash-1", "新答案")
            .unwrap();

        assert_eq!(
            store
                .cached_translation("google", "auto", "zh-CN", "hash-1")
                .unwrap()
                .as_deref(),
            Some("新答案"),
            "重写该顶掉旧答案"
        );
    }

    /// 超额修剪：写满上限之后再写，最旧的被挤出去，最新的全都在。
    /// 命中与否本身就是表内容的完整读法，逐条问就够了，不必开裸连接。
    #[test]
    fn the_cache_stays_within_its_cap() {
        let store = store();
        let cap = super::TRANSLATIONS_MAX;
        // 先放满 cap 条，再放一条新的：最早那条（hash-0）该被挤出去。
        for i in 0..cap {
            store
                .save_translation("google", "auto", "zh-CN", &format!("hash-{i}"), "答案")
                .unwrap();
        }
        store
            .save_translation("google", "auto", "zh-CN", "hash-new", "答案")
            .unwrap();

        assert!(
            store
                .cached_translation("google", "auto", "zh-CN", "hash-0")
                .unwrap()
                .is_none(),
            "最旧那条该被挤出去"
        );
        assert!(
            store
                .cached_translation("google", "auto", "zh-CN", "hash-new")
                .unwrap()
                .is_some(),
            "最新的那条必须在"
        );
        // 同一秒写入的条目 created_at 相同，靠 id 兜底定序：中段那些必须仍在。
        assert!(
            store
                .cached_translation("google", "auto", "zh-CN", &format!("hash-{}", cap - 1))
                .unwrap()
                .is_some()
        );
    }
}
