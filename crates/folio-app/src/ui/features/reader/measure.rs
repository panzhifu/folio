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
    known: Vec<String>,
    /// `(字体, 字号) → (ascent, descent)` 缓存。字体度量与文本无关，所以正文可
    /// 绕过逐行 shaping 取它（[`Measure::font_extents`]）；一章 distinct 的样式很少。
    extents: Mutex<HashMap<(FontKey, u32), (f32, f32)>>,
}

impl GpuiMeasure {
    pub fn new(
        texts: Arc<TextSystem>,
        window_texts: Arc<WindowTextSystem>,
        body: impl Into<SharedString>,
        mono: impl Into<SharedString>,
        faces: Arc<BookFonts>,
    ) -> Self {
        let known = texts.all_font_names();
        Self {
            texts,
            window_texts,
            body: body.into(),
            mono: mono.into(),
            faces,
            known,
            extents: Mutex::new(HashMap::new()),
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

    /// 字体上下 extent，按 `(字体, 字号)` 缓存 —— 正文行盒几何不再逐行 shaping。
    /// 量一个代表字符即可：`run_metrics` 报的是字体度量，与具体文本基本无关。
    fn font_extents(&self, font: &FontKey, font_size: f32) -> (f32, f32) {
        let key = (font.clone(), font_size.to_bits());
        if let Ok(cache) = self.extents.lock()
            && let Some(ext) = cache.get(&key)
        {
            return *ext;
        }
        let m = self.run_metrics("x", font, font_size);
        let ext = (m.ascent, m.descent);
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

    fn char_edges(&self, text: &str, font: &FontKey, font_size: f32) -> Option<Vec<f32>> {
        if text.is_empty() {
            return None;
        }
        let shaped =
            self.window_texts
                .shape_line(text.into(), px(font_size), &[self.run(text, font)], None);
        let width = f32::from(shaped.width);
        let glyphs: Vec<_> = shaped.runs.iter().flat_map(|r| r.glyphs.iter()).collect();
        // edges[k] = 第 k 个字符起点的 x；edges[0] 恒为 0（run 的原点）。
        // 字符边界找不到恰好落在它之上的字形（shaping 把它并进了前一个
        // cluster，比如连字）时，沿用前一个边界的 x。
        let mut edges = vec![0.0f32];
        let mut cursor = 0.0f32;
        let mut gi = 0usize;
        for (offset, _) in text.char_indices().skip(1) {
            while gi < glyphs.len() && (glyphs[gi].index as usize) < offset {
                cursor = f32::from(glyphs[gi].position.x);
                gi += 1;
            }
            if let Some(glyph) = glyphs.get(gi)
                && glyph.index == offset
            {
                cursor = f32::from(glyph.position.x);
            }
            edges.push(cursor);
        }
        edges.push(width);
        Some(edges)
    }
}

#[cfg(test)]
mod tests {
    //! [`gpui_font`] 那一个出口的分支。它是纯函数（只拼一个 `Font`，不碰文本系统），
    //! 所以这四条能在宿主机上钉住「读者的字体是 primary，书里的字体只补缺字」这条决策 ——
    //! 真机上 `add_fonts` 会带出什么名字，测不到，也不该在这里假装测到了。

    // 这里不用 `use super::*`：那会把 `gpui_kit` 顶层的 `test` 宏一起带进这个模块，
    // 于是 `#[test]` 解析到它而不是内建的测试属性 —— 展开自己套自己，
    // 整个 app 的测试目标都编不过（`recursion limit reached while expanding #[test]`）。
    use super::{BookFonts, Font, FontKey, SharedString, gpui_font, widths_from_edges};

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
}
