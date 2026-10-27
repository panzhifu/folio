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
use folio_core::content::format::css::{FontFaceRule, FontSrc, RuleSet};
use folio_core::content::format::{FontBytes, FontRef};
use gpui_kit::TextSystem;

use super::measure::is_generic;

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

/// 开书那一段的一次性计时，随 [`BookFonts::for_document`] 一起交回 `Book::new`。
///
/// 与帧计时分开：`Book::new` 不在 `ReaderSession::render` 里，`perf::reset()` 会把
/// 帧账清掉。这些数只在 `folio_app::perf` 开到 DEBUG 时才会被 `perf::log_book_open`
/// 打出来。
#[derive(Clone, Copy, Debug, Default)]
pub struct FontLoadStats {
    /// `RuleSet::parse` 那一趟（`for_document` 自己的第一遍 CSS 解析）。
    pub parse_ns: u64,
    /// `doc.fonts(&wanted)` 那一趟（含容器解压）。
    pub bytes_ns: u64,
    /// 逐份 `add_fonts` + 前后两次 `all_font_names`，以及设备字体别名那一趟。
    pub register_ns: u64,
    /// `doc.fonts` 交回来的份数。
    pub faces: usize,
    /// 别名缓存命中的份数（命中时**不读字节**）。
    pub cache_hits: usize,
    /// 真正读了字节的份数。
    pub bytes_read: usize,
    /// `add_fonts` 返回错误的份数（字体损坏 / fontdb 认不出）。
    pub failed: usize,
    /// `add_fonts` 成功但 `all_font_names` 没有新增名字的份数。
    ///
    /// 真机与桌面正常应为 0；测试文本系统可能整类忽略 `add_fonts`，这时它会等于份数。
    pub empty_diff: usize,
}

/// 字体身份：容器里那条资源的内容身份，不是 `href`。
///
/// `href` 在两本不同的书里可以指向不同的字节（多看模板的 `Fonts/kt.ttf` 就是例子）；
/// 进程级别名缓存拿它当键会让第二本书认到第一本的内部家族名。EPUB 从中央目录的
/// `crc32 + raw_len` 取，MOBI 对已解出的字节现算。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct FontId {
    crc32: u32,
    raw_len: u64,
}

impl FontId {
    fn of(face: &FontBytes) -> Self {
        Self {
            crc32: face.crc32,
            raw_len: face.raw_len,
        }
    }

    fn of_ref(font: &FontRef) -> Self {
        Self {
            crc32: font.crc32,
            raw_len: font.raw_len,
        }
    }
}

/// CSS 里折出来的字体请求：wanted 路径、声明名映射、`@font-face` 原表。
///
/// 在 UI 线程 `author_rules` 之后建一次，随 `FontLoadJob` 进 worker；worker 不再
/// 解析第二遍 CSS。`faces` 给设备字体别名那一趟用。
#[derive(Clone, Debug, Default)]
pub struct FontRequest {
    pub wanted: Vec<String>,
    pub declared_of: HashMap<String, String>,
    pub faces: Vec<FontFaceRule>,
}

impl FontRequest {
    pub fn from_rules(rules: &RuleSet) -> Self {
        let mut declared_of: HashMap<String, String> = HashMap::new();
        let mut wanted: Vec<String> = Vec::new();
        for face in &rules.font_faces {
            for src in &face.src {
                let FontSrc::Url(href) = src else {
                    // `local(...)` 与设备路径（`res://…`）没有字节可注册，见系统别名那一趟。
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
        Self {
            wanted,
            declared_of,
            faces: rules.font_faces.clone(),
        }
    }
}

/// 开书时交给首趟排版作业的字体准备任务。
///
/// 只在排版队列（单线程）里跑一次：第一次 `run` 读字节并注册，结果缓存进 `result`；
/// 后续章节/窗口里的 `Document` 再拿到同一个 `Arc<FontLoadJob>` 时直接复用，不会重复
/// 解压，也不会重复 `add_fonts`。它必须是 `Send + Sync`：随 `TypesetInput` 搬进 worker。
pub struct FontLoadJob {
    doc: Arc<dyn Document>,
    request: FontRequest,
    result: Mutex<Option<(Arc<BookFonts>, FontLoadStats)>>,
}

impl FontLoadJob {
    pub fn new(doc: Arc<dyn Document>, request: FontRequest) -> Self {
        Self {
            doc,
            request,
            result: Mutex::new(None),
        }
    }

    /// 跑一次（或取第一次的结果）。`texts` 是排版作业手里那份 `Arc<TextSystem>`。
    pub fn run(&self, texts: &TextSystem) -> (Arc<BookFonts>, FontLoadStats) {
        let mut result = self
            .result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((fonts, stats)) = result.as_ref() {
            return (fonts.clone(), *stats);
        }
        let (fonts, stats) = BookFonts::for_request(texts, self.doc.as_ref(), &self.request);
        super::perf::log_font_load(
            stats.parse_ns,
            stats.bytes_ns,
            stats.register_ns,
            stats.faces,
            stats.cache_hits,
            stats.bytes_read,
            stats.failed,
            stats.empty_diff,
        );
        *result = Some((fonts.clone(), stats));
        (fonts, stats)
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
    /// 结果按**内容身份**（`crc32 + raw_len`）存进进程级缓存（见 [`registered`]）：同一份
    /// 字体重新打开一本书时会**再注册一次**，而 `all_font_names()` 是去重的 —— 第二次的
    /// 增量是空，只靠 diff 会以为「这本书没有内嵌字体」。缓存让每次开书都拿得到第一次
    /// 算出来的映射。**不能按 `href` 存**：两本书的同一路径可以是不同字节。
    /// 从一本书的 CSS 与容器建别名表。同步入口：测试与 `font_load_bench` 用；生产开书
    /// 走 [`for_texts`](Self::for_texts) 的作业版本（`FontLoadJob`），避免在 UI 线程读字节。
    #[cfg(test)]
    pub fn for_document(
        doc: &dyn Document,
        css: &str,
        cx: &gpui_kit::App,
    ) -> (Arc<BookFonts>, FontLoadStats) {
        Self::for_texts(cx.text_system(), doc, css)
    }

    /// 同 [`for_document`](Self::for_document)，但文本系统由调用方给 —— 排版 worker
    /// 手里只有 `Arc<TextSystem>`，没有 `App`。同步路径保留给测试与 `font_load_bench`。
    #[cfg(test)]
    pub fn for_texts(
        texts: &TextSystem,
        doc: &dyn Document,
        css: &str,
    ) -> (Arc<BookFonts>, FontLoadStats) {
        let parse = super::perf::Timer::start();
        let rules = RuleSet::parse(css, folio_core::content::format::css::Origin::Author);
        let parse_ns = parse.ns();
        let request = FontRequest::from_rules(&rules);
        let (fonts, stats) = Self::for_request(texts, doc, &request);
        (fonts, FontLoadStats { parse_ns, ..stats })
    }

    /// 用 UI 已经折好的 [`FontRequest`] 建别名表。worker 只做三件事：
    /// 按身份查别名缓存 → 未命中才读这一份字节 → 注册。
    ///
    /// EPUB 走 `doc.font_refs`（中央目录元数据，不解压）；没有元数据入口的 provider
    /// 返回 `None`，这里退回 `doc.fonts` 一次读全（仍按身份缓存）。
    pub fn for_request(
        texts: &TextSystem,
        doc: &dyn Document,
        request: &FontRequest,
    ) -> (Arc<BookFonts>, FontLoadStats) {
        let mut stats = FontLoadStats::default();
        let mut register = None;
        let mut subtract_bytes = false;
        let mut fonts = Self::empty();
        if !request.wanted.is_empty() {
            if let Some(refs) = doc.font_refs(&request.wanted) {
                stats.faces = refs.len();
                let timer = super::perf::Timer::start();
                subtract_bytes = true;
                for reference in refs {
                    let Some(declared) = request.declared_of.get(&reference.href).cloned() else {
                        continue;
                    };
                    let id = FontId::of_ref(&reference);
                    let names = match cached_names(id) {
                        Some(names) => {
                            stats.cache_hits += 1;
                            names
                        }
                        None => {
                            let read = super::perf::Timer::start();
                            let Some(bytes) = doc.read_font(&reference) else {
                                stats.failed += 1;
                                continue;
                            };
                            stats.bytes_ns += read.ns();
                            stats.bytes_read += 1;
                            let before = texts.all_font_names();
                            // 注册失败（字体损坏 / fontdb 认不出）只丢这一份，不拦其余的。
                            if texts.add_fonts(vec![Cow::Owned(bytes)]).is_err() {
                                stats.failed += 1;
                                continue;
                            }
                            // 字体表真的变宽了：让后续建的表都带上新版本。
                            face_registered();
                            let added: Vec<String> = texts
                                .all_font_names()
                                .into_iter()
                                .filter(|name| !before.iter().any(|old| old == name))
                                .collect();
                            if added.is_empty() {
                                stats.empty_diff += 1;
                            }
                            store_names(id, &added);
                            added
                        }
                    };
                    if !names.is_empty() {
                        fonts.insert(declared, names);
                    }
                }
                register = Some(timer);
            } else {
                // 没有元数据入口（MOBI 等）：退回一次读全，仍按身份缓存注册结果。
                let bytes = super::perf::Timer::start();
                let faces = doc.fonts(&request.wanted);
                stats.bytes_ns = bytes.ns();
                stats.faces = faces.len();
                stats.bytes_read = faces.len();
                let timer = super::perf::Timer::start();
                for face in faces {
                    let Some(declared) = request.declared_of.get(&face.href).cloned() else {
                        continue;
                    };
                    let id = FontId::of(&face);
                    let names = match cached_names(id) {
                        Some(names) => {
                            stats.cache_hits += 1;
                            names
                        }
                        None => {
                            let before = texts.all_font_names();
                            if texts.add_fonts(vec![Cow::Owned(face.bytes)]).is_err() {
                                stats.failed += 1;
                                continue;
                            }
                            face_registered();
                            let added: Vec<String> = texts
                                .all_font_names()
                                .into_iter()
                                .filter(|name| !before.iter().any(|old| old == name))
                                .collect();
                            if added.is_empty() {
                                stats.empty_diff += 1;
                            }
                            store_names(id, &added);
                            added
                        }
                    };
                    if !names.is_empty() {
                        fonts.insert(declared, names);
                    }
                }
                register = Some(timer);
            }
        }
        let register = register.unwrap_or_else(super::perf::Timer::start);

        // 第二趟：书里点名用**设备字体**的那一类（`local("Noto Serif")`、
        // `url(res:///system/fonts/DroidSansFallback.ttf)`）。字节不随书来，只按家族名
        // 或文件名去设备已注册的家族里找同一张脸。容器字体先登记，这一趟只补空。
        let needs_system = request.faces.iter().any(|face| {
            fonts.families_for(&face.family).is_none()
                && face
                    .src
                    .iter()
                    .any(|src| matches!(src, FontSrc::Local(_) | FontSrc::System(_)))
        });
        if needs_system {
            let known = texts.all_font_names();
            for (declared, family) in system_aliases(&request.faces, &known) {
                if fonts.families_for(&declared).is_none() {
                    fonts.insert(declared, vec![family]);
                }
            }
        }

        // 版本取**建完之后**那个值：这一趟自己注册的字体也算「字体表变了」。
        // 取建之前的值就漏了 —— 下一本书的字体集不同，却可能拿到与这一本相同的版本号，
        // 于是块级缓存把按旧字体表算出的行宽当成作数的。
        fonts.revision = face_revision();
        let register_ns = register.ns();
        stats.register_ns = if subtract_bytes {
            register_ns.saturating_sub(stats.bytes_ns)
        } else {
            register_ns
        };
        (Arc::new(fonts), stats)
    }
}

/// `@font-face` 里指向**设备字体**的那些声明 → 这台机器上真有的家族名。
///
/// 只看 [`FontSrc::Local`] 与 [`FontSrc::System`]：容器字体归注册那一趟管，
/// 这里给它让位。一个声明名有多条设备来源时取第一条真能落地的；同一个声明名
/// 在多个 `@font-face` 里重复出现时只留第一条。
fn system_aliases(faces: &[FontFaceRule], known: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for face in faces {
        let found = face.src.iter().find_map(|src| match src {
            FontSrc::Local(name) => system_family(name, known),
            FontSrc::System(path) => system_family(path, known),
            FontSrc::Url(_) => None,
        });
        let Some(family) = found else {
            continue;
        };
        if !out.iter().any(|(declared, _)| declared == &face.family) {
            out.push((face.family.clone(), family));
        }
    }
    out
}

/// 一条设备字体提示 → 已装家族名。
///
/// 两种形态都走这里：`local(...)` 给的家族名（先按整串比），以及 `res://…` /
/// `/system/fonts/…` 给的文件路径（去掉目录、扩展名与 `-Regular` 这类字面后缀，
/// 再按驼峰拆成 `Droid Sans Fallback` 这种家族名形态）。
///
/// 匹配顺序：忽略大小写与非字母数字的**全等** → 唯一前缀。前缀只在提示够长时启用，
/// 且必须唯一 —— 一份 `NotoSansCJK-Regular` 对着一台装着简繁两套 Noto CJK 的机器
/// 是不该猜的。
fn system_family(hint: &str, known: &[String]) -> Option<String> {
    let hint = hint.trim();
    if hint.is_empty() || is_generic(hint) {
        return None;
    }
    if let Some(found) = match_known(hint, known) {
        return Some(found);
    }
    let stem = file_stem(hint);
    if stem.is_empty() || stem == hint {
        return None;
    }
    name_candidates(stem)
        .into_iter()
        .find_map(|candidate| match_known(&candidate, known))
}

/// 去掉目录与字体扩展名。`res:///system/fonts/DroidSansFallback.ttf` →
/// `DroidSansFallback`；没有扩展名的家族名原样返回。
fn file_stem(hint: &str) -> &str {
    let name = hint.rsplit(['/', '\\']).next().unwrap_or(hint);
    match name.rsplit_once('.') {
        Some((stem, ext))
            if ["ttf", "otf", "ttc", "otc", "woff", "woff2"]
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known)) =>
        {
            stem.trim()
        }
        _ => name.trim(),
    }
}

/// 文件名可能怎么读成家族名：原样、驼峰拆开、去掉字面后缀、两者都做。
fn name_candidates(stem: &str) -> Vec<String> {
    let spaced = stem.replace(['-', '_'], " ");
    let camel = split_camel(&spaced);
    let stripped = strip_style_words(&spaced);
    let camel_stripped = split_camel(&stripped);
    let mut out = vec![stem.to_string(), camel, stripped, camel_stripped];
    out.dedup();
    out
}

/// `DroidSansFallback` → `Droid Sans Fallback`。连续大写（`CJK`）不拆。
fn split_camel(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (index, ch) in chars.iter().copied().enumerate() {
        if ch.is_uppercase() && index > 0 {
            let prev = chars[index - 1];
            let next_is_lower = chars.get(index + 1).is_some_and(|next| next.is_lowercase());
            if prev.is_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_uppercase() && next_is_lower)
            {
                out.push(' ');
            }
        }
        out.push(ch);
    }
    out
}

/// 去掉 `Regular` / `Bold` / `Italic` 这一类**字面**后缀：文件名里的它们不参与家族名。
fn strip_style_words(name: &str) -> String {
    const STYLE_WORDS: &[&str] = &[
        "regular",
        "bold",
        "italic",
        "oblique",
        "light",
        "medium",
        "black",
        "thin",
        "semibold",
        "extrabold",
        "extralight",
        "heavy",
        "book",
    ];
    name.split_whitespace()
        .filter(|part| {
            !STYLE_WORDS
                .iter()
                .any(|style| style.eq_ignore_ascii_case(part))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 忽略大小写、空格、连字符的家族名比较；前缀只在唯一命中时才算。
fn match_known(hint: &str, known: &[String]) -> Option<String> {
    let normalized = normalize(hint);
    if normalized.len() < 2 {
        return None;
    }
    if let Some(found) = known.iter().find(|name| normalize(name) == normalized) {
        return Some(found.clone());
    }
    // 太短的提示（`A` / `SG`）拿去做前缀会撞上半个字体表。
    if normalized.len() < 4 {
        return None;
    }
    let mut by_prefix = known
        .iter()
        .filter(|name| normalize(name).starts_with(&normalized));
    if let Some(first) = by_prefix.next()
        && by_prefix.next().is_none()
    {
        return Some(first.clone());
    }
    let mut by_suffix = known
        .iter()
        .filter(|name| normalized.starts_with(&normalize(name)));
    if let Some(first) = by_suffix.next()
        && by_suffix.next().is_none()
    {
        return Some(first.clone());
    }
    None
}

fn normalize(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// 进程级登记表：字体内容身份 → 注册成功后设备新增的家族名。
///
/// 必需而非优化：`all_font_names()` 去重，同一份字体第二次注册拿不到增量，只靠 diff
/// 会把「这本书带内嵌字体」误判成没有。进程内每份字体只算一次。
///
/// **键必须是内容身份，不能是 `href`**：`href` 只是容器里的路径，两本不同的书可以有
/// 同一条 `OEBPS/Fonts/kt.ttf` 而装着不同字节；拿路径当键会让第二本书跳过注册，并把
/// 第一本的内部家族名登记到第二本的声明名下。身份取中央目录的 `crc32 + raw_len`，
/// 见 [`FontId`]；这是快筛，不是密码学哈希（碰撞概率极低，但不是零）。
fn registered() -> &'static Mutex<HashMap<FontId, Vec<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<FontId, Vec<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_names(id: FontId) -> Option<Vec<String>> {
    registered()
        .lock()
        .ok()
        .and_then(|cache| cache.get(&id).cloned())
}

fn store_names(id: FontId, names: &[String]) {
    if let Ok(mut cache) = registered().lock() {
        cache.insert(id, names.to_vec());
    }
}

#[cfg(test)]
mod tests {
    //! 别名表本身是纯数据结构，宿主上直接钉。`for_document` 那一条要真容器与真文本系统，
    //! 归到集成那一档，不在这里假装测到。

    use super::{
        BookFonts, FontId, FontRequest, cached_names, store_names, system_aliases, system_family,
    };
    use folio_core::content::format::FontBytes;
    use folio_core::content::format::css::{Origin, RuleSet};

    fn installed() -> Vec<String> {
        vec![
            "Droid Sans Fallback".into(),
            "Noto Serif".into(),
            "Noto Sans CJK SC".into(),
            "Noto Sans CJK TC".into(),
            "LXGW WenKai".into(),
        ]
    }

    /// 设备字体路径按文件名找家族：驼峰拆开、去掉 `-Bold` 这类字面后缀；
    /// 家族名本身先按整串比。泛族名与对不上的路径都不许瞎猜。
    #[test]
    fn a_device_font_hint_maps_to_an_installed_family() {
        let known = installed();
        assert_eq!(
            system_family("res:///system/fonts/DroidSansFallback.ttf", &known).as_deref(),
            Some("Droid Sans Fallback")
        );
        assert_eq!(
            system_family("/system/fonts/NotoSerif-Bold.otf", &known).as_deref(),
            Some("Noto Serif")
        );
        assert_eq!(
            system_family("Droid Sans Fallback", &known).as_deref(),
            Some("Droid Sans Fallback")
        );
        assert_eq!(
            system_family("notoserif-regular", &known).as_deref(),
            Some("Noto Serif"),
            "小写、带字面后缀也认"
        );
        assert_eq!(
            system_family("res:///system/fonts/NoSuchFace.ttf", &known),
            None
        );
        assert_eq!(system_family("serif", &known), None, "泛族名不映射");
    }

    /// 斗战狂潮那一条：`@font-face` 的 `src` 全是设备路径时，用文件名给声明名挂上
    /// 设备上的那个家族。两条 `res://` 指向同一张脸，别名只出一条。
    #[test]
    fn a_face_pointing_at_a_system_font_gets_an_alias() {
        let rules = RuleSet::parse(
            "@font-face { font-family: easypub; src: \
                 url(res:///system/fonts/DroidSansFallback.ttf), \
                 url(res:///ebook/fonts/../../system/fonts/DroidSansFallback.ttf) }",
            Origin::Author,
        );
        assert_eq!(rules.font_faces[0].src.len(), 2, "两条设备来源都要留着");
        assert_eq!(
            system_aliases(&rules.font_faces, &installed()),
            vec![("easypub".to_string(), "Droid Sans Fallback".to_string())]
        );
    }

    /// `local("…")` 指名道姓要设备上的一张脸；容器字体归注册那一趟，不走这里。
    #[test]
    fn local_and_container_sources_are_handled_separately() {
        let local = RuleSet::parse(
            "@font-face { font-family: Body; src: local(\"Noto Serif\") }",
            Origin::Author,
        );
        assert_eq!(
            system_aliases(&local.font_faces, &installed()),
            vec![("Body".to_string(), "Noto Serif".to_string())]
        );

        let container = RuleSet::parse(
            "@font-face { font-family: Body; src: url(Fonts/body.otf) }",
            Origin::Author,
        );
        assert!(
            system_aliases(&container.font_faces, &installed()).is_empty(),
            "容器字体不该被系统那一趟抢走"
        );
    }

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

    fn face(href: &str, crc32: u32, raw_len: u64) -> FontBytes {
        FontBytes {
            href: href.into(),
            media_type: "font/ttf".into(),
            bytes: Vec::new(),
            raw_len,
            crc32,
        }
    }

    /// 别名缓存的身份是内容，不是 `href`：两本书的同一路径装着不同字节时必须分开。
    #[test]
    fn the_font_identity_is_content_not_href() {
        let a = face("OEBPS/Fonts/kt.ttf", 0x1111_1111, 100);
        let b = face("OEBPS/Fonts/kt.ttf", 0x2222_2222, 100);
        let a_again = face("OEBPS/Fonts/kt.ttf", 0x1111_1111, 100);
        assert_ne!(FontId::of(&a), FontId::of(&b), "同一路径不同字节必须分开");
        assert_eq!(FontId::of(&a), FontId::of(&a_again));
    }

    /// 进程缓存也已按身份分区；拿同一 href 的另一份字节查，不能命中第一本的家族名。
    #[test]
    fn the_alias_cache_does_not_share_names_across_identities() {
        let a = face("OEBPS/Fonts/kt.ttf", 0xAAAA_0001, 101);
        let b = face("OEBPS/Fonts/kt.ttf", 0xBBBB_0001, 101);
        store_names(FontId::of(&a), &["Family A".to_string()]);
        assert_eq!(
            cached_names(FontId::of(&a)),
            Some(vec!["Family A".to_string()])
        );
        assert_eq!(
            cached_names(FontId::of(&b)),
            None,
            "另一份字节不能拿到第一本的家族名"
        );
    }

    /// `FontRequest` 只把 `url()` 当容器路径；`local()`/系统路径归设备别名那一趟。
    #[test]
    fn a_font_request_keeps_only_url_sources() {
        let rules = RuleSet::parse(
            "@font-face { font-family: A; src: url(x.ttf), local(Noto Serif) } \
             @font-face { font-family: B; src: local(Noto Sans) }",
            Origin::Author,
        );
        let request = FontRequest::from_rules(&rules);
        assert_eq!(request.wanted, vec!["x.ttf".to_string()]);
        assert_eq!(
            request.declared_of.get("x.ttf").map(String::as_str),
            Some("A")
        );
        assert_eq!(request.faces.len(), 2);
    }

    /// 第二次打开同一本 HP：身份缓存命中，`read_font` 一次都不调。
    ///
    /// 忽略：真书只在 `tests/fixtures/local/`，不入库。手动跑：
    /// `cargo test -p folio-app --lib -- --ignored --nocapture the_second_open_reads_no_font_bytes`
    #[ignore = "真书 HP 不在仓库；手动跑"]
    #[gpui_kit::test]
    fn the_second_open_reads_no_font_bytes(cx: &mut gpui_kit::TestAppContext) {
        use std::path::PathBuf;

        cx.update(gpui_kit::init);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/local/hali-bote-quanji.epub");
        if !path.is_file() {
            eprintln!("{} 不在，跳过", path.display());
            return;
        }
        let doc = folio_core::content::document::open(&path).expect("打开 HP");
        let css = doc.stylesheet();
        let rules = RuleSet::parse(&css, Origin::Author);
        let request = FontRequest::from_rules(&rules);
        let (_, first) =
            cx.update(|cx| BookFonts::for_request(cx.text_system(), doc.as_ref(), &request));
        let (_, second) =
            cx.update(|cx| BookFonts::for_request(cx.text_system(), doc.as_ref(), &request));
        assert_eq!(first.faces, 2, "HP 有两份内嵌字体");
        assert_eq!(first.bytes_read, 2, "第一次要读两份字节");
        assert_eq!(second.bytes_read, 0, "第二次命中身份缓存，不该再读字节");
        assert_eq!(second.cache_hits, 2, "两份都该命中");
    }

    /// 真实容器 + 真实文本系统的开书计时；只在手动读数字时跑。
    ///
    /// 跑法：
    /// ```text
    /// cargo test -p folio-app --lib --release -- --ignored --nocapture font_load_bench
    /// ```
    #[ignore = "量墙钟，只在读数字时手动跑"]
    #[gpui_kit::test]
    fn font_load_bench(cx: &mut gpui_kit::TestAppContext) {
        use std::time::Instant;

        cx.update(gpui_kit::init);
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/local/hali-bote-quanji.epub");
        if !path.is_file() {
            eprintln!("{} 不在，跳过（真书不入库）", path.display());
            return;
        }
        let doc = folio_core::content::document::open(&path).expect("打开 HP");
        let css = doc.stylesheet();
        // 先把 href 对一遍：如果 CSS 的 wanted 和容器回来的 href 不同形，别名表空是路径
        // 对不上，不是注册失败。
        let rules = RuleSet::parse(&css, Origin::Author);
        let mut wanted: Vec<String> = Vec::new();
        for face in &rules.font_faces {
            for src in &face.src {
                if let folio_core::content::format::css::FontSrc::Url(href) = src
                    && !wanted.iter().any(|existing| existing == href)
                {
                    wanted.push(href.clone());
                }
            }
        }
        let faces = doc.fonts(&wanted);
        println!(
            "wanted={wanted:?}\nfaces={:?}",
            faces
                .iter()
                .map(|face| (
                    face.href.as_str(),
                    face.bytes.len(),
                    face.crc32,
                    face.raw_len
                ))
                .collect::<Vec<_>>()
        );
        for round in 1..=3 {
            let started = Instant::now();
            let (fonts, stats) = cx.update(|cx| BookFonts::for_document(doc.as_ref(), &css, cx));
            let total_us = started.elapsed().as_micros();
            println!(
                "round {round}: total_us={total_us} parse_us={} bytes_us={} register_us={} faces={} failed={} empty_diff={} families={}",
                stats.parse_ns / 1000,
                stats.bytes_ns / 1000,
                stats.register_ns / 1000,
                stats.faces,
                stats.failed,
                stats.empty_diff,
                fonts.families.len(),
            );
            // `TestAppContext` 的文本系统可能整类忽略 `add_fonts`（`families=0`、
            // `empty_diff=faces`）；这条 ignored 探针只负责把数打出来，真机注册耗时要
            // 在设备上量，不在宿主测试里假装测到。
        }
    }
}
