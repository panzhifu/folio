//! 这本书的**内嵌字体别名表**：CSS 声明的家族名 → 这台设备上真注册着的家族名。
//!
//! 存在理由只有一条：书的 `@font-face` 声明的名字（`Book Body`、`easypub` …）与
//! fontdb 从字体文件里读出来的内部家族名**不是一回事**，而 gpui 的 `resolve_font`
//! 只认后者。度量与绘制都要把「作者点名的那个名字」翻译成「设备真有的那个名字」，
//! 两边共用这一份表，才不会出现「按 A 的宽度居中、按 B 的宽度绘制」。
//!
//! 它与 [`crate::ui::features::reader::model::measure::gpui_font`] 的分工：作者的
//! `font-family` 候选链里查得到这张表，**那份字体就当 primary**（随书带来的字节就是作者
//! 写明的正文用字，优先级排在「设备上恰好装着同名家族」之前）；读者的那一档整体降到
//! 补缺链上，只有书里那一份画不出来的那一格才轮到它（ASCII 永远留在 primary，
//! 那是 gpui 的规则）。
//!
//! 字节不进这里，也不进书库（见 `folio_core::content::format::FontBytes`）：每次开书按
//! `@font-face` 的 `src` 去容器现读一次，注册进进程级文本系统。任何一步失败都
//! 降级成「这本书没有内嵌字体」—— 与没有这个功能时逐字节同一条路径，正文照读者的字体排。

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use folio_core::content::document::Document;
use folio_core::content::format::css::{FontSrc, Origin, RuleSet};
use gpui_kit::App;

/// 进程级「字体注册事件」计数：每成功往文本系统里注册一批字体就 +1。
///
/// 它是排版失效键里 `fonts_rev` 那一格的来源（`folio_core::layout::LayoutKey`）。数的是
/// **事件**而不是「第几本书」：两本都没有内嵌字体的书拿到同一个版本（设备字体表确实没变，
/// 缓存不该白清），而新书带来新字体时版本跳一格，于是所有按旧字体表算出的行宽全部作废 ——
/// 那正是块级缓存接线前记成「已知口径缺口」的那一处。
static FACE_EVENTS: AtomicU32 = AtomicU32::new(0);

/// 当前字体表版本。
fn face_revision() -> u32 {
    FACE_EVENTS.load(Ordering::Relaxed)
}

/// 记一次「设备字体表变宽了」。只在 `add_fonts` 真成功之后调。
fn face_registered() {
    FACE_EVENTS.fetch_add(1, Ordering::Relaxed);
}

/// 声明名 → 设备家族名。`Send + Sync`：会在后台线程的度量里被拿着。
pub struct BookFonts {
    /// 一个声明名可能对应多份内部家族（一份字体文件带好几个名字），
    /// 第一个当 primary，其余留在补缺链上。
    families: HashMap<String, Vec<String>>,
    /// 建这张表时的字体表版本，见 [`FACE_EVENTS`]。
    revision: u32,
}

impl Default for BookFonts {
    /// 空表 + **当下**的字体表版本。默认值取当下而不是 0：一张表登记了什么不重要，
    /// 重要的是它建出来时设备上的字体是哪一批。
    fn default() -> Self {
        Self {
            families: HashMap::new(),
            revision: face_revision(),
        }
    }
}

impl BookFonts {
    /// 没有内嵌字体（Markdown / TXT，或容器丢了的那条降级路）。空表 = 逐字节同一条路径。
    pub fn empty() -> Self {
        Self::default()
    }

    /// 这张表对应的字体表版本，进排版失效键。
    pub fn revision(&self) -> u32 {
        self.revision
    }

    /// 登记一条别名。同名重复登记不会重复条目，`@font-face` 的多条 `src` 归到同一个家族。
    pub fn insert(&mut self, declared: String, families: Vec<String>) {
        let entry = self.families.entry(declared).or_default();
        for name in families {
            if !entry.iter().any(|existing| existing == &name) {
                entry.push(name);
            }
        }
    }

    /// 某个声明名注册到的内部家族名（按注册顺序）；没登记过时是 `None`。返回借用而不是
    /// 克隆，度量与绘制每一条 run 都要问。
    pub fn families_for(&self, name: &str) -> Option<&[String]> {
        self.families.get(name).map(Vec::as_slice)
    }

    /// 只有测试在调（钉「空表读不出别名」那一格），生产这一侧没人问。
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.families.is_empty()
    }

    /// 从一本书的 CSS 与容器建别名表：读 `@font-face` 的 `src` → 去容器取字节 →
    /// 注册进进程文本系统 → 按「注册前后家族名表之差」反推这个家族落到了哪个内部名下。
    ///
    /// 逐份注册、逐份 diff 而不是整批 diff：一份字体文件可能带好几个内部家族名，
    /// 一次只注册一份，新增的那几个名字就都属于**这一个**声明的家族（`@font-face`
    /// 声明里一个家族名对一份文件是常态）。face 数有上限（见 `format::epub`），
    /// 一次开书几趟枚举，不进渲染帧。
    ///
    /// 结果按 `href` 存进进程级缓存（见 [`registered`]）：同一份字体重新打开一本书时
    /// 会**再注册一次**，而 `all_font_names()` 是去重的 —— 第二次的增量是空，只靠 diff
    /// 会以为「这本书没有内嵌字体」。缓存让每次开书都拿得到第一次算出来的映射。
    pub fn for_document(doc: &dyn Document, css: &str, cx: &App) -> Arc<BookFonts> {
        let rules = RuleSet::parse(css, Origin::Author);
        // 一个 href 可能被多个家族声明引用（同一份字体给多个名字），逐个都记。
        let mut declared_of: HashMap<String, String> = HashMap::new();
        let mut wanted: Vec<String> = Vec::new();
        for face in &rules.font_faces {
            for src in &face.src {
                let FontSrc::Url(href) = src else {
                    // `local(...)` 引用设备已装字体，不需要注册字节。
                    continue;
                };
                declared_of
                    .entry(href.clone())
                    .or_insert_with(|| face.family.clone());
                if !wanted.iter().any(|w| w == href) {
                    wanted.push(href.clone());
                }
            }
        }
        if wanted.is_empty() {
            return Arc::new(Self::empty());
        }
        let faces = doc.fonts(&wanted);
        if faces.is_empty() {
            return Arc::new(Self::empty());
        }

        let texts = cx.text_system();
        let mut fonts = Self::empty();
        for face in faces {
            let Some(declared) = declared_of.get(&face.href).cloned() else {
                continue;
            };
            // 缓存命中就不再注册、不再 diff（重复注册拿不到增量，见本函数文档）。
            let names = match cached_names(&face.href) {
                Some(names) => names,
                None => {
                    let before = texts.all_font_names();
                    // 注册失败（字体损坏 / fontdb 认不出）只丢这一份，不拦其余的。
                    if texts.add_fonts(vec![Cow::Owned(face.bytes)]).is_err() {
                        continue;
                    }
                    // 字体表真的变宽了：让后续建的表都带上新版本。
                    face_registered();
                    let added: Vec<String> = texts
                        .all_font_names()
                        .into_iter()
                        .filter(|name| !before.iter().any(|old| old == name))
                        .collect();
                    store_names(face.href, &added);
                    added
                }
            };
            if !names.is_empty() {
                fonts.insert(declared, names);
            }
        }
        // 版本取**建完之后**那个值：这一趟自己注册的字体也算「字体表变了」。
        // 取建之前的值就漏了 —— 下一本书的字体集不同，却可能拿到与这一本相同的版本号，
        // 于是块级缓存把按旧字体表算出的行宽当成作数的。
        fonts.revision = face_revision();
        Arc::new(fonts)
    }
}

/// 进程级登记表：容器 href → 注册成功后设备新增的家族名。
///
/// 必需而非优化：`all_font_names()` 去重，同一份字体第二次注册拿不到增量，只靠 diff
/// 会把「这本书带内嵌字体」误判成没有。进程内每份字体只算一次；`href` 当键足够 ——
/// 同一本书的同一资源路径指向同一份字节。
fn registered() -> &'static Mutex<HashMap<String, Vec<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_names(href: &str) -> Option<Vec<String>> {
    registered()
        .lock()
        .ok()
        .and_then(|cache| cache.get(href).cloned())
}

fn store_names(href: String, names: &[String]) {
    if let Ok(mut cache) = registered().lock() {
        cache.insert(href, names.to_vec());
    }
}

#[cfg(test)]
mod tests {
    //! 别名表本身是纯数据结构，宿主上直接钉。`for_document` 那一条要真容器与真文本系统，
    //! 归到集成那一档，不在这里假装测到。

    use super::BookFonts;

    #[test]
    fn an_inserted_alias_is_read_back_and_unknown_names_are_absent() {
        let mut fonts = BookFonts::empty();
        assert!(fonts.is_empty());
        assert!(fonts.families_for("Book Body").is_none());

        fonts.insert("Book Body".into(), vec!["Noto Serif".into()]);
        assert_eq!(
            fonts.families_for("Book Body").unwrap(),
            &["Noto Serif".to_string()]
        );
        assert!(fonts.families_for("serif").is_none(), "泛族名不该凭空有链");
    }

    /// 同名重复登记（`@font-face` 的多条 `src`）不产生重复条目。
    #[test]
    fn repeated_inserts_do_not_duplicate_families() {
        let mut fonts = BookFonts::empty();
        fonts.insert("Book Body".into(), vec!["Noto Serif".into()]);
        fonts.insert(
            "Book Body".into(),
            vec!["Noto Serif".into(), "Noto Serif CJK".into()],
        );
        assert_eq!(
            fonts.families_for("Book Body").unwrap(),
            &["Noto Serif".to_string(), "Noto Serif CJK".to_string()]
        );
    }
}
