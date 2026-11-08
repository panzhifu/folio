//! 随书字体的**加载管线**：`@font-face` → 身份去重 → 读字节 → 子集化 → 改名 → 注册 →
//! 别名表。2026-11-09 从 `folio_app::ui::features::reader::model::book_fonts` 整体搬来，
//! gpui 的接触点收进 [`FontSink`] 一个 trait（app 用 `TextSystemSink` 实现）——
//! 与 [`crate::layout::Measure`] 同一条边界画法：接口在 core，实现在 app。
//!
//! 上一版的完整设计文档见 `doc/字体加载优化.md`；行内注释保留搬迁时的口径。

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use read_fonts::TableProvider;
use read_fonts::collections::IntSet;
use read_fonts::types::{NameId, Tag};
use skera::{DEFAULT_DROP_TABLES, DEFAULT_LAYOUT_FEATURES, Plan, SubsetFlags};

use crate::content::document::Document;
use crate::content::format::css::{FontFaceRule, FontSrc, RuleSet};
use crate::content::format::{FontBytes, FontRef};
use crate::text::families as font_primary;
use crate::text::names::{LatinCoverage, latin_by_family};

/// 与 app 侧 `perf::Timer` 同形的计时段：`start` 起表，`ns` 报已过去的纳秒。
/// 搬进 core 后不再借 app 的那一份 —— 口径一致（`Instant` 差的纳秒），不需要同步。
struct Timer(Instant);

impl Timer {
    fn start() -> Self {
        Self(Instant::now())
    }

    fn ns(&self) -> u64 {
        self.0.elapsed().as_nanos() as u64
    }
}

/// 泛族名：`font-family` 里点到它们时按名字判他要的是哪一类，不按这份字节注册。
/// 搬自 `folio_app::…::measure`（那里的度量还按它分回退链）—— 两处必须同一份名单。
const GENERIC_FAMILIES: &[&str] = &[
    "serif",
    "sans-serif",
    "monospace",
    "cursive",
    "fantasy",
    "system-ui",
    "ui-serif",
    "ui-sans-serif",
    "ui-monospace",
    "initial",
    "default",
];

/// 这个家族名是不是泛族名（`serif` / `monospace` 这一族）。
pub fn is_generic(family: &str) -> bool {
    GENERIC_FAMILIES
        .iter()
        .any(|g| family.eq_ignore_ascii_case(g))
}

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

/// 记一次「字体表因为**书外**的原因变宽了」：读者自己下载或导入了一份字体
/// （见 `platform::user_fonts`）。与 [`face_registered`] 记的是同一个事件，只是入口
/// 在本模块之外 —— 开个口子而不是把 [`FACE_EVENTS`] 摆出去，数数的那杆秤留在
/// 这一个文件里。
pub fn note_external_registration() {
    face_registered();
}
/// 声明名 → 设备家族名。`Send + Sync`：会在后台线程的度量里被拿着。
pub struct BookFonts {
    /// 一个声明名可能对应多份内部家族（一份字体文件带好几个名字），
    /// 第一个能当 primary 的当 primary，其余留在补缺链上。
    families: HashMap<String, Vec<FaceName>>,
    /// 建这张表时的字体表版本，见 [`FACE_EVENTS`]。
    revision: u32,
}

/// 注册成功后设备报回来的一个内部家族名，以及它有没有拉丁 `m` 的字形。
///
/// `latin` 从前决定「能不能当 gpui 的 primary」：gpui 的 `CosmicTextSystem::load_family`
/// 会把没有 `m` 的脸从 primary 候选里剔掉，剔空后 `resolve_font` 直接 panic——桌面被
/// fontconfig 的回退表接住，Android 上 gpui 的全局回退表一个都不存在，整台进程
/// SIGABRT（2026-10-30《诡秘之主》的 `sy.ttf`：只有 18 个字形、没有拉丁 `m`，在
/// Meizu 20 上就是这么闪退的）。那一格随 gpui-folio be92574 删除：过滤器没了，
/// primary 未覆盖的字符也改走补缺链。
///
/// `latin` 现在只剩一个消费者：[`font_primary`](folio_core::text)
/// 登记表，供**字体选择器**把 emoji 与符号字体挡在读者可选列表之外（无拉丁的脸不该
/// 当正文字体的候选）；随书字体当 primary 不再受它管。
///
/// 判据仍与 gpui 上游那句同源（`charmap.map('m') == 0`）：登记表记的是同一件事，
/// 选择器的口径才有据可依。
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
    /// 这一趟是不是直接命中了 [`FontLoadJob`] 的结果缓存（没有真跑）。app 侧的
    /// `perf::log_font_load` 只在**真跑**的那一趟记账 —— 从前判定藏在 `run` 的
    /// 早退路径里，下沉后由调用方按这一格决定记不记。
    pub cached_run: bool,
}
/// 字体身份：容器里那条资源的内容身份，不是 `href`。
///
/// `href` 在两本不同的书里可以指向不同的字节（多看模板的 `Fonts/kt.ttf` 就是例子）；
/// 进程级别名缓存拿它当键会让第二本书认到第一本的内部家族名。EPUB 从中央目录的
/// `crc32 + raw_len` 取，MOBI 对原始 FONT 记录现算（不解压，见 `mobi::font_refs`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontId {
    crc32: u32,
    raw_len: u64,
}

impl FontId {
    pub fn of(face: &FontBytes) -> Self {
        Self {
            crc32: face.crc32,
            raw_len: face.raw_len,
        }
    }

    pub fn of_ref(font: &FontRef) -> Self {
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
    /// 跑一次（或取第一次的结果）。`sink` 是排版作业手里那份文本系统的桥
    /// （app 侧 `TextSystemSink`）。
    pub fn run(&self, sink: &dyn FontSink) -> (Arc<BookFonts>, FontLoadStats) {
        let mut result = self
            .result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((fonts, stats)) = result.as_ref() {
            let mut stats = *stats;
            stats.cached_run = true;
            return (fonts.clone(), stats);
        }
        let (fonts, stats) = BookFonts::for_request(sink, self.doc.as_ref(), &self.request);
        *result = Some((fonts.clone(), stats));
        (fonts, stats)
    }
}

/// app 与 core 之间的**文本系统桥**：注册交字节、问设备上已有哪些家族、问某个家族
/// 有没有某个字形。实现在 app（`TextSystemSink` 包着 `Arc<TextSystem>`）；测试给假的。
pub trait FontSink: Send + Sync {
    /// 交一批 sfnt 字节注册进文本系统，回每份报出来的家族名（顺序与入参对齐）。
    /// 整批失败回 `None` —— 调用方把整批记 failed。
    fn add_faces(&self, faces: Vec<Vec<u8>>) -> Option<Vec<Vec<String>>>;

    /// 设备上已注册的家族名（`all_font_names()` 的口径：去重）。
    fn known_families(&self) -> Vec<String>;

    /// 某个家族的某张脸有没有这个字形（现在只问 `'m'`）。
    fn family_has_glyph(&self, family: &str, ch: char) -> bool;
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
    /// **这本书自己的表**里查得到的名字（容器字体与作者点名的设备别名）一律可用：
    /// gpui 侧的「无拉丁 `m` 不当 primary」过滤器已随 gpui-folio be92574 删除，
    /// primary 缺的字符（ASCII 与生僻字都在内）由补缺链兜住。名字不在这本书的表里
    /// （读者偏好、设备随机名）才问进程登记表 —— 那一条现在服务的是字体选择器，
    /// emoji 与符号字体不该出现在可选列表里。
    pub fn primary_usable(&self, name: &str) -> bool {
        let shipped = self
            .families
            .values()
            .flatten()
            .any(|face| face.name.eq_ignore_ascii_case(name));
        // 不在表里才走登记表 —— 那条路要拿锁，别让每次排版都走它。
        shipped || font_primary::usable(name)
    }

    /// 只有测试在调（钉「空表读不出别名」那一格），生产这一侧没人问。
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.families.is_empty()
    }

    /// 登记过的声明名数。探针（`font_load_bench`）打它对账 —— 字段私有，给个只读口。
    pub fn families_len(&self) -> usize {
        self.families.len()
    }
    /// 用 UI 已经折好的 [`FontRequest`] 建别名表。worker 只做三件事：
    /// 按身份查别名缓存 → 未命中才读这一份字节 → 注册。
    ///
    /// 身份先走 `doc.font_refs_in`：开书读 CSS 那一趟已经把共享容器打开，这一问
    /// 复用同一个 ZIP、只补一遍 OPF；provider 没有共享容器这条路时退回
    /// `doc.font_refs`（自己开容器），再没有元数据入口才退回 `doc.fonts` 一次读全
    /// （仍按身份缓存）。
    pub fn for_request(
        sink: &dyn FontSink,
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
            let refs_timer = Timer::start();
            let refs = doc
                .font_refs_in(&request.wanted)
                .or_else(|| doc.font_refs(&request.wanted));
            stats.refs_ns = refs_timer.ns();
            if let Some(refs) = refs {
                stats.faces = refs.len();
                let timer = Timer::start();
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
                    let read = Timer::start();
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
                match register_batch(sink, pending) {
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
            } else {
                // 没有元数据入口的 provider（CBZ / PDF 等）：退回一次读全，仍按身份缓存注册结果。
                let bytes = Timer::start();
                let faces = doc.fonts(&request.wanted);
                stats.bytes_ns = bytes.ns();
                stats.faces = faces.len();
                stats.bytes_read = faces.len();
                let timer = Timer::start();
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
                match register_batch(sink, pending) {
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
        let register = register.unwrap_or_else(Timer::start);

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
            let known = sink.known_families();
            for (declared, family) in system_aliases(&request.faces, &known) {
                if fonts.families_for(&declared).is_none() {
                    // 设备字体手上没有字节，但字体系统有：真解析一遍有没有 `m`，
                    // 别再按名字假设它一定能当 primary。
                    let latin = sink.family_has_glyph(&family, 'm');
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
    let started = Timer::start();
    let subset = subset_font_bytes(&bytes, charset);
    stats.subset_ns += started.ns();
    if let Some(subset) = subset {
        let started = Timer::start();
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
/// cmap/name 必须在 `add_faces` **之前**读：那一步拿走字节的所有权。注册失败
/// （sink 回 `None`）返回 `None`，由调用方把整批记 failed。
fn register_batch(
    sink: &dyn FontSink,
    pending: Vec<PendingFace>,
) -> Option<Vec<(String, Vec<FaceName>)>> {
    if pending.is_empty() {
        return Some(Vec::new());
    }
    let mut meta: Vec<(AliasKey, String)> = Vec::with_capacity(pending.len());
    let mut coverages: Vec<LatinCoverage> = Vec::with_capacity(pending.len());
    let mut bytes: Vec<Vec<u8>> = Vec::with_capacity(pending.len());
    for face in pending {
        meta.push((face.key, face.declared));
        coverages.push(latin_by_family(&face.bytes));
        bytes.push(face.bytes);
    }
    // 一次注册整批；wgpu 那侧直接从 fontdb 每条 `load_font_source` 交回的 face 里
    // 读家族名，所以这里拿到的就是「哪份字体有哪些家族」，不需要前后差集，也不需要
    // 拿 ttf-parser 的 name 表去猜 fontdb 报了哪一条。
    let family_sets = sink.add_faces(bytes)?;
    // 字体表真的变宽了：让后续建的表都带上新版本。一批只记一次事件。
    face_registered();
    // 选择器那份可选名单也变了（书籍字体分组、列表刷新都以它对账）。
    font_primary::bump_families_revision();
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
                    // 非子集的名字进「书籍字体」登记表（子集名带标记，函数里自己会拒）。
                    font_primary::note_book_family(&face.name);
                }
                (declared, names)
            })
            .collect(),
    )
}
/// 而它们恰恰是书里英文与数字该用的那份字形。
pub fn charset_with_ascii(charset: &[u32]) -> Vec<u32> {
    let mut out = charset.to_vec();
    out.extend(0x20..=0x7e);
    out.sort_unstable();
    out.dedup();
    out
}

/// 子集身份：字符集（含 ASCII）的进程内 hash。只用来区分「同一个字体文件、不同书」
/// 的子集，不必跨进程稳定。
pub fn charset_hash(charset: &[u32]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "folio-font-subset-v1".hash(&mut hasher);
    charset.hash(&mut hasher);
    hasher.finish().max(1)
}

/// 子集家族名里的 tag：**字体身份 + 字符集**。两本不同的书用同一个字符集、但字体文件
/// 不同（或多看模板的同一个 `Fonts/kt.ttf` 装着不同字节）时，家族名也必须分开。
pub fn subset_family_tag(id: FontId, subset_hash: u64) -> u64 {
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
pub fn subset_font_bytes(bytes: &[u8], charset: &[u32]) -> Option<Vec<u8>> {
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
pub fn rename_subset_family(bytes: &[u8], tag: u64) -> Option<Vec<u8>> {
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
    //! 归到 app 侧（`folio_app::…::book_fonts` 的测试），不在这里假装测到。
    //!
    //! `scratch_subset_llf_keeps_outlines` 读的是本机才有的样本（`/tmp/folio-css/`），
    //! 默认 ignored。

    use crate::content::format::FontBytes;
    use crate::content::format::css::{Origin, RuleSet};

    use super::{
        AliasKey, BookFonts, FaceName, FontId, FontRequest, cached_names, charset_hash,
        charset_with_ascii, store_names, subset_font_bytes, system_aliases, system_family,
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

    /// 子集字符集永远带 ASCII（primary 要 `m`），hash 对同一份集合稳定。
    #[test]
    fn subset_charset_keeps_ascii_and_hashes_stably() {
        let charset = charset_with_ascii(&[0x6c49]);
        assert!(charset.contains(&('m' as u32)), "ASCII 必须无条件补进来");
        assert!(charset.contains(&0x6c49));
        assert_eq!(charset_hash(&charset), charset_hash(&charset.clone()));
        assert_ne!(charset_hash(&charset), 0, "0 留给整份字体");
    }
}
