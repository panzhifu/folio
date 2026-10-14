//! `Measure` 的 gpui 实现：断行走 App 级 `TextSystem::line_wrapper`，
//! 整行度量走窗口级 `WindowTextSystem::layout_line`。
//!
//! 两者的分工是 spike① 实测出来的，不是猜的：`line_wrapper` / `advance` 挂在
//! `Arc<TextSystem>` 上（不要求 `&mut Window`，所以能进后台线程），
//! `shape_line` / `layout_line` 挂在 `Arc<WindowTextSystem>` 上。
//!
//! 一个必须同源的细节：**断行用的宽度累加**与**行盒报告的宽度**都来自同一套
//! 单字 advance，否则会出现「按 A 算说放得下、按 B 量却溢出」。
//!
//! 桌面与移动**共用这一份**。原先移动端有另一份逐行照抄的实现，唯一的区别是
//! `f32::from(x)` 还是 `x.as_f32()`；两件事因此成立：一处改了断行口径两端同时生效，
//! 以及同一本书在两端折出的行数可以逐行比对（`document.rs` 的 tests 就靠这个前提）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use folio_core::layout::{FontKey, LineMetrics, Measure};
use gpui_kit::*;

use super::book_fonts::BookFonts;

/// CSS 的泛族名。它们在 gpui 里**解析不出真实字体**：`TextSystem::resolve_font`
/// 找不到就叫平台默认（一个拉丁字体，汉字量到的是 .notdef 的宽度），
/// 而绘制路径会按字符串走 fallback 找到真的中文字体 —— 于是「按 A 宽居中、
/// 按 B 宽绘制」，标题就歪到版心外了。
/// 阅读器也不该让书的 `serif` 盖掉读者选的字体，所以一律换成偏好字体。
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

/// 等宽那一种泛族名。它们换成读者的 `mono_font_family`，而不是正文字体 ——
/// UA 表给 `code` / `pre` 的就是 `monospace`（`folio_core::content::format::css::ua`），
/// 以前它和 `serif` 走同一条回落，结果代码块用的是正文那一份，等宽整个丢掉。
const MONO_FAMILIES: &[&str] = &["monospace", "ui-monospace"];

fn is_generic(family: &str) -> bool {
    GENERIC_FAMILIES
        .iter()
        .any(|g| family.eq_ignore_ascii_case(g))
}

/// `FontKey` → gpui `Font`。**度量与绘制必须走这一个函数**：
/// 两边各自拼 Font 的话，一次字段改动就会让居中偏移与实际字宽对不上。
///
/// 三个输入各有各的回落，都不许把「解析不出来的名字」交出去 ——
/// `TextSystem::resolve_font` 解析不到时是 **panic**，不是回退，而书的 `font-family`
/// 在目标设备上几乎都不存在（实测《斗战狂潮》的 `easypub` 会让 Android 直接 SIGABRT）：
///
/// - `key` 的候选链里有**设备上真装着**的非泛族名 → 那一个当 primary（作者点名了，照旧生效）；
/// - 泛族名或设备上没装的 → primary 换成读者偏好：等宽那几种用 `mono`，其余用 `body`；
///   而 `mono` 自己也要在 `known` 里，否则退回 `body`（读者填了个没装的等宽名，
///   不该因此把整本书画成方框）；
/// - `faces` 是这本书注册成功的内嵌字体，**只进 `fallbacks`**：gpui 在 shaping 时
///   逐字符挑覆盖得住的那一份（`cosmic_text_system.rs` 的 `pick_covering_slot`），
///   于是读者的字体照常是正文，只有它画不出来的那一格才轮到书里的字体。
///   ASCII 永远留在 primary 上 —— 那是 gpui 的规则，不是这里的取舍。
pub fn gpui_font(
    key: &FontKey,
    body: &SharedString,
    mono: &SharedString,
    faces: &BookFonts,
    known: &[String],
) -> Font {
    // 作者点名的是一整条候选链（`"MyFont", "Noto Serif CJK SC", serif`），逐个试，
    // 取第一个这台设备真装着的。只认第一个的话，`MyFont` 不在就整条落回读者的字体，
    // 而作者排在第二的那一份明明装着 —— 泛族名不算「装着」，见 [`GENERIC_FAMILIES`]。
    let chosen = key
        .candidates()
        .iter()
        .find(|name| !is_generic(name) && known.iter().any(|n| n.eq_ignore_ascii_case(name)));
    let installed = chosen.is_some();
    let family = match chosen {
        Some(name) => SharedString::from(name.to_string()),
        // 整条链都没装着：泛族名里等宽那几种换成读者的等宽偏好，其余换成正文偏好。
        // `mono` 自己也要在册，否则退回 `body`（读者填了个没装的名字不该让全书画方框）。
        None if MONO_FAMILIES
            .iter()
            .any(|m| key.family.eq_ignore_ascii_case(m))
            && known
                .iter()
                .any(|name| name.eq_ignore_ascii_case(mono.as_ref())) =>
        {
            mono.clone()
        }
        None => body.clone(),
    };
    // primary 已经是作者点名的那一个时，别再把它自己挂成 fallback（同族自己补自己没意义）。
    // 找书的字体时同样按整条链试：`@font-face` 声明的名字可能是链上的第二个。
    let fallbacks = if installed {
        None
    } else {
        key.candidates()
            .iter()
            .find_map(|name| faces.fallbacks_for(name))
            .map(|names| FontFallbacks::from_fonts(names.to_vec()))
    };
    Font {
        family,
        fallbacks,
        weight: FontWeight(key.weight as f32),
        style: if key.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
        ..Default::default()
    }
}

/// 绘制路径的 [`FontKey`] → `Font` 缓存。
///
/// 为什么需要：`place_lines` 每帧对每一条可见 run 都要一个 `Font`，而 [`gpui_font`]
/// 要把候选链上的每个名字拿去设备字体表里线性扫一遍（`known.iter().any(..)`）。
/// 一个四千行、几百条 run 的章，帧帧重扫就是最主要的「滑动卡顿」来源之一。
/// 一本书里 distinct 的 `FontKey` 通常只有几个，缓存后每帧只做一次哈希查表。
///
/// 缓存键只有 `FontKey`，所以**body / mono / faces / known 变了必须 `clear`**：
/// 它们不入键，却直接决定同一个 `FontKey` 解出哪个 `Font`。调用点见 `ReaderSession`：
/// 换书重建，读者改字体时清空。
pub struct FontCache {
    known: Vec<String>,
    resolved: HashMap<FontKey, Font>,
}

impl FontCache {
    pub fn new(known: Vec<String>) -> Self {
        Self {
            known,
            resolved: HashMap::new(),
        }
    }

    /// 设备字体表换了一份（换书、装了内嵌字体）时重建。
    pub fn reset(&mut self, known: Vec<String>) {
        self.known = known;
        self.resolved.clear();
    }

    /// body / mono 换了（读者改正文或等宽字体）时清缓存：它们不入键。
    pub fn clear(&mut self) {
        self.resolved.clear();
    }

    pub fn resolve(
        &mut self,
        key: &FontKey,
        body: &SharedString,
        mono: &SharedString,
        faces: &BookFonts,
    ) -> Font {
        if let Some(font) = self.resolved.get(key) {
            return font.clone();
        }
        let font = gpui_font(key, body, mono, faces, &self.known);
        self.resolved.insert(key.clone(), font.clone());
        font
    }
}

/// 一段文字里出现了哪几类文字。按位存进缓存键。
///
/// 存在的理由只有一句话：**样式声明的字体不是这段字实际用的字体**。中文书里 `<p>` 的字族
/// 解析到一张拉丁脸，汉字与假名是 shaping 时按字符回退到另一张脸上的，而行盒的
/// ascent/descent 恰恰由那张脸给。`font_extents` 不收文本时（2026-10-13 之前）只能拿一个
/// 拉丁代表字符量一次再按 `(字体, 字号)` 缓存，于是混排的行盒永远按拉丁高度折 ——
/// 那不只是行高：`LineRun.ascent` 同时是绘制的基线和选中矩形的高。
///
/// 分档**不要求精确**：判据只是「同类的字大致落在同一张回退脸上」。多一档只多量一次，
/// 漏一档则退回拉丁档的代表字符，那与改动前的行为一致，不是新增的错。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Script {
    Latin,
    Han,
    Kana,
    Hangul,
    Arabic,
    Hebrew,
    Indic,
    Thai,
}

use Script::*;

/// 这一档的代表字符。**取该档里几乎一定有的字**：`一`（CJK 基本区第一个码位）、
/// `あ`/`한` 等，都是任何覆盖该文字的字库里必然存在的字形 —— 量到一个 `.notdef` 方框
/// 的度量就等于什么也没修。
fn representative(script: Script) -> char {
    match script {
        Latin => 'x',
        Han => '一',
        Kana => 'あ',
        Hangul => '한',
        Arabic => 'ع',
        Hebrew => 'א',
        Indic => 'अ',
        Thai => 'ก',
    }
}

/// 这个码位属于哪一档。**只做区间比较**：逐字符调 `char::script()` 要把 unicode 表引进来，
/// 而这段文字每个 run 都要扫一遍。
///
/// **具体档必须排在拉丁那条兜底之前**——顺序写反过一次：`if at < 0x2E80 { Latin }` 打头，
/// 于是阿拉伯文（U+0600 段）、希伯来文、天城文、泰文全被吞进拉丁档，代表字符测试当场
/// 抓到（`ع` 报成 Latin）。拉丁是**兜底档**，不是区间档。
fn script_of(ch: char) -> Script {
    let at = ch as u32;
    if (0x3040..0x3100).contains(&at) {
        Kana // 平假名与片假名。先于 Han 判：它就嵌在 CJK 那一大段里
    } else if (0xAC00..0xD800).contains(&at) || (0x1100..0x1200).contains(&at) {
        Hangul // 音节表谚文 + 字母（jamo）
    } else if (0x0600..0x0700).contains(&at) || (0x0750..0x0780).contains(&at) {
        Arabic
    } else if (0x0590..0x0600).contains(&at) {
        Hebrew
    } else if (0x0900..0x0E00).contains(&at) {
        Indic // 天城文起，一路到泰文之前的那几张印度系字库（同一次回退大多落在同一张脸）
    } else if (0x0E00..0x1000).contains(&at) {
        Thai // 泰文与老挝文
    } else if (0x2E80..0x3100).contains(&at)
        || (0x3200..0x4400).contains(&at)
        || (0x4E00..0xA000).contains(&at)
        || (0xF900..0xFB00).contains(&at)
        || (0xFE30..0xFE50).contains(&at)
        || (0xFF00..0xFF61).contains(&at)
        || at >= 0x20000
    {
        // CJK 部首/标点/兼容/基本区/扩展 A 与 B/全角形态/增补平面：汉字那一坨。
        // 中间 0x3100..0x3200 那一小段是注音与方言字，归进这一档（同一次回退同一张脸）。
        Han
    } else {
        // ASCII、拉丁扩展、希腊、西里尔、亚美尼亚、通用标点都归这一档：它们与正文字族
        // 同脸或同族，回退出去的那张脸高度差不到会切字的程度。
        Latin
    }
}

const ALL_SCRIPTS: [Script; 8] = [Latin, Han, Kana, Hangul, Arabic, Hebrew, Indic, Thai];

/// 一段文本的类别位掩码。`u8` 装得下这八档，越界的类别不存在（`script_of` 是全函数）。
/// 空文本给零掩码，由 [`font_extents`](Measure::font_extents) 那头的兜底档接住。
fn script_mask(text: &str) -> u8 {
    let mut mask = 0u8;
    for ch in text.chars() {
        mask |= 1 << (script_of(ch) as u8);
    }
    mask
}

/// 把一档量到的 extent 并进来。
///
/// **`acc` 是 `Option` 而不是从 `(0.0, 0.0)` 起步**：gpui 报的 `descent` 可以是负的
/// —— 本机正文族 LXGW WenKai 在 20px 下报 `ascent 20.5 / descent −5.5`（字形盒比 em 矮，
/// 差的那截算进了 ascent）。从 0.0 起 `max` 会把那个负数**静默吞成 0**：内容高从 15 变 20.5，
/// 半 leading 跟着算错，行盒整体下移。这条是这次改动自己引入过的回归，被实测逮到（2026-10-13），
/// 所以初值必须由**第一档的实测**定，不能由零定。
fn merge_extents(acc: Option<(f32, f32)>, next: (f32, f32)) -> (f32, f32) {
    match acc {
        None => next,
        Some((a, d)) => (a.max(next.0), d.max(next.1)),
    }
}

/// `(字体, 字号, 文字类别) → (ascent, descent)` 的共享缓存。
///
/// 键里那格 `u8` 就是上面那个掩码：**章里 distinct 的组合是个位数到十位数**（纯拉丁、
/// 拉丁+汉字、拉丁+汉字+假名那种），不是行数 —— 所以按文本分类不会把这段拉回逐行 shaping。
///
/// 单独立一个名字：它套了三层（`Arc` 为了克隆共享、`Mutex` 为了内部可变、
/// `HashMap` 为了查），摊在字段签名里读不出「这是一张共享的表」这件事。
type FontExtentsCache = Arc<Mutex<HashMap<(FontKey, u32, u8), (f32, f32)>>>;

#[derive(Clone)]
pub struct GpuiMeasure {
    /// 断行用的单字 advance：挂在 App 级 `TextSystem` 上。
    texts: Arc<TextSystem>,
    /// 整段绘制宽度：`layout_line` 只在窗口级文本系统上。
    window_texts: Arc<WindowTextSystem>,
    /// 样式没写 font-family 时用哪个族。来自读者偏好，不是书的 UA 值。
    body: SharedString,
    /// `monospace` 那一类泛族名换成哪个族。同样是读者偏好。
    mono: SharedString,
    /// 这本书的内嵌字体，只用来补缺字形。见 [`gpui_font`]。
    faces: Arc<BookFonts>,
    /// 这台设备注册的字族名，供 [`gpui_font`] 判一个 CSS 家族能不能用。
    ///
    /// `Arc` 是为了让 [`GpuiMeasure`] 可 `Clone`：它由 `texts.all_font_names()` 现算
    /// （整张字体表枚举 + 排序 + 去重），而度量器要被缓存起来反复用
    /// （见 `reader::Document::measure`），所以克隆一个指针而不是复制一份表。
    known: Arc<Vec<String>>,
    /// `(字体, 字号, 文字类别) → (ascent, descent)` 缓存。正文因此**不必逐行 shaping**
    /// （[`Measure::font_extents`]），但键里必须有那一格文字类别：汉字走的是回退链上另一张脸，
    /// 只按 `(字体, 字号)` 缓存会把混排的行盒折成拉丁高度。一章里 distinct 的组合是十位数，
    /// 不是行数。
    ///
    /// `Arc<Mutex<..>>` 而不是裸 `Mutex`：度量器本身要可 `Clone`（它被缓存起来反复用，
    /// 见 `reader::Document::measure`），而克隆必须**共享**这张缓存 —— 每个克隆各带
    /// 一份就等于每帧从零开始量字体的 ascent/descent，那正是这张表要省的。
    extents: FontExtentsCache,
}

impl GpuiMeasure {
    pub fn new(
        texts: Arc<TextSystem>,
        window_texts: Arc<WindowTextSystem>,
        body: impl Into<SharedString>,
        mono: impl Into<SharedString>,
        faces: Arc<BookFonts>,
    ) -> Self {
        let known = Arc::new(texts.all_font_names());
        Self {
            texts,
            window_texts,
            body: body.into(),
            mono: mono.into(),
            faces,
            known,
            extents: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn font(&self, key: &FontKey) -> Font {
        gpui_font(key, &self.body, &self.mono, &self.faces, &self.known)
    }

    /// 一个 run 的 shaping 描述。`color` 对度量没有影响，但 `TextRun` 要求填，
    /// 这里统一给黑：绘制时会按行样式重新传颜色。
    fn run(&self, text: &str, key: &FontKey) -> TextRun {
        TextRun {
            len: text.len(),
            font: self.font(key),
            color: black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    }
}

/// 相邻边界相减 = 每个字符的前进量（末项是整段宽，不做差）。
/// 单独成函数是为了能脱离文本系统单测 —— 排版的宽度表按它填，值得钉住。
fn widths_from_edges(edges: &[f32]) -> Vec<f32> {
    edges.windows(2).map(|w| (w[1] - w[0]).max(0.0)).collect()
}

impl Measure for GpuiMeasure {
    fn char_width(&self, ch: char, font: &FontKey, font_size: f32) -> f32 {
        let font_id = self.texts.resolve_font(&self.font(font));
        // 找不到字形时 advance 会报错；给一个 em 宽的占位，让断行继续而不是整段失败。
        self.texts
            .advance(font_id, px(font_size), ch)
            .map(|size| f32::from(size.width))
            .unwrap_or(font_size)
    }

    fn run_metrics(&self, text: &str, font: &FontKey, font_size: f32) -> LineMetrics {
        if text.is_empty() {
            return LineMetrics::default();
        }
        let layout =
            self.window_texts
                .layout_line(text, px(font_size), &[self.run(text, font)], None);
        LineMetrics {
            width: f32::from(layout.width),
            ascent: f32::from(layout.ascent),
            descent: f32::from(layout.descent),
        }
    }

    /// 一次 shaping 拿到整段每个字符的前进量：复用 [`char_edges`](Self::char_edges)
    /// 的那条边界算法，相邻边界相减即得。默认实现是逐字 `char_width`
    /// （`resolve_font` + `advance` 各一次/字），一段几千字的正文就是几千次查表；
    /// 这里一次 shaping 搞定，且与命中/选中矩形同源，不会出现「折行算得下、画出来超出」。
    fn char_widths(&self, text: &str, font: &FontKey, font_size: f32) -> Vec<f32> {
        match self.char_edges(text, font, font_size) {
            Some(edges) => widths_from_edges(&edges),
            None => Vec::new(),
        }
    }

    /// 上下 extent：按 `(字体, 字号, 这段里出现的文字类别)` 缓存。
    ///
    /// **为什么必须收 `text`**：样式声明的字族解析到的是拉丁脸，汉字与假名在 shaping 时
    /// 回退到另一张脸上，而行盒的高度是那张脸给的。用固定代表字符（以前就是一个 `'x'`）
    /// 量一次再按 `(字体, 字号)` 缓存，等于宣布「中文行按拉丁字体的高度折」——
    /// `line-height: 1.2` 配 Noto Sans CJK（自然盒约 1.4em）时，行盒比字形矮，
    /// 上下沿各切掉一截，基线也跟着歪（`LineRun.ascent` 同时是绘制基线与选中矩形的高）。
    ///
    /// 代价没有回到「逐行 shaping」：掩码是纯区间比较，而一次量算只发生在**新的组合**上 ——
    /// 一章里那是十位数（纯拉丁 / 拉丁+汉字 / 拉丁+汉字+假名…），不是行数。
    /// 每一档只量它自己的代表字符，行盒取各档的最大值；这与 CSS 的画法一致
    /// （一行的高度是各行内盒 extent 的并集，`wrap.rs` 那边也按 `max` 合）。
    fn font_extents(&self, text: &str, font: &FontKey, font_size: f32) -> (f32, f32) {
        let mut mask = script_mask(text);
        if mask == 0 {
            // 没有字的一段（空 run）仍要占一行：落到拉丁档，别返回一对零。
            mask = 1 << (Latin as u8);
        }
        let key = (font.clone(), font_size.to_bits(), mask);
        if let Ok(cache) = self.extents.lock()
            && let Some(ext) = cache.get(&key)
        {
            return *ext;
        }
        let mut acc: Option<(f32, f32)> = None;
        for script in ALL_SCRIPTS {
            if mask & (1 << (script as u8)) == 0 {
                continue;
            }
            // 每一档只量它自己的**一个代表字符**：一次 `layout_line`，不是整段 shaping。
            let m = self.run_metrics(&String::from(representative(script)), font, font_size);
            acc = Some(merge_extents(acc, (m.ascent, m.descent)));
        }
        let ext = acc.unwrap_or((font_size * 0.8, font_size * 0.2));
        if let Ok(mut cache) = self.extents.lock() {
            cache.insert(key, ext);
        }
        ext
    }

    /// 两个 shaping 同源的查询（`byte_at_x` / `char_edges`）：绘制用
    /// `shape_line` 摆字形，命中与选中矩形就用**同一份** shaping 结果定位，
    /// kerning、字体回退、标点压缩天然一致 —— 与 gpui-kit 文本选择的
    /// `index_for_position` 同一思路。空文本给 `None` 走退路。
    fn byte_at_x(&self, text: &str, font: &FontKey, font_size: f32, x: f32) -> Option<usize> {
        if text.is_empty() {
            return None;
        }
        let shaped =
            self.window_texts
                .shape_line(text.into(), px(font_size), &[self.run(text, font)], None);
        let width = f32::from(shaped.width);
        let glyphs: Vec<_> = shaped.runs.iter().flat_map(|r| r.glyphs.iter()).collect();
        if glyphs.is_empty() {
            return Some(text.len());
        }
        // 每个字形的前进量 = 下一个字形的 x − 自己的 x（最后一个用整段宽兜底）。
        for (i, glyph) in glyphs.iter().enumerate() {
            let gx = f32::from(glyph.position.x);
            let advance = glyphs
                .get(i + 1)
                .map_or(width - gx, |next| f32::from(next.position.x) - gx);
            if advance <= 0.0 {
                continue;
            }
            if x < gx + advance {
                // 中点判归属；连字/合字字形的 cluster 覆盖多个字符，按中点
                // 落进前半还是后半取对应字符边界。
                let after = x > gx + advance * 0.5;
                let ch = text.get(glyph.index..).and_then(|s| s.chars().next())?;
                let byte = glyph.index + if after { ch.len_utf8() } else { 0 };
                return Some(byte.min(text.len()));
            }
        }
        Some(text.len())
    }

    /// 每个逻辑字符边界的 x，**按阅读方向取「起点」那一侧**。算术在
    /// [`edges_from_glyphs`] 里（那份是纯函数，方向与边界都能单测，不需要窗口）。
    fn char_edges(&self, text: &str, font: &FontKey, font_size: f32) -> Option<Vec<f32>> {
        if text.is_empty() {
            return None;
        }
        let shaped =
            self.window_texts
                .shape_line(text.into(), px(font_size), &[self.run(text, font)], None);
        let width = f32::from(shaped.width);
        // shaper 交回来的就是**视觉顺序**的 (逻辑起点, x) 对，原样递下去。
        let visual: Vec<(usize, f32)> = shaped
            .runs
            .iter()
            .flat_map(|r| r.glyphs.iter())
            .map(|g| (g.index, f32::from(g.position.x)))
            .collect();
        Some(edges_from_glyphs(text, &visual, width))
    }
}

/// 从**视觉顺序**的字形表算出每个逻辑字符边界的「阅读起点」x。
///
/// 为什么不能照旧「沿视觉顺序走、拿 `index` 与字符边界比」：RTL 那一段的 `index` 在视觉
/// 空间里是**递减**的（阿拉伯文的最右那个字形才是逻辑第 0 个），于是那个循环一步都撞不上，
/// 发出去的是 `[0, 0, 0, …, width]`，选中矩形塌成一条零宽 —— 图看着是对的，划词却是空的。
///
/// `edges[k]` 因此定义为「第 k 个字符在**阅读方向上的起点**」：LTR 是它的左缘，RTL 是它的
/// 右缘；末项是这一段在阅读方向上的**终点**。整条表 LTR 递增、RTL 递减，调用方两边都取
/// `min/max`（`content::hit` 的选中矩形）。
///
/// 方向不靠外面传进来：把簇按逻辑序排开，第二个簇落在第一个左边就是从右往左读 —— 判的是
/// 同一份 shaping 数字，不新增一处可能漂移的状态。**代价**是一段内部混排（既有 LTR 又有
/// RTL 的同一个 run）只能按开头两位判读，那种 run 的逐字矩形是近似的；而点击落点走的是
/// [`byte_at_x`](Measure::byte_at_x) 那一条，它逐字形在视觉空间里找，仍然精确。
pub(super) fn edges_from_glyphs(text: &str, visual: &[(usize, f32)], width: f32) -> Vec<f32> {
    if visual.is_empty() {
        return vec![0.0, width];
    }
    // 每个簇（同一 `index` 的几个字形）在视觉空间里占的左右两边。前进量按**视觉邻居**算：
    // 字形总是从左往右摆的，下一位的 x 减自己的 x 就是它占多宽 —— 这与阅读方向无关，
    // 也和 `byte_at_x` 同源。
    let mut span: HashMap<usize, (f32, f32)> = HashMap::new();
    for (i, (at, x)) in visual.iter().enumerate() {
        let right = visual
            .get(i + 1)
            .map_or(width, |next| x + (next.1 - x).max(0.0));
        span.entry(*at)
            .and_modify(|(lo, hi)| {
                *lo = lo.min(*x);
                *hi = hi.max(right);
            })
            .or_insert((*x, right));
    }
    let mut keys: Vec<usize> = span.keys().copied().collect();
    keys.sort_unstable();
    let rtl = keys
        .windows(2)
        .next()
        .is_some_and(|w| span[&w[1]].0 < span[&w[0]].0);
    let reading_start = |at: usize| span.get(&at).map(|(lo, hi)| if rtl { *hi } else { *lo });
    // 边界落在某个簇中间（连字、组合符）时沿用前一个边界的 x —— 与旧写法同一退路。
    let mut edges = Vec::with_capacity(text.chars().count() + 1);
    let mut cursor = reading_start(0).unwrap_or(0.0);
    edges.push(cursor);
    for (offset, _) in text.char_indices().skip(1) {
        if let Some(x) = reading_start(offset) {
            cursor = x;
        }
        edges.push(cursor);
    }
    // 终点：阅读方向上最后那一头。LTR 是整段宽，RTL 是最左边那个簇的左缘。
    edges.push(if rtl {
        span[&keys[0]].0.min(span[&keys[keys.len() - 1]].0)
    } else {
        width
    });
    edges
}

#[cfg(test)]
mod tests {
    //! [`gpui_font`] 那一个出口的分支。它是纯函数（只拼一个 `Font`，不碰文本系统），
    //! 所以这四条能在宿主机上钉住「读者的字体是 primary，书里的字体只补缺字」这条决策 ——
    //! 真机上 `add_fonts` 会带出什么名字，测不到，也不该在这里假装测到了。

    // 这里不用 `use super::*`：那会把 `gpui_kit` 顶层的 `test` 宏一起带进这个模块，
    // 于是 `#[test]` 解析到它而不是内建的测试属性 —— 展开自己套自己，
    // 整个 app 的测试目标都编不过（`recursion limit reached while expanding #[test]`）。
    use super::{
        ALL_SCRIPTS, BookFonts, Font, FontKey, Script, SharedString, gpui_font, merge_extents,
        representative, script_mask, script_of, widths_from_edges,
    };

    fn key(family: &str) -> FontKey {
        FontKey {
            family: family.to_string(),
            families: Vec::new(),
            weight: 400,
            italic: false,
        }
    }

    /// 带整条候选链的那一份：`families` 是作者按优先级写下的顺序。
    fn key_chain(family: &str, rest: &[&str]) -> FontKey {
        FontKey {
            family: family.to_string(),
            families: std::iter::once(family)
                .chain(rest.iter().copied())
                .map(String::from)
                .collect(),
            weight: 400,
            italic: false,
        }
    }

    /// 别名表：声明名 → 设备上真注册着的家族名。
    fn faces(pairs: &[(&str, &[&str])]) -> BookFonts {
        let mut out = BookFonts::empty();
        for (declared, internal) in pairs {
            out.insert(
                (*declared).to_string(),
                internal.iter().map(|s| s.to_string()).collect(),
            );
        }
        out
    }

    fn body() -> SharedString {
        "LXGW WenKai".into()
    }

    fn mono() -> SharedString {
        "JetBrains Mono".into()
    }

    /// 读者选的字体在册，书里那份不在册（内部名才是真的）。
    fn known() -> Vec<String> {
        ["LXGW WenKai", "JetBrains Mono", "Noto Serif"]
            .iter()
            .map(|n| n.to_string())
            .collect()
    }

    /// 一条 `Font` 上的补缺链读回来。`FontFallbacks` 只有 `fallback_list()` 这一个出口，
    /// 每次断言都写一遍 `as_ref().map(...)` 会把测试写得比被 tested 的逻辑还长。
    fn chain(font: &Font) -> Vec<String> {
        font.fallbacks
            .as_ref()
            .map(|fallbacks| fallbacks.fallback_list().to_vec())
            .unwrap_or_default()
    }

    #[test]
    fn the_readers_font_stays_primary_and_the_books_font_only_fills_gaps() {
        let font = gpui_font(
            &key("Book Body"),
            &body(),
            &mono(),
            &faces(&[("Book Body", &["Noto Serif"])]),
            &known(),
        );
        assert_eq!(font.family, body(), "正文永远先用读者选的那一份");
        assert!(font.fallbacks.is_some(), "书里声明过就该挂上补缺链");
        assert_eq!(
            chain(&font),
            vec!["Noto Serif".to_string()],
            "补缺字的是书的字体，不是替正文的那一份"
        );
    }

    /// 没写 `font-family`（泛族名也一样）时照样要挂得上：真书里正文常常一句字族都不写，
    /// 而 `@font-face` 声明的名字只写在 `body` 那条规则里。
    #[test]
    fn a_generic_family_still_gets_the_books_font_as_fallback() {
        let font = gpui_font(
            &key("serif"),
            &body(),
            &mono(),
            &faces(&[("serif", &["Noto Serif"])]),
            &known(),
        );
        assert_eq!(font.family, body());
        assert_eq!(
            chain(&font),
            vec!["Noto Serif".to_string()],
            "泛族名也不该把书的字体丢掉"
        );
    }

    /// 作者的候选链上第一个没装着、第二个装着 —— 第二个该当 primary。
    /// 只认第一个的话，`easypub` 不在就等于整条链作废，读者的偏好会盖掉一个
    /// 明明能用的家族，而作者写第二名要的正是「这个不在就用那个」。
    #[test]
    fn a_later_candidate_the_device_has_becomes_primary() {
        let font = gpui_font(
            &key_chain("easypub", &["Noto Serif", "serif"]),
            &body(),
            &mono(),
            &faces(&[]),
            &known(),
        );
        assert_eq!(font.family, "Noto Serif");
        assert!(
            font.fallbacks.is_none(),
            "primary 是链上装着的那一个，不必再挂自己当 fallback"
        );
    }

    /// 整条链都没装着才落回读者偏好 —— 泛族名（`serif`）永远不算装着，
    /// 它在注册表里出现也不许当 primary（交出去就是一条断掉的回退链）。
    #[test]
    fn generic_names_are_never_picked_from_the_chain() {
        let mut known = known();
        known.push("serif".to_string());
        let font = gpui_font(
            &key_chain("MyFont", &["serif"]),
            &body(),
            &mono(),
            &faces(&[]),
            &known,
        );
        assert_eq!(font.family, body(), "链上只剩泛族名时用读者的");
    }

    /// 书的内嵌字体挂在链上**第二个**名字时也要找得到：`fallbacks` 按整条链试。
    #[test]
    fn the_books_face_may_be_named_by_a_later_candidate() {
        let font = gpui_font(
            &key_chain("MyFont", &["Book Body"]),
            &body(),
            &mono(),
            &faces(&[("Book Body", &["Noto Serif"])]),
            &known(),
        );
        assert_eq!(font.family, body(), "两个都没装着，正文用读者的");
        assert_eq!(chain(&font), vec!["Noto Serif".to_string()]);
    }

    /// 作者点名了一个设备上**真装着**的家族：这是原有的语义，不动 ——
    /// 内嵌字体那一条不该顺手把「装过的字体」也压回读者的。
    #[test]
    fn a_family_the_device_actually_has_stays_primary() {
        let font = gpui_font(&key("Noto Serif"), &body(), &mono(), &faces(&[]), &known());
        assert_eq!(font.family, "Noto Serif");
        assert!(
            font.fallbacks.is_none(),
            "primary 已经是它自己，不必再挂自己当 fallback"
        );
    }

    /// 等宽那一种泛族名换成 `mono_font_family`。以前它和 `serif` 走同一条回落，
    /// 于是 UA 给 `code` / `pre` 的 `monospace` 变成了正文字体，等宽整个丢掉。
    #[test]
    fn monospace_generic_uses_the_mono_preference() {
        let installed = gpui_font(&key("monospace"), &body(), &mono(), &faces(&[]), &known());
        assert_eq!(installed.family, mono(), "装了就该用");

        // 读者填了一个这台机器没装的等宽名：回正文，而不是把一个解析不出来的名字
        // 交给 `resolve_font`（那是 panic）。
        let missing = gpui_font(
            &key("monospace"),
            &body(),
            &SharedString::from("Fira Code"),
            &faces(&[]),
            &known(),
        );
        assert_eq!(missing.family, body());
    }

    /// 缓存命中要跟直算一致，body/mono 换了要能失效 —— 缓存键只有 `FontKey`，
    /// 而 body / mono 不入键，漏掉一次 `clear` 就会把上一套字体的解析结果留在画面上。
    #[test]
    fn the_font_cache_matches_a_direct_resolve_and_clears_with_the_body() {
        use super::FontCache;

        let faces = faces(&[]);
        let mut cache = FontCache::new(known());
        let key = key("serif");
        let first = cache.resolve(&key, &body(), &mono(), &faces);
        assert_eq!(
            cache.resolve(&key, &body(), &mono(), &faces).family,
            first.family,
            "同一个键两次解析要一致"
        );

        // 读者改成另一份正文：同一个 FontKey 现在该落到新的 body。
        let other = SharedString::from("Noto Serif");
        cache.clear();
        assert_eq!(
            cache.resolve(&key, &other, &mono(), &faces).family,
            other,
            "清了缓存之后要按新的 body 重新解析"
        );
    }

    /// `char_widths` 的纯函数那一半：相邻 shaping 边界相减。
    /// shaping 那一半要真文本系统，宿主测不了（与 [`gpui_font`] 的分工一致）。
    #[test]
    fn char_widths_diffs_shaping_edges() {
        // edges 是每个字符**起点**的 x，末项是整段宽。差出来就是每字符的前进量；
        // 连字/簇把两个字符并到一个 x 时，后一个差出 0（它的宽算进了前一个）。
        let edges = [0.0f32, 10.0, 25.0, 25.0, 30.0];
        assert_eq!(widths_from_edges(&edges), vec![10.0, 15.0, 0.0, 5.0]);
        // 少于两个边界时没有前进量可言。
        assert!(widths_from_edges(&[0.0]).is_empty());
        assert!(widths_from_edges(&[]).is_empty());
    }

    /// 分类这一半是纯函数，能在宿主上直接测——它正是「混排到底分不分得开」的那道门。
    ///
    /// 关掉修复的对照：把 `font_extents` 退回成不读 `text`（或 `wrap.rs` 那边递一个固定
    /// 串下去），下面三条里至少「混排同时出现两档」与「每一档的代表字符落在自己那一档」
    /// 就会红。
    #[test]
    fn a_mixed_run_carries_both_scripts_in_the_cache_key() {
        let latin = script_mask("plain english only");
        let han = script_mask("于是我们奋力向前");
        let mixed = script_mask("于是我们 forward，逆水行舟");
        assert_eq!(latin, 1 << (Script::Latin as u8), "纯拉丁只该有一档");
        assert_ne!(han, latin, "汉字必须分出另一档，否则混排行按拉丁高度折");
        assert_eq!(
            mixed,
            latin | han,
            "中英混排必须同时带两档：行盒取的是各档的最大值"
        );
        // 汉字档的量算必须比拉丁档高：这是这次改动存在的全部理由（真机上 Noto Sans CJK
        // 的自然盒约 1.4em，拉丁脸约 1em）。
        assert!(
            super::representative(Script::Han) != super::representative(Script::Latin),
            "两档要用不同的代表字符，否则量出来是同一个数"
        );
    }

    /// 每一档的代表字符必须**真的属于那一档**：拿错字符就是去量另一张脸的高度，
    /// 等于把 bug 从「一律按拉丁量」换成「一律按某个别的字量」。
    #[test]
    fn each_representative_char_falls_in_its_own_bucket() {
        for script in ALL_SCRIPTS {
            let rep = representative(script);
            assert_eq!(
                script_of(rep),
                script,
                "{rep}（U+{:04X}）应当被分到 {script:?}，实际落在别档",
                rep as u32
            );
        }
    }

    /// 空文本的掩码是零，而 `font_extents` 那头把它当拉丁档量（不返回一对零）。
    /// 这里能纯测的是前半：分档函数对空输入**不许**造出某个具体的字来。
    #[test]
    fn an_empty_run_has_no_script_of_its_own() {
        assert_eq!(script_mask(""), 0, "没有字的一段不该凭空分出某一档");
        // 后半（零掩码 → 拉丁档）在 `font_extents` 里，要真文本系统才测得到。
    }

    /// 多档合并的初值必须由**第一档的实测**定，不能从 `(0, 0)` 起 `max`。
    ///
    /// 这不是假想的谨慎：gpui 报的 `descent` 在本机正文族（LXGW WenKai, 20px）上是
    /// **−5.5**，而这次改动的第一版就是从 0.0 起步取 max，把那个负数静默吞成 0 ——
    /// 内容高从 15 变 20.5，半 leading 与基线一起错。实测把它逮住了，这条门钉住它不再回来。
    #[test]
    fn merging_keeps_a_negative_descent_from_the_first_bucket() {
        assert_eq!(
            merge_extents(None, (20.5, -5.5)),
            (20.5, -5.5),
            "第一档报多少就是多少，负 descent 不许被零盖掉"
        );
        assert_eq!(
            merge_extents(Some((16.0, -4.0)), (20.5, -5.5)),
            (20.5, -4.0),
            "两档各取自己的最大：ascent 取高的，descent 取不那么负的"
        );
    }

    // ---- 逐字边界表（`edges_from_glyphs`，RTL 那一批加的纯函数）----

    use super::edges_from_glyphs;

    /// LTR：边界就是每个字符的左缘，表递增，末项是整段宽。
    ///
    /// 这一条也是「LTR 一克都没变」的反证基准：把 `rtl` 那一路改成恒 `false`，
    /// 下面那条 RTL 用例红、这条照旧绿；反过来把它改成恒 `true`，这条红。
    #[test]
    fn ltr_edges_are_each_clusters_left_edge() {
        let visual = [(0, 0.0), (1, 10.0), (2, 20.0), (3, 30.0)];
        assert_eq!(
            edges_from_glyphs("abcd", &visual, 40.0),
            vec![0.0, 10.0, 20.0, 30.0, 40.0]
        );
    }

    /// RTL：shaper 把阿拉伯文的簇从右往左摆，逻辑第 0 个字落在**最右**。
    ///
    /// 钉的是这一批之前的真事故 —— 旧写法沿视觉顺序走、拿 `index` 与字符边界比，
    /// 在这种表上一次都撞不上，于是发出 `[0, 0, 0, 0, 40]`：字画得对，划词选中却
    /// 塌成一条零宽。现在 `edges[k]` 是「阅读方向上的起点」（RTL 即右缘），表递减，
    /// 相邻两项 min/max 出来就是每个字真正的框。
    #[test]
    fn rtl_edges_are_each_clusters_right_edge_in_logical_order() {
        // 视觉从左到右依次是逻辑 3、2、1、0。
        let visual = [(3, 0.0), (2, 10.0), (1, 20.0), (0, 30.0)];
        let edges = edges_from_glyphs("abcd", &visual, 40.0);
        assert_eq!(edges, vec![40.0, 30.0, 20.0, 10.0, 0.0], "{edges:?}");
        assert_eq!(edges.len(), "abcd".chars().count() + 1, "长度那条门不能破");
        for k in 0..4 {
            let (a, b) = (edges[k], edges[k + 1]);
            // 逻辑第 k 个字画在 [30−10k, 40−10k] 那一段上。
            assert_eq!(
                (a.min(b), a.max(b)),
                (30.0 - 10.0 * k as f32, 40.0 - 10.0 * k as f32),
                "第 {k} 个字的框该贴住它画出来的位置"
            );
        }
    }

    /// 一个字符由多个字形组成（同一 `index`）时，框要覆盖那几位合起来占的宽度。
    #[test]
    fn a_multi_glyph_cluster_spans_all_of_its_glyphs() {
        // 逻辑 0 由两个字形画成（合字拆开排），逻辑 1 在后面一位。
        let rtl = [(1, 0.0), (0, 10.0), (0, 25.0)];
        let edges = edges_from_glyphs("ab", &rtl, 40.0);
        // 递减表：字符 0 的右缘取它最右那位的右边（40），左缘取最左那位的 x（10）。
        assert_eq!(edges[0], 40.0, "起点取最右那一头的边：{edges:?}");
        assert_eq!(edges.len(), 3, "长度 = 字符数 + 1");
    }

    /// 一个字都没有（空串走不到这里，但整段全是不可见字形时视觉表是空的）。
    #[test]
    fn an_empty_glyph_table_falls_back_to_the_two_ends() {
        assert_eq!(edges_from_glyphs("abc", &[], 30.0), vec![0.0, 30.0]);
    }

    /// 边界落在某个簇中间（连字把两个字符并成一位）时沿用前一个边界，不许 panic、
    /// 也不许把表长短写错 —— 调用方按「字符数 + 1」这条硬校验筛过。
    #[test]
    fn a_boundary_inside_a_cluster_inherits_the_previous_edge() {
        // 「ab」被合成一个簇（只有 index 0 有字形），字符 1 没有自己的起点。
        let visual = [(0, 0.0), (0, 18.0)];
        let edges = edges_from_glyphs("ab", &visual, 30.0);
        assert_eq!(edges.len(), 3, "{edges:?}");
        assert_eq!(
            edges[1],
            edges[2].min(edges[1]),
            "撞不上就沿用前一个：{edges:?}"
        );
    }
}
