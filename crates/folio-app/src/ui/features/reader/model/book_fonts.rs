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
use read_fonts::TableProvider;
use read_fonts::collections::IntSet;
use read_fonts::types::{NameId, Tag};
use skera::{DEFAULT_DROP_TABLES, DEFAULT_LAYOUT_FEATURES, Plan, SubsetFlags};

use super::measure::is_generic;
use crate::ui::foundation::font_primary;

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
    /// 第一个能当 primary 的当 primary，其余留在补缺链上。
    families: HashMap<String, Vec<FaceName>>,
    /// 建这张表时的字体表版本，见 [`FACE_EVENTS`]。
    revision: u32,
}

/// 注册成功后设备报回来的一个内部家族名，以及它能不能当 gpui 的 **primary**。
///
/// 能不能当 primary 的判据只有一个：字体里有没有拉丁 `m` 的字形 —— gpui 的
/// `CosmicTextSystem::load_family` 会把没有 `m` 的脸从 primary 候选里剔掉
/// （gpui-pre-wgpu `cosmic_text_system.rs` 里那段 HACK，注释写着编辑器要量 `m` 的宽），
/// 剔完候选为空时 `resolve_font` 直接 panic。桌面端那一 panic 会被 fontconfig 的回退表
/// 接住，Android 上 gpui 的全局回退表（`.ZedMono`、Helvetica、Noto Sans…）一个都不存在，
/// 于是整台进程 SIGABRT —— 2026-10-30《诡秘之主》的 `sy.ttf`（只有 18 个字形：
/// 几个数字、方括号与十个汉字，没有拉丁 `m`）在 Meizu 20 上就是这么闪退的。
///
/// 判据必须与 gpui 那一句逐字同源（`charmap.map('m') == 0`）：我们放过去的家族它照剔，
/// 它剔空的那个家族我们不能挑。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceName {
    pub name: String,
    /// 字体里有拉丁 `m` 的字形。
    pub latin: bool,
}

impl FaceName {
    /// 设备字体：名字从 `all_font_names()` 里挑出来，`latin` 由调用方用
    /// `TextSystem::font_has_glyph` 真解析后给（见 `device_family_has_m`），不再按
    /// 进程登记表猜。
    fn device(name: String, latin: bool) -> Self {
        Self { name, latin }
    }
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

/// 字体准备那一段的一次性计时，随 [`FontLoadJob::run`] 的返回值交给日志与探针。
///
/// 与帧计时分开：`Book::new` 不在 `ReaderSession::render` 里，`perf::reset()` 会把
/// 帧账清掉。这些数只在 `folio_app::perf` 开到 DEBUG 时才会被 `perf::log_font_load`
/// 打出来。
#[derive(Clone, Copy, Debug, Default)]
pub struct FontLoadStats {
    /// `RuleSet::parse` 那一趟（`for_document` 自己的第一遍 CSS 解析）。
    pub parse_ns: u64,
    /// 字体身份那一趟：`font_refs_in`（复用共享容器）或 `font_refs`（自己开容器）。
    pub refs_ns: u64,
    /// `doc.fonts(&wanted)` 那一趟（含容器解压）。
    pub bytes_ns: u64,
    /// 子集化（skera）与改名那一趟。
    pub subset_ns: u64,
    /// 子集前后字节数（只算真切出来的那几份）。
    pub subset_bytes_before: usize,
    pub subset_bytes_after: usize,
    /// 想切但没切成的份数（TTC / skera 报错 / 改名失败）；这些走整份字体。
    pub subset_failures: usize,
    /// 逐份 `add_fonts` + 前后两次 `all_font_names`，以及设备字体别名那一趟。
    pub register_ns: u64,
    /// `doc.fonts` 交回来的份数。
    pub faces: usize,
    /// 别名缓存命中的份数（命中时**不读字节**）。
    pub cache_hits: usize,
    /// 真正读了字节的份数。
    pub bytes_read: usize,
    /// 读不到字节（`read_font` 回 `None`）或整批 `add_fonts` 失败的份数。
    pub failed: usize,
    /// 注册成功但没给这份字体找到任何一个 fontdb 报出来的家族名的份数。
    ///
    /// 真机与桌面正常应为 0（name 表与 fontdb 应当同形）；测试文本系统整类忽略
    /// `add_fonts` 时它会等于份数。
    pub empty_diff: usize,
}

/// 字体身份：容器里那条资源的内容身份，不是 `href`。
///
/// `href` 在两本不同的书里可以指向不同的字节（多看模板的 `Fonts/kt.ttf` 就是例子）；
/// 进程级别名缓存拿它当键会让第二本书认到第一本的内部家族名。EPUB 从中央目录的
/// `crc32 + raw_len` 取，MOBI 对原始 FONT 记录现算（不解压，见 `mobi::font_refs`）。
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

/// 别名缓存键：字体身份 + **子集标记**。
///
/// `subset == 0` 是整份字体；非 0 是「按这本书的字符集切出来的子集」的 hash。两张
/// 表必须分开：子集字体带唯一家族名（见 [`rename_subset_family`]），拿整份字体的
/// 缓存去填子集的声明名会让读者看到上一本书的缺字版本。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct AliasKey {
    font: FontId,
    subset: u64,
}

impl AliasKey {
    fn whole(font: FontId) -> Self {
        Self { font, subset: 0 }
    }

    fn subset(font: FontId, subset: u64) -> Self {
        Self {
            font,
            subset: subset | 1,
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
            stats.refs_ns,
            stats.subset_ns,
            stats.bytes_ns,
            stats.register_ns,
            stats.faces,
            stats.cache_hits,
            stats.bytes_read,
            stats.failed,
            stats.empty_diff,
            stats.subset_bytes_before,
            stats.subset_bytes_after,
            stats.subset_failures,
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
    /// 同一个家族名带着不同的 `latin` 登记两次时取或：只要有一张脸带 `m`，这个家族就能用。
    pub fn insert(&mut self, declared: String, families: Vec<FaceName>) {
        let entry = self.families.entry(declared).or_default();
        for face in families {
            match entry.iter_mut().find(|existing| existing.name == face.name) {
                Some(existing) => existing.latin |= face.latin,
                None => entry.push(face),
            }
        }
    }

    /// 某个声明名注册到的内部家族名（按注册顺序）；没登记过时是 `None`。返回借用而不是
    /// 克隆，度量与绘制每一条 run 都要问。
    pub fn families_for(&self, name: &str) -> Option<&[FaceName]> {
        self.families.get(name).map(Vec::as_slice)
    }

    /// 这个家族名能不能交给 gpui 当 primary。
    ///
    /// 两条都要：进程登记表里没被标成「没有拉丁 `m`」，以及**这本书自己的表**里同名
    /// 的脸至少有一张带 `m`（这张表可能是测试直接构造的，没走过注册登记）。
    /// 名字不在这本书的表里时按「设备字体」处理，由登记表说话。
    pub fn primary_usable(&self, name: &str) -> bool {
        let mut shipped = false;
        for face in self.families.values().flatten() {
            if face.name.eq_ignore_ascii_case(name) {
                shipped = true;
                if face.latin {
                    return true;
                }
            }
        }
        // 书里同名脸一张带 m 的都没有：不交。名字不在这本书的表里（设备字体）才问
        // 进程登记表 —— 那条路要拿锁，别让每次排版都走它。
        !shipped && font_primary::usable(name)
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
    /// 身份先走 `doc.font_refs_in`：开书读 CSS 那一趟已经把共享容器打开，这一问
    /// 复用同一个 ZIP、只补一遍 OPF；provider 没有共享容器这条路时退回
    /// `doc.font_refs`（自己开容器），再没有元数据入口才退回 `doc.fonts` 一次读全
    /// （仍按身份缓存）。
    pub fn for_request(
        texts: &TextSystem,
        doc: &dyn Document,
        request: &FontRequest,
    ) -> (Arc<BookFonts>, FontLoadStats) {
        let mut stats = FontLoadStats::default();
        let mut register = None;
        let mut subtract_bytes = false;
        let mut fonts = Self::empty();
        // 整本书的可见字符集（导入扫描顺手收的）。空 = 老库 / 非 EPUB：不切子集。
        let book_charset = doc.font_charset();
        let subset_charset = if book_charset.is_empty() {
            Vec::new()
        } else {
            charset_with_ascii(book_charset)
        };
        let subset_key = if subset_charset.is_empty() {
            0
        } else {
            charset_hash(&subset_charset)
        };
        if !request.wanted.is_empty() {
            let refs_timer = super::perf::Timer::start();
            let refs = doc
                .font_refs_in(&request.wanted)
                .or_else(|| doc.font_refs(&request.wanted));
            stats.refs_ns = refs_timer.ns();
            if let Some(refs) = refs {
                stats.faces = refs.len();
                let timer = super::perf::Timer::start();
                subtract_bytes = true;
                let mut pending: Vec<PendingFace> = Vec::new();
                for reference in refs {
                    let Some(declared) = request.declared_of.get(&reference.href).cloned() else {
                        continue;
                    };
                    let id = FontId::of_ref(&reference);
                    let first_key = if subset_key != 0 {
                        AliasKey::subset(id, subset_key)
                    } else {
                        AliasKey::whole(id)
                    };
                    if let Some(names) = cached_names(first_key) {
                        stats.cache_hits += 1;
                        if !names.is_empty() {
                            fonts.insert(declared, names);
                        }
                        continue;
                    }
                    let read = super::perf::Timer::start();
                    let Some(bytes) = doc.read_font(&reference) else {
                        stats.failed += 1;
                        continue;
                    };
                    stats.bytes_ns += read.ns();
                    stats.bytes_read += 1;
                    let (key, bytes) =
                        prepare_face(id, bytes, subset_key, &subset_charset, &mut stats);
                    // 子集没切成而整份字体已经在缓存里：别重复注册整份。
                    if key.subset == 0
                        && subset_key != 0
                        && let Some(names) = cached_names(key)
                    {
                        stats.cache_hits += 1;
                        if !names.is_empty() {
                            fonts.insert(declared, names);
                        }
                        continue;
                    }
                    pending.push(PendingFace {
                        key,
                        declared,
                        bytes,
                    });
                }
                let pending_len = pending.len();
                match register_batch(texts, pending) {
                    Some(results) => {
                        for (declared, names) in results {
                            if names.is_empty() {
                                stats.empty_diff += 1;
                                continue;
                            }
                            fonts.insert(declared, names);
                        }
                    }
                    // `add_fonts` 整批失败：这一批的读取都白付了，按份数记失败。
                    None => stats.failed += pending_len,
                }
                register = Some(timer);
            } else {
                // 没有元数据入口的 provider（CBZ / PDF 等）：退回一次读全，仍按身份缓存注册结果。
                let bytes = super::perf::Timer::start();
                let faces = doc.fonts(&request.wanted);
                stats.bytes_ns = bytes.ns();
                stats.faces = faces.len();
                stats.bytes_read = faces.len();
                let timer = super::perf::Timer::start();
                let mut pending: Vec<PendingFace> = Vec::new();
                for face in faces {
                    let Some(declared) = request.declared_of.get(&face.href).cloned() else {
                        continue;
                    };
                    let id = FontId::of(&face);
                    let first_key = if subset_key != 0 {
                        AliasKey::subset(id, subset_key)
                    } else {
                        AliasKey::whole(id)
                    };
                    if let Some(names) = cached_names(first_key) {
                        stats.cache_hits += 1;
                        if !names.is_empty() {
                            fonts.insert(declared, names);
                        }
                        continue;
                    }
                    let (key, bytes) =
                        prepare_face(id, face.bytes, subset_key, &subset_charset, &mut stats);
                    if key.subset == 0
                        && subset_key != 0
                        && let Some(names) = cached_names(key)
                    {
                        stats.cache_hits += 1;
                        if !names.is_empty() {
                            fonts.insert(declared, names);
                        }
                        continue;
                    }
                    pending.push(PendingFace {
                        key,
                        declared,
                        bytes,
                    });
                }
                let pending_len = pending.len();
                match register_batch(texts, pending) {
                    Some(results) => {
                        for (declared, names) in results {
                            if names.is_empty() {
                                stats.empty_diff += 1;
                                continue;
                            }
                            fonts.insert(declared, names);
                        }
                    }
                    None => stats.failed += pending_len,
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
                    // 设备字体手上没有字节，但字体系统有：真解析一遍有没有 `m`，
                    // 别再按名字假设它一定能当 primary。
                    let latin = device_family_has_m(texts, &family);
                    fonts.insert(declared, vec![FaceName::device(family, latin)]);
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

/// 设备字体有没有拉丁 `m`：真正解析（解析家族 → 加载 face → 查 cmap），不靠名字假设。
///
/// 字体可能在 `all_font_names()` 里但被平台从 primary 候选里剔掉（没有 `m` 的脸 ——
/// emoji / 符号字体），这一问答的就是「它能不能当 primary」。
fn device_family_has_m(texts: &TextSystem, family: &str) -> bool {
    let font = gpui_kit::Font {
        family: family.to_string().into(),
        ..Default::default()
    };
    texts.font_has_glyph(&font, 'm')
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

/// 读回来的一份字体：能切子集就用子集（带唯一家族名），切不动就交回整份。
///
/// `subset_key == 0` = 这本书没有字符集（老库 / 非 EPUB）或调用方不切；直接整份。
fn prepare_face(
    id: FontId,
    bytes: Vec<u8>,
    subset_key: u64,
    charset: &[u32],
    stats: &mut FontLoadStats,
) -> (AliasKey, Vec<u8>) {
    if subset_key == 0 {
        return (AliasKey::whole(id), bytes);
    }
    let started = super::perf::Timer::start();
    let subset = subset_font_bytes(&bytes, charset);
    stats.subset_ns += started.ns();
    if let Some(subset) = subset {
        let started = super::perf::Timer::start();
        let renamed = rename_subset_family(&subset, subset_family_tag(id, subset_key));
        stats.subset_ns += started.ns();
        if let Some(renamed) = renamed
            && renamed.len() < bytes.len()
        {
            stats.subset_bytes_before += bytes.len();
            stats.subset_bytes_after += renamed.len();
            return (AliasKey::subset(id, subset_key), renamed);
        }
    }
    stats.subset_failures += 1;
    (AliasKey::whole(id), bytes)
}

/// 一份还没注册的字体：缓存键、声明名、字节。
struct PendingFace {
    key: AliasKey,
    declared: String,
    bytes: Vec<u8>,
}

/// 把**整批**未命中的字体一次注册进文本系统，交回每份的
/// `(声明名, 家族名 + 能不能当 primary)`。
///
/// 为什么不是逐份 `add_fonts`：`all_font_names()` 是整张字体表的枚举 + 排序 + 去重，
/// 逐份注册要付 `2 × 份数` 次；这一趟只付两次（`before`/`after`）。名字归属改用
/// `latin_by_family` 已经解析出的 name 表，而不是靠「前后差集」—— 差集在批量里分不清
/// 哪一份新增了哪个名字；name 表是每份字体自己的，天然分得清。
///
/// cmap/name 必须在 `add_fonts` **之前**读：那一步拿走字节的所有权。注册失败
/// （`add_fonts` 的 `Err`）返回 `None`，由调用方把整批记 failed。
fn register_batch(
    texts: &TextSystem,
    pending: Vec<PendingFace>,
) -> Option<Vec<(String, Vec<FaceName>)>> {
    if pending.is_empty() {
        return Some(Vec::new());
    }
    let mut meta: Vec<(AliasKey, String)> = Vec::with_capacity(pending.len());
    let mut coverages: Vec<LatinCoverage> = Vec::with_capacity(pending.len());
    let mut bytes: Vec<Cow<'static, [u8]>> = Vec::with_capacity(pending.len());
    for face in pending {
        meta.push((face.key, face.declared));
        coverages.push(latin_by_family(&face.bytes));
        bytes.push(Cow::Owned(face.bytes));
    }
    // 一次注册整批；wgpu 那侧直接从 fontdb 每条 `load_font_source` 交回的 face 里
    // 读家族名，所以这里拿到的就是「哪份字体有哪些家族」，不需要前后差集，也不需要
    // 拿 ttf-parser 的 name 表去猜 fontdb 报了哪一条。
    let family_sets = texts.add_fonts_with_families(bytes).ok()?;
    // 字体表真的变宽了：让后续建的表都带上新版本。一批只记一次事件。
    face_registered();
    Some(
        meta.into_iter()
            .zip(coverages)
            .zip(family_sets)
            .map(|(((id, declared), coverage), families)| {
                let names: Vec<FaceName> = families
                    .into_iter()
                    .map(|name| FaceName {
                        latin: coverage.covers(&name),
                        name,
                    })
                    .collect();
                store_names(id, &names);
                for face in &names {
                    font_primary::note_face(&face.name, face.latin);
                }
                (declared, names)
            })
            .collect(),
    )
}

/// 子集化用的字符集：书的可见字符 + 可见 ASCII。
///
/// ASCII 必须无条件下进去：gpui 的 primary 过滤要求一张脸带 `m`，而「这本书恰好没有
/// 英文/数字」不代表作者带来的字体要因此失去 primary 资格。
fn charset_with_ascii(charset: &[u32]) -> Vec<u32> {
    let mut out = charset.to_vec();
    out.extend(0x20..=0x7e);
    out.sort_unstable();
    out.dedup();
    out
}

/// 子集身份：字符集（含 ASCII）的进程内 hash。只用来区分「同一个字体文件、不同书」
/// 的子集，不必跨进程稳定。
fn charset_hash(charset: &[u32]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "folio-font-subset-v1".hash(&mut hasher);
    charset.hash(&mut hasher);
    hasher.finish().max(1)
}

/// 子集家族名里的 tag：**字体身份 + 字符集**。两本不同的书用同一个字符集、但字体文件
/// 不同（或多看模板的同一个 `Fonts/kt.ttf` 装着不同字节）时，家族名也必须分开。
fn subset_family_tag(id: FontId, subset_hash: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "folio-font-family-v1".hash(&mut hasher);
    id.hash(&mut hasher);
    subset_hash.hash(&mut hasher);
    hasher.finish().max(1)
}

/// skera 切一份字体到给定字符集。
///
/// `Plan` 用与 CLI 默认一致的选项：保留 GSUB/GPOS（含 `vert`/`vrt2`/`valt` 竖排
/// 特性），保留 name 0..6 与英文记录。字体是 TTC / 解析不了 / skera 报错时回 `None`，
/// 调用方退回整份字体。
///
/// **只切带 `glyf` 的字体**：skera 0.9 没有 CFF charstring 的子集路径，CFF 字体过一遍
/// 会连 `CFF ` 表一起丢干净 —— 《诡秘之主》的 `llf.ttf`（方正飞凡体，CFF 轮廓）实测
/// 子集 1952 字节、cmap 全在而轮廓全空，屏上 h2 整块隐形。没有 `glyf` 就整份放行。
fn subset_font_bytes(bytes: &[u8], charset: &[u32]) -> Option<Vec<u8>> {
    let font = read_fonts::FontRef::new(bytes).ok()?;
    font.glyf().ok()?;
    let mut unicodes = IntSet::<u32>::empty();
    for cp in charset {
        unicodes.insert(*cp);
    }
    let drop_tables: IntSet<Tag> = DEFAULT_DROP_TABLES.iter().copied().collect();
    let layout_features: IntSet<Tag> = DEFAULT_LAYOUT_FEATURES.iter().copied().collect();
    let name_ids: IntSet<NameId> = (0..=6).map(NameId::from).collect();
    let name_languages: IntSet<u16> = [0x0409u16].into_iter().collect();
    let plan = Plan::new(
        &IntSet::empty(),
        &unicodes,
        &font,
        SubsetFlags::default(),
        &drop_tables,
        &IntSet::all(),
        &layout_features,
        &name_ids,
        &name_languages,
    );
    skera::subset_font(&font, &plan).ok()
}

/// 给子集字体里所有家族/全名/PostScript 名加上 `#folio<tag>`，让它在进程字体表里
/// 与别的书的子集、以及整份字体本身分得开。
///
/// 进程字体表是全局的，而 `load_family` 只按家族名匹配；两个不同子集若同名，后注册
/// 的那张脸会和先注册的混在一个家族里，`find_best_match` 按字重/斜体挑一张，缺字的
/// 情况就落到读者眼前。改名是让「这本书的这张子集」有唯一身份的最小代价。
fn rename_subset_family(bytes: &[u8], tag: u64) -> Option<Vec<u8>> {
    let suffix = format!("{}{tag:016x}", font_primary::SUBSET_MARKER);
    let font = read_fonts::FontRef::new(bytes).ok()?;
    let name = font.name().ok()?;
    let data = name.string_data();
    let mut records = Vec::new();
    let mut renamed_any = false;
    for record in name.name_record() {
        // 语言标签记录不搬；带 >0x8000 语言号的记录一起丢，免得写出没有标签表的坏表。
        if record.language_id() >= 0x8000 {
            continue;
        }
        let mut text = record.string(data).ok()?.to_string();
        // 只改家族与全名；PostScript 名（6）有字符集限制，不塞 `#`。
        if matches!(record.name_id().to_u16(), 1 | 4 | 16) {
            text.push_str(&suffix);
            renamed_any = true;
        }
        records.push(write_fonts::tables::name::NameRecord {
            platform_id: record.platform_id(),
            encoding_id: record.encoding_id(),
            language_id: record.language_id(),
            name_id: record.name_id(),
            string: text.into(),
        });
    }
    if !renamed_any {
        return None;
    }
    records.sort_by_key(|r| (r.platform_id, r.encoding_id, r.language_id, r.name_id));
    let name = write_fonts::tables::name::Name::new(records);
    let mut builder = write_fonts::FontBuilder::new();
    builder.add_table(&name).ok()?;
    builder.copy_missing_tables(font);
    Some(builder.build())
}

/// 一份字体字节里每个家族名带不带拉丁 `m`。
///
/// 名字按 name 表 1（family）与 16（typographic family）的记录找，**任何语言**都算 ——
/// fontdb 报回来的可能是本地化那一条。一条都对不上时退回「这份字体里有没有任何一张脸
/// 带 m」：比对失败不该把作者带来的字体整个丢掉，而单脸的字体（真书里的常态）两种问法
/// 是同一个答案。
struct LatinCoverage {
    by_family: HashMap<String, bool>,
    any: bool,
}

impl LatinCoverage {
    fn covers(&self, family: &str) -> bool {
        self.by_family.get(family).copied().unwrap_or(self.any)
    }
}

fn latin_by_family(bytes: &[u8]) -> LatinCoverage {
    let mut coverage = LatinCoverage {
        by_family: HashMap::new(),
        any: false,
    };
    let count = ttf_parser::fonts_in_collection(bytes).unwrap_or(1);
    for index in 0..count {
        let Ok(face) = ttf_parser::Face::parse(bytes, index) else {
            continue;
        };
        let has_m = face.glyph_index('m').is_some();
        coverage.any |= has_m;
        for name in face.names() {
            if !matches!(name.name_id, 1 | 16) {
                continue;
            }
            let Some(text) = name.to_string() else {
                continue;
            };
            coverage
                .by_family
                .entry(text)
                .and_modify(|known| *known |= has_m)
                .or_insert(has_m);
        }
    }
    coverage
}

/// 进程级登记表：字体内容身份 → 注册成功后设备新增的家族名（带「能不能当 primary」）。
///
/// 必需而非优化：`all_font_names()` 去重，同一份字体第二次注册拿不到增量，只靠 diff
/// 会把「这本书带内嵌字体」误判成没有。进程内每份字体只算一次。
///
/// **键必须是内容身份，不能是 `href`**：`href` 只是容器里的路径，两本不同的书可以有
/// 同一条 `OEBPS/Fonts/kt.ttf` 而装着不同字节；拿路径当键会让第二本书跳过注册，并把
/// 第一本的内部家族名登记到第二本的声明名下。身份取中央目录的 `crc32 + raw_len`，
/// 见 [`FontId`]；这是快筛，不是密码学哈希（碰撞概率极低，但不是零）。
fn registered() -> &'static Mutex<HashMap<AliasKey, Vec<FaceName>>> {
    static CACHE: OnceLock<Mutex<HashMap<AliasKey, Vec<FaceName>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_names(key: AliasKey) -> Option<Vec<FaceName>> {
    registered()
        .lock()
        .ok()
        .and_then(|cache| cache.get(&key).cloned())
}

fn store_names(key: AliasKey, names: &[FaceName]) {
    if let Ok(mut cache) = registered().lock() {
        cache.insert(key, names.to_vec());
    }
}

#[cfg(test)]
mod tests {
    //! 别名表本身是纯数据结构，宿主上直接钉。`for_document` 那一条要真容器与真文本系统，
    //! 归到集成那一档，不在这里假装测到。

    use super::{
        AliasKey, BookFonts, FaceName, FontId, FontRequest, cached_names, charset_hash,
        charset_with_ascii, latin_by_family, rename_subset_family, store_names, subset_family_tag,
        subset_font_bytes, system_aliases, system_family,
    };

    #[test]
    #[ignore = "探针：读 /tmp/folio-css/llf.ttf（本机才有的样本），CI 机器上没那个目录"]
    fn scratch_subset_llf_keeps_outlines() {
        use read_fonts::TableProvider;
        let bytes = std::fs::read("/tmp/folio-css/llf.ttf").expect("llf");
        let chars: Vec<u32> = "塔罗会克莱恩·莫雷蒂0TheFool.愚者"
            .chars()
            .map(|c| c as u32)
            .collect();
        let charset = charset_with_ascii(&chars);
        let subset = subset_font_bytes(&bytes, &charset).expect("子集成功");
        let whole_glyf = read_fonts::FontRef::new(&bytes)
            .unwrap()
            .glyf()
            .map(|g| g.offset_data().len())
            .unwrap_or(0);
        let sub_glyf = read_fonts::FontRef::new(&subset)
            .unwrap()
            .glyf()
            .map(|g| g.offset_data().len())
            .unwrap_or(0);
        println!(
            "before {} (glyf {whole_glyf}) → after {} (glyf {sub_glyf})",
            bytes.len(),
            subset.len()
        );
        std::fs::write("/tmp/folio-css/llf_subset.ttf", &subset).unwrap();
        let face = ttf_parser::Face::parse(&subset, 0).expect("可解析");
        for ch in ['塔', '罗', '会', '克', 'A', 'm'] {
            let gid = face.glyph_index(ch);
            let outline = gid.map(|g| {
                let mut n = 0u32;
                face.outline_glyph(g, &mut Counting(&mut n));
                n
            });
            println!("{ch} gid={:?} outline_ops={:?}", gid.map(|g| g.0), outline);
        }
    }

    struct Counting<'a>(&'a mut u32);
    impl ttf_parser::OutlineBuilder for Counting<'_> {
        fn move_to(&mut self, _: f32, _: f32) {
            *self.0 += 1;
        }
        fn line_to(&mut self, _: f32, _: f32) {
            *self.0 += 1;
        }
        fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {
            *self.0 += 1;
        }
        fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {
            *self.0 += 1;
        }
        fn close(&mut self) {
            *self.0 += 1;
        }
    }
    use crate::ui::foundation::font_primary::SUBSET_MARKER;
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

        fonts.insert("Book Body".into(), vec![named("Noto Serif", true)]);
        assert_eq!(
            fonts.families_for("Book Body").unwrap(),
            &[named("Noto Serif", true)]
        );
        assert!(fonts.families_for("serif").is_none(), "泛族名不该凭空有链");
    }

    /// 同名重复登记（`@font-face` 的多条 `src`）不产生重复条目；
    /// 同一个名字一次带 `m`、一次不带时算带 —— 只要有一张脸能用，这个家族就能用。
    #[test]
    fn repeated_inserts_do_not_duplicate_families() {
        let mut fonts = BookFonts::empty();
        fonts.insert("Book Body".into(), vec![named("Noto Serif", false)]);
        fonts.insert(
            "Book Body".into(),
            vec![named("Noto Serif CJK", true), named("Noto Serif", true)],
        );
        assert_eq!(
            fonts.families_for("Book Body").unwrap(),
            &[named("Noto Serif", true), named("Noto Serif CJK", true)]
        );
    }

    fn named(name: &str, latin: bool) -> FaceName {
        FaceName {
            name: name.to_string(),
            latin,
        }
    }

    /// 判据必须与 gpui 的 `charmap.map('m') == 0` 同源：随包的 emoji 字体没有 `m`
    /// （有数字与 #、*，没有拉丁字母），所以它当不了 primary；名字对不上时按整份字体
    /// 「有没有任何一张脸带 m」回答，不因为比对失败就把字体丢掉。
    #[test]
    fn a_font_without_latin_m_is_not_primary_material() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("android/app/src/main/assets/fonts/NotoColorEmoji.ttf");
        let bytes = std::fs::read(&path).expect("随包的 emoji 字体在仓库里");
        assert!(!latin_by_family(&bytes).covers("Noto Color Emoji"));
        assert!(
            !latin_by_family(&bytes).covers("A Family That Is Not In This Font"),
            "名字对不上时退回整份字体：这份里一张带 m 的脸都没有"
        );
        assert!(!latin_by_family(b"not a font").any, "认不出的字节不 panic");
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
        let a_key = AliasKey::whole(FontId::of(&a));
        store_names(a_key, &[named("Family A", true)]);
        assert_eq!(cached_names(a_key), Some(vec![named("Family A", true)]));
        assert_eq!(
            cached_names(AliasKey::whole(FontId::of(&b))),
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

    /// worker 先问 `font_refs_in`（共享容器），只有它给不出时才退回自己开容器的
    /// `font_refs`。这条门用一个「`font_refs_in` 有值、`font_refs` 一调就 panic」的
    /// 文档钉住顺序 —— 反了就会在真实的 EPUB 上重开一次 ZIP、再解析一遍 OPF。
    #[gpui_kit::test]
    fn the_worker_prefers_the_shared_container_over_opening_its_own(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use folio_core::content::document::{ContentRef, Document as OpenedDocument};
        use folio_core::content::format::{
            Content, ContentKind, DocMeta, FlowDoc, FontRef, Format,
        };

        struct SharedOnly(Box<dyn OpenedDocument>);
        impl OpenedDocument for SharedOnly {
            fn meta(&self) -> &DocMeta {
                self.0.meta()
            }
            fn kind(&self) -> ContentKind {
                self.0.kind()
            }
            fn content(&self) -> ContentRef<'_> {
                self.0.content()
            }
            fn image_bytes(&self, href: &str) -> Option<Vec<u8>> {
                self.0.image_bytes(href)
            }
            fn stylesheet(&self) -> String {
                self.0.stylesheet()
            }
            fn chapter_source(&self, index: usize) -> folio_core::Result<String> {
                self.0.chapter_source(index)
            }
            fn font_refs_in(&self, _wanted: &[String]) -> Option<Vec<FontRef>> {
                Some(vec![FontRef {
                    href: "x".to_string(),
                    media_type: "font/ttf".to_string(),
                    raw_len: 1,
                    crc32: 1,
                }])
            }
            fn font_refs(&self, _wanted: &[String]) -> Option<Vec<FontRef>> {
                panic!("共享容器已经给出身份，不该再开第二个 ZIP")
            }
        }

        cx.update(gpui_kit::init);
        let inner = folio_core::content::document::from_content(
            DocMeta {
                title: "共享容器".into(),
                format: Format::Epub,
                ..DocMeta::default()
            },
            Content::Reflow(FlowDoc::default()),
        );
        let doc = SharedOnly(inner);
        let request = FontRequest {
            wanted: vec!["x".to_string()],
            declared_of: [("x".to_string(), "Fam".to_string())].into_iter().collect(),
            faces: Vec::new(),
        };
        let (_, stats) = cx.update(|cx| BookFonts::for_request(cx.text_system(), &doc, &request));
        assert_eq!(stats.faces, 1, "用的就是共享容器给的那一份");
        assert_eq!(
            stats.failed, 1,
            "没有容器的测试文档读不到字节，只记一次失败"
        );
    }

    /// 子集字符集永远带 ASCII（primary 要 `m`），hash 对同一份集合稳定。
    #[test]
    fn subset_charset_keeps_ascii_and_hashes_stably() {
        let charset = charset_with_ascii(&[0x6c49]);
        assert!(charset.contains(&('m' as u32)), "ASCII 必须无条件补进来");
        assert!(charset.contains(&0x6c49));
        assert_eq!(charset_hash(&charset), charset_hash(&charset.clone()));
        assert_ne!(charset_hash(&charset), 0, "0 留给整份字体");
    }

    /// 真书 HP：导入收的整本字符集 → skera 子集 → 家族名加唯一后缀。
    ///
    /// 忽略：真书只在 `tests/fixtures/local/`，不入库。手动跑：
    /// `cargo test -p folio-app --lib -- --ignored --nocapture hp_font_subset_is_renamed_and_smaller`
    #[test]
    #[ignore = "真书 HP 不在仓库；手动跑"]
    fn hp_font_subset_is_renamed_and_smaller() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/local/hali-bote-quanji.epub");
        if !path.is_file() {
            eprintln!("{} 不在，跳过", path.display());
            return;
        }
        let doc = folio_core::content::document::open(&path).expect("打开 HP");
        let charset = charset_with_ascii(doc.font_charset());
        assert!(
            charset.len() > 1000,
            "导入该收到整本字符集：{}",
            charset.len()
        );
        let hash = charset_hash(&charset);
        let css = doc.stylesheet();
        let rules = RuleSet::parse(&css, folio_core::content::format::css::Origin::Author);
        let request = FontRequest::from_rules(&rules);
        let refs = doc.font_refs(&request.wanted).expect("EPUB 有身份");
        assert_eq!(refs.len(), 2, "HP 有两份内嵌字体");

        for reference in refs {
            let id = FontId::of_ref(&reference);
            let bytes = doc.read_font(&reference).expect("读一份");
            let subset = subset_font_bytes(&bytes, &charset).expect("skera 该切得动");
            assert!(
                subset.len() < bytes.len(),
                "{}: {} B 的子集不该比原字体大",
                reference.href,
                subset.len()
            );
            let renamed = rename_subset_family(&subset, subset_family_tag(id, hash)).expect("改名");
            let coverage = latin_by_family(&renamed);
            let names: Vec<String> = coverage.by_family.keys().cloned().collect();
            assert!(
                names.iter().all(|name| name.contains(SUBSET_MARKER)),
                "子集家族名必须带唯一后缀：{names:?}"
            );
            assert!(
                names.iter().any(|name| coverage.covers(name)),
                "子集必须还带拉丁 m：{names:?}"
            );
            println!(
                "{}: {} B -> {} B ({:.1}%), families={names:?}",
                reference.href,
                bytes.len(),
                renamed.len(),
                100.0 * renamed.len() as f64 / bytes.len() as f64,
            );
        }
    }

    /// 这条探针的输出里附一句「CSS 里是谁在点这份字体」——只用 `declared_of` 那一份映射。
    fn declared_of_note(href: &str) -> String {
        let _ = href;
        String::new()
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
        // 默认 HP；`FOLIO_FONT_BENCH=<路径>` 指任何一本在盘上的书（换大本读数字用）。
        let path = std::env::var("FOLIO_FONT_BENCH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/local/hali-bote-quanji.epub")
            });
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
        println!("wanted 条数={}", wanted.len());
        for face in &faces {
            // 每份字体：大小、CSS 里点它的声明名、它每一张脸带不带拉丁 `m`。
            // 不带 `m` 的脸 gpui 会整张从 primary 里剔掉（判据见 [`FaceName`]），
            // 所以这一列直接说明「作者带来的这份字体用得上用不上」。
            let coverage = latin_by_family(&face.bytes);
            let mut names: Vec<String> = coverage.by_family.keys().cloned().collect();
            names.sort();
            println!(
                "  {:<28} {:>9} B  any_m={}  {}{}",
                face.href,
                face.bytes.len(),
                coverage.any,
                names
                    .iter()
                    .map(|name| format!("{name}={}", coverage.covers(name)))
                    .collect::<Vec<_>>()
                    .join(" "),
                declared_of_note(&face.href),
            );
        }
        let missing: Vec<&str> = wanted
            .iter()
            .map(String::as_str)
            .filter(|href| !faces.iter().any(|face| face.href == *href))
            .collect();
        println!(
            "CSS 点了 {} 条，容器里真有 {} 份，**对不到文件的 {} 条**：{missing:?}",
            wanted.len(),
            faces.len(),
            missing.len()
        );
        for round in 1..=3 {
            let started = Instant::now();
            let (fonts, stats) = cx.update(|cx| BookFonts::for_document(doc.as_ref(), &css, cx));
            let total_us = started.elapsed().as_micros();
            println!(
                "round {round}: total_us={total_us} parse_us={} refs_us={} subset_us={} bytes_us={} register_us={} faces={} failed={} empty_diff={} subset={}->{} subset_fail={} families={}",
                stats.parse_ns / 1000,
                stats.refs_ns / 1000,
                stats.subset_ns / 1000,
                stats.bytes_ns / 1000,
                stats.register_ns / 1000,
                stats.faces,
                stats.failed,
                stats.empty_diff,
                stats.subset_bytes_before,
                stats.subset_bytes_after,
                stats.subset_failures,
                fonts.families.len(),
            );
            // `TestAppContext` 的文本系统可能整类忽略 `add_fonts`（`families=0`、
            // `empty_diff=faces`）；这条 ignored 探针只负责把数打出来，真机注册耗时要
            // 在设备上量，不在宿主测试里假装测到。
        }
    }
}
