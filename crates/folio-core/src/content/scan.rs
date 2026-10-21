//! 一遍标记扫描就得出章节摘要：可见字数 + 第一个标题的文本 + 文档序的图片引用，
//! 外加文档序的作者样式（内联 `<style>` 与 `<link rel=stylesheet>`）。
//!
//! 存在的理由是导入的账。给一本书每章算 `char_len`（全局字符进度那根尺的分母）和标题
//! 回落，走 `Dom::parse` 要建节点表、父子表，还要 `assign_paths()` 给**每个节点**拼一条
//! 路径字符串 —— 路径是锚点坐标系才用的东西，导入时一次都不读它。这一层只要两样数，
//! 于是让它们在一趟 `Tokenizer` 里跑完，一个节点都不建。
//!
//! ## 口径必须与 DOM 那条逐字相同
//!
//! `char_len` 进了库，读者的位置也进了库：同一本书两次入库算出不同的分母，进度条就会跳。
//! 所以这里照抄的是 DOM 那条的**全部细节**，不是「大致一样」：
//!
//! - 字数：同一份跳过名单（[`ua::is_skipped_tag`]，那 13 个标签），同一个「以第一个
//!   `<body>` 为根」的落点规则，同一个空文本节点不计。
//! - 标题：`Dom::text_of_subtree` 那份名单里能带文本的标签（是 13 个的子集），文档序
//!   第一个 h1/h2/h3，同一句 `split_whitespace().join(" ")`，空串照样算「没有」。
//!
//! 树构造那一边有几条不在文档里的动作，这里也得跟着，否则一章差一个字符：
//!
//! - `<pre>` / `<listing>` / `<textarea>` 之后紧跟的第一个换行**被丢掉**（HTML 的规定，
//!   html5ever 在 `ignore_lf` 里做），所以缩进换行不算进字数。
//! - `NUL` 在 in-body 那一档被树构造直接丢字符（只记一条解析错误），所以也不进字数。
//! - `</body>` **不**结束正文根：树构造不把 body 弹出打开栈，之后的文本（空白与否都一样）
//!   照样插进第一个 body，所以字数一直数到章末。
//! - `<head>` 里出现**非空白**文本时，树构造会当场关掉 head、开一个 body，那段文本于是
//!   落在正文根里；纯空白则留在 head，不进正文。所以「body 从哪儿开始」要看字符而不是标签。
//! - `<title>` / `<style>` / `<script>` 里的文本归它们自己（RCDATA/rawtext），不触发上面
//!   那条 —— 一本正经的书在 `<head>` 里有 `<title>书名</title>`，这是最常见的坑。
//!
//! 对拍：`content::scan::tests` 拿仓库里每一本真书的每一章，逐章比两条路的字数与标题，
//! 不等就红。改动 DOM 那侧的口径时，那边会立刻把这件事喊出来。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use serde::{Deserialize, Serialize};

use crate::content::format::css::ua;

/// 一章扫出来的几样东西。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChapterSummary {
    /// 与 `anchor::TextMap` 同口径的可见字符数。
    pub visible_chars: usize,
    /// 文档序第一个 h1/h2/h3 的文本（空白压成单空格）；没有或全空是 `None`。
    pub heading: Option<String>,
    /// 文档序的每一处图片引用，带着它前面的可见字符数。见 [`ImageUse`]。
    pub images: Vec<ImageUse>,
    /// 文档序的每一块内联 `<style>` 文本（**原样**，含注释与前后空白）。
    ///
    /// 存在的理由与 [`ImageUse`] 同一件事：CHM 没有 OPF，作者样式要么内联在每一页的
    /// `<head>` 里，要么用 `<link>` 指一页兄弟文件，而这两样都要**读过那一页**才知道。
    /// 导入那一趟本来就要逐章跑这里（数字数与图清单），顺路收走是一趟扫描的价钱；
    /// 单开一趟 `Dom::parse` 实测 **4.1 ms / 章**（《哈利·波特》301 章、8.6 MB；
    /// 同一批走本扫描是 2.1 ms / 章），对 CHM 那种「整本都得 LZX 重放」的容器是白加的一倍。
    ///
    /// 这段文本不进 `visible_chars`：DOM 把 RCDATA 归 `<style>` 自己，两边同口径，
    /// 对拍那一组盯着这件事。
    pub styles: Vec<String>,
    /// 文档序的每一个 `<link rel="stylesheet">` 的 href，**未解析**的原样引用。
    ///
    /// 与 [`ImageUse::src`] 同一个立场：扫描器不认容器，所以清洗（去 query / fragment、
    /// 百分号解码）与按本章目录解析都留给格式层做 —— 只有「算不算样式表」这一句判据
    /// 需要共用（[`is_stylesheet_rel`]）。
    pub stylesheets: Vec<String>,
}

/// `<link rel="…">` 里算作者样式表的那一条判据：`rel` 按空白切词，**任一词**是
/// `stylesheet`（大小写不敏感）。
///
/// 抽出来只因为这一句在两处要用：本扫描（CHM 那一族，不建 DOM）与 `format::htmlz`
/// 的 DOM 走查。两份各写一遍迟早漂在一句 `rel="alternate stylesheet"` 或
/// `rel="STYLESHEET"` 上，而漂的症状是「这本书的样式整包看不见」，屏上没有线索。
///
/// 认「任一词」而不是「整串相等」是照 HTML 的规定：`rel` 是一个**令牌列表**。
/// 于是 `rel="icon stylesheet"` 算样式表，`rel="icon"` 不算。
pub(crate) fn is_stylesheet_rel(rel: &str) -> bool {
    rel.split_whitespace()
        .any(|token| token.eq_ignore_ascii_case("stylesheet"))
}

/// 正文里的一处图片引用：作者写下的原样 `src`，加上它在这一章里的位置。
///
/// 存在的理由是一件排版给不出来的事：**整本书**那一列图要在读者走到它之前就知道
/// 「一共有几张、这一张在第几章第几个字」。排版只认识「已经排好的那一章」，
/// 而这一列要的是清单 —— 导入那一趟已经在逐章跑 [`ChapterScan`]，顺手记下来是
/// 一趟扫描的价钱，换掉的是「为下一张图把一章排一遍」。
///
/// `src` 是**未解析**的那一条原样引用（`<img src="images/a.png">` 里那一段），
/// 与 [`crate::layout::ports::ResolvedImage::href`] 不同：后者要按本章目录解一遍、
/// 还要对得上那本书的图片表，那是界面侧 [`ChapterImages::resolve`] 的口径。这里存原样
/// 是因为扫描器不认容器 —— 而**同一句解析只做一次**才不会出现「列里画得出、正文里画不出」
/// 那种两本账。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageUse {
    /// 作者写下的那一条引用（相对本章目录）。
    pub src: String,
    /// 这一处前面已经数到的可见字符数 —— 与 `Locator.offset` 同一个轴。
    ///
    /// 指到图片**之前**那一个字：图片自己在那个轴上不占一格（它是 U+FFFC 一个替换元素，
    /// 由排版插在文本流里，不由这里的字数器数进来）。差这一个字落在同一个行盒里，
    /// 「定位原文」要的就是那一行。
    pub offset: usize,
}

/// HTML 的空元素。树构造不会把它们压进「打开的元素」栈：后面出现的文本是它们的
/// **兄弟**，不是孩子。跳过名单里的 `br` 与那几个元信息标签都在这一列里 ——
/// 不排掉的话，一个没闭合的 `<br>` 会把往后整章都当成「在跳过子树里」而全部漏计。
const VOID_TAGS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// 标题回落认的三级，与 `epub::heading_of` 同一份。
const HEADINGS: &[&str] = &["h1", "h2", "h3"];

/// 进入外来内容的标签：只有在 SVG / MathML 里头，自闭合 `<svg><title/></svg>` 才真的闭合。
const FOREIGN_TAGS: &[&str] = &["svg", "math"];

/// 这三个标签之后紧跟的第一个换行会被树构造丢掉。
const LF_AFTER_START: &[&str] = &["pre", "listing", "textarea"];

/// 正文根（**第一个** `<body>`）走到哪一段。
///
/// 只有两段，因为**它不会关**：`</body>` 不把 body 弹出打开栈（HTML 的规定，实测过），
/// 之后的文本照样插进第一个 body，所以计到章末为止。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Root {
    /// 还没开始：head 里的空白不进正文，而 head 里的散文本会当场把 body 带出来。
    Before,
    /// 正在正文根里。
    Open,
}

/// 扫一遍的状态。字段全是 `Cell` / `RefCell`：`TokenSink::process_token` 只要 `&self`。
struct Scan {
    /// 可见字符数。
    chars: Cell<usize>,
    /// 正文根走到哪一段。
    root: Cell<Root>,
    /// 当前打开的、需要整块跳过的元素名。空 = 这一段可见。
    skip: RefCell<Vec<&'static str>>,
    /// 外来内容（SVG / MathML）的嵌套深度：只有里头才尊重自闭合标签。
    foreign: Cell<usize>,
    /// 下一个字符标记要先丢掉的开头换行（`ignore_lf`）。
    ignore_lf: Cell<bool>,
    /// 正在收第一个标题：它在等的那个闭合标签。
    heading_tag: RefCell<Option<&'static str>>,
    heading_text: RefCell<String>,
    /// 标题要么已经收完，要么这本书没有标题 —— 两种情况都不该再开第二个。
    heading_done: Cell<bool>,
    /// 文档序的图片引用（见 [`ImageUse`]）。
    uses: RefCell<Vec<ImageUse>>,
    /// 正在收的那一块 `<style>` 的文本；`</style>` 到达时整块搬进 `styles`。
    style_text: RefCell<String>,
    /// 文档序的内联样式块。
    styles: RefCell<Vec<String>>,
    /// 文档序的 `<link rel=stylesheet>` 引用。
    sheets: RefCell<Vec<String>>,
}

impl Default for Scan {
    fn default() -> Self {
        Self {
            chars: Cell::new(0),
            root: Cell::new(Root::Before),
            skip: RefCell::new(Vec::new()),
            foreign: Cell::new(0),
            ignore_lf: Cell::new(false),
            heading_tag: RefCell::new(None),
            heading_text: RefCell::new(String::new()),
            heading_done: Cell::new(false),
            uses: RefCell::new(Vec::new()),
            style_text: RefCell::new(String::new()),
            styles: RefCell::new(Vec::new()),
            sheets: RefCell::new(Vec::new()),
        }
    }
}

impl Scan {
    /// 这一段字符进不进可见文本：正文根里、且不在被跳过的子树里。
    fn counting(&self) -> bool {
        self.root.get() == Root::Open && self.skip.borrow().is_empty()
    }

    /// 这一处引用在不在**正文流**里（图片引用只在流里才算一张）。
    ///
    /// 两种「在」：正文根已经开且不在被跳过的子树里；或者根还没开而这一处会当场把
    /// body 带出来 —— `<head>` 里的 `<img>` 在树构造里就是「关掉 head、开一个 body、
    /// 把这一张插进去」，与散文本触发 body 是同一条规则。
    ///
    /// 不算的是被整块跳过的子树里头（`<script>` / `<style>` / `<template>` 那一族）：
    /// 那里头的一张图在正文坐标系里没有位置，排不出来，也就没人画它。
    fn in_flow(&self) -> bool {
        let skip = self.skip.borrow();
        match self.root.get() {
            Root::Open => skip.is_empty(),
            Root::Before => skip.last().is_none_or(|top| *top == "head"),
        }
    }

    /// 这一个开始标签是一次图片引用吗？是的话交出作者写下的那一条**原样**引用。
    ///
    /// 认的形状与 `layout::collect::picture_src` 同一套，一条都不许多：
    /// - `<img src>` —— 一直认的那一种。
    /// - 外来内容里的 `<image href>`（`xlink:` 那一头 html5ever 拆成前缀，`name.local`
    ///   就是 `href`），退一步认 `<image src>`。
    /// - HTML 内容里的 `<image>` 按 `<img>` 看（树构造就是这么改的），所以那一支只看 `src`。
    ///
    /// `<svg>` 自己不记：排版把「带一张 `<image>` 的 svg」当**一张**图，收了这个就不往下递归
    /// （`wrap.rs::push_node` 那句「不再递归它的子节点」），记的是里头那张。
    fn picture_ref(name: &str, tag: &Tag, foreign: usize) -> Option<String> {
        let attr = |want: &str| {
            tag.attrs
                .iter()
                .find(|a| {
                    // 标记这一层**还没拆**属性名的前缀 —— `xlink:href` 整串就是名字，
                    // 拆它是树构造那一步（`adjust foreign attributes`）的事。这里先按最后一个
                    // `:` 切开再比，认的才是 DOM 那一条认的同一个属性。
                    let found: &str = &a.name.local;
                    found.rsplit(':').next() == Some(want)
                })
                .map(|a| a.value.to_string())
                .filter(|value| !value.is_empty())
        };
        match name {
            "img" => attr("src"),
            "image" if foreign > 0 => attr("href").or_else(|| attr("src")),
            "image" => attr("src"),
            _ => None,
        }
    }

    /// 这一个开始标签是一次样式表引用吗？是的话交出作者写下的那一条 `href` 原样引用。
    ///
    /// `rel` 的判据只有 [`is_stylesheet_rel`] 那一份；`href` 为空（`<link rel=stylesheet href="">`）
    /// 不算一次引用，与图片那一支「空值不记」同一口径。属性名按最后一个 `:` 切开再比，
    /// 与 [`Self::picture_ref`] 同一个理由：标记这一层还没拆前缀。
    fn stylesheet_ref(name: &str, tag: &Tag) -> Option<String> {
        if name != "link" {
            return None;
        }
        let attr = |want: &str| {
            tag.attrs
                .iter()
                .find(|a| a.name.local.rsplit(':').next() == Some(want))
                .map(|a| a.value.to_string())
        };
        attr("rel").filter(|rel| is_stylesheet_rel(rel))?;
        attr("href").filter(|href| !href.trim().is_empty())
    }

    /// 名单里的名字取成 `'static` 的那一份，栈里存它就一次分配都没有。
    fn known(list: &[&'static str], name: &str) -> Option<&'static str> {
        list.iter().copied().find(|known| *known == name)
    }

    fn tag(&self, tag: &Tag) {
        let name = &*tag.name;
        let opening = tag.kind == TagKind::StartTag;
        // 自闭合只在外来内容里真的闭合；HTML 内容里 `<title/>` 之类照样是个开标签。
        let self_closed = tag.self_closing && self.foreign.get() > 0;

        if opening {
            if Self::known(FOREIGN_TAGS, name).is_some() {
                self.foreign.set(self.foreign.get() + 1);
            }
            if name == "body" {
                // 树构造在这里把 head 关掉，所以挂在 head 上的跳过栈要清空。
                self.skip.borrow_mut().clear();
                if self.root.get() == Root::Before {
                    self.root.set(Root::Open);
                }
            }
            if Self::known(LF_AFTER_START, name).is_some() {
                self.ignore_lf.set(true);
            }
            // 图片引用按文档序记下来，位置用**那一刻已经数到的**可见字符数。
            // 放在跳过栈与正文根都更新之后：`<body>` 那一格要先抬起来，
            // 一本 `<body><img .../></body>` 的第一张图才不会被判成「根还没开」。
            if let Some(src) = Self::picture_ref(name, tag, self.foreign.get())
                && self.in_flow()
            {
                self.uses.borrow_mut().push(ImageUse {
                    src,
                    offset: self.chars.get(),
                });
            }
            if !self_closed {
                if let Some(skipped) = ua::skipped_tag(name) {
                    // 空元素不进打开栈（`br` 就在跳过名单里，必须排掉）。
                    if Self::known(VOID_TAGS, name).is_none() {
                        self.skip.borrow_mut().push(skipped);
                    }
                }
                self.open_heading(name);
                // 开一块新的 `<style>`：把残留清掉，免得上一块没闭合的字串进这一块。
                if name == "style" {
                    self.style_text.borrow_mut().clear();
                }
            }
            // 样式表引用：`<link>` 是空元素、不进跳过栈，也不问正文根 —— 它写在 `<head>`
            // 里是常态，而样式本来就不占正文坐标系的一格。
            if let Some(href) = Self::stylesheet_ref(name, tag) {
                self.sheets.borrow_mut().push(href);
            }
        } else {
            if Self::known(FOREIGN_TAGS, name).is_some() {
                self.foreign.set(self.foreign.get().saturating_sub(1));
            }
            // `</body>` 什么都不做：它不把 body 弹出打开栈，之后的文本仍归第一个 body。
            if self.skip.borrow().last() == Some(&name) {
                self.skip.borrow_mut().pop();
            }
            if name == "style" {
                // 收完这一块。文本可能是空的（`<style></style>`），那种不进清单 ——
                // 拼接时它一个字符都不加，留着只会让「这本书有几块内联样式」多算。
                let text = std::mem::take(&mut *self.style_text.borrow_mut());
                if !text.trim().is_empty() {
                    self.styles.borrow_mut().push(text);
                }
            }
            if self
                .heading_tag
                .borrow()
                .map(|t| t == name)
                .unwrap_or(false)
            {
                self.close_heading();
            }
        }
    }

    /// 文档序第一个 h1/h2/h3 开始收。标题在 `<head>` 里也照收 —— DOM 那侧是全局找元素，
    /// 不看正文根。
    fn open_heading(&self, name: &str) {
        if self.heading_done.get() {
            return;
        }
        let Some(heading) = Self::known(HEADINGS, name) else {
            return;
        };
        // 标题嵌标题（`<h1><h2>`）在树构造里会先隐式关掉前一个，这里同一件事实：
        // 收的就是这一个，前一个到此为止。
        if self.heading_tag.borrow().is_some() {
            self.close_heading();
            return;
        }
        *self.heading_tag.borrow_mut() = Some(heading);
    }

    fn close_heading(&self) {
        *self.heading_tag.borrow_mut() = None;
        self.heading_done.set(true);
    }

    fn characters(&self, text: &str) {
        // 树构造在这里丢一个开头换行；这里也丢，两边数出来的才是同一串。
        let mut text = text;
        if self.ignore_lf.take()
            && let Some(rest) = text.strip_prefix('\n')
        {
            text = rest;
        }
        if text.is_empty() {
            return;
        }
        // `<style>` 里的字符归样式表自己（DOM 那一侧是 RCDATA），所以这段既不进字数也不
        // 开正文根 —— 但**收走必须在这里，在那两个 `return` 之前**。实测：把这一段挪到
        // 「head 里散文本」那一支之后，`<head>` 里的样式块整块丢掉（`stray_in_head` 为假
        // 就 return 了），只有 body 里那一块还在 —— 症状是 CHM 的书「样式表只有一半」，
        // 而字数与进度条一点不变，屏上看着全对。
        if self.skip.borrow().last() == Some(&"style") {
            self.style_text.borrow_mut().push_str(text);
            return;
        }
        let blank = text.chars().all(char::is_whitespace);
        if self.root.get() == Root::Before {
            // `<head>` 里的散文本会让树构造当场开 body；纯空白不触发，而
            // `<title>` 这类自带文本的元素里的字符归它自己，也不触发。
            let top = self.skip.borrow().last().copied();
            let stray_in_head = top.is_none() || top == Some("head");
            if !(stray_in_head && !blank) {
                return;
            }
            // 树构造的顺序是「pop head → push body → 重新处理这个字符」，所以这一步
            // 之后 head 不再压栈：那段文本是正文根的孩子，不是 head 的孩子。
            if top == Some("head") {
                self.skip.borrow_mut().pop();
            }
            self.root.set(Root::Open);
        }
        if self.counting() {
            self.chars.set(self.chars.get() + text.chars().count());
        }
        if self.heading_tag.borrow().is_some() && self.skip.borrow().is_empty() {
            self.heading_text.borrow_mut().push_str(text);
        }
    }

    fn finish(&self) -> ChapterSummary {
        // 标题没闭合就到底：DOM 在 `end()` 里会把打开的元素全部弹掉，文本照样归它，
        // 所以这里收的就是那个结果。
        let heading = {
            let raw = self.heading_text.borrow();
            let joined = raw.split_whitespace().collect::<Vec<_>>().join(" ");
            (!joined.is_empty()).then_some(joined)
        };
        ChapterSummary {
            visible_chars: self.chars.get(),
            heading,
            images: std::mem::take(&mut self.uses.borrow_mut()),
            styles: {
                // 到章末还开着的 `<style>`（作者少写了闭合标签，真实导出里见得到）：
                // DOM 那边这段文本仍归 style 节点，所以这里也收走，不丢。
                let last = std::mem::take(&mut *self.style_text.borrow_mut());
                let mut blocks = std::mem::take(&mut *self.styles.borrow_mut());
                if !last.trim().is_empty() {
                    blocks.push(last);
                }
                blocks
            },
            stylesheets: std::mem::take(&mut self.sheets.borrow_mut()),
        }
    }
}

impl TokenSink for Scan {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        match token {
            Token::TagToken(tag) => self.tag(&tag),
            Token::CharacterTokens(text) => self.characters(&text),
            // NUL 不进字数：树构造在 in-body 那一档只记一条解析错误，不插字符
            // —— 对拍打出来的：DOM 那边「甲\0乙」是 2 个字，不是 3 个。
            Token::NullCharacterToken => {}
            // 注释、DOCTYPE、解析错误都不产文本。
            // 注释 / DOCTYPE / 解析错误都不产文本，但 `ignore_lf` 是一次性的，要在这里清掉。
            _ => self.ignore_lf.set(false),
        }
        TokenSinkResult::Continue
    }
}

/// 扫一章，得出可见字数与第一个标题。
///
/// 一次都不建 DOM：`Tokenizer` 直接吐标记，状态就是上面那几个格子。
pub fn chapter_summary(xhtml: &str) -> ChapterSummary {
    let mut scan = ChapterScan::new();
    scan.push_str(xhtml);
    scan.finish()
}

/// 可分段喂的扫描器：整串喂（[`chapter_summary`]）与一边解压一边喂走的都是它。
///
/// 分块边界可能落在一个多字节字符中间，所以留最多 3 字节的尾巴到下一块 —— html5ever
/// 自己那个 `from_utf8` 流式解码器做的是同一件事。非法字节按 `from_utf8_lossy` 的口径
/// 换成一个 U+FFFD：**必须一样**，因为 `Zip::read_text` 给 DOM 那条的输入正是 lossy
/// 换过的字符串，两条路遇到坏字节要数出同一个数（对拍里有一条专门打这个）。
pub(crate) struct ChapterScan {
    tokenizer: Tokenizer<ScanHandle>,
    state: Rc<Scan>,
    /// 末尾没凑齐一个字符的字节（UTF-8 最长 4 字节，所以最多 3 个）。
    tail: Vec<u8>,
}

impl ChapterScan {
    pub(crate) fn new() -> Self {
        let state = Rc::new(Scan::default());
        Self {
            tokenizer: Tokenizer::new(ScanHandle(Rc::clone(&state)), TokenizerOpts::default()),
            state,
            tail: Vec::new(),
        }
    }

    /// 喂一块**已经解压**的字节。
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.tail.is_empty() {
            self.consume(bytes);
            return;
        }
        // 只有带着尾巴时才发现要拼一份，常见情况是零拷贝地直接用进来的那块。
        let mut joined = std::mem::take(&mut self.tail);
        joined.extend_from_slice(bytes);
        self.consume(&joined);
    }

    /// 喂一段已经是合法 UTF-8 的文本（整串那条路）。
    pub(crate) fn push_str(&mut self, text: &str) {
        let queue = BufferQueue::default();
        queue.push_back(StrTendril::from(text));
        let _ = self.tokenizer.feed(&queue);
    }

    /// 把一块字节切成「合法前缀 + 要么尾巴要么一个坏序列」，逐段喂。
    fn consume(&mut self, bytes: &[u8]) {
        self.tail.clear();
        let mut rest = bytes;
        loop {
            // 常见的一趟就是这里：整块合法就整块喂出去，一次校验。
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    self.push_str(text);
                    return;
                }
                Err(err) => {
                    let valid = err.valid_up_to();
                    if valid > 0 {
                        self.push_str(
                            std::str::from_utf8(&rest[..valid])
                                .expect("valid_up_to 给的就是合法前缀的长度"),
                        );
                        rest = &rest[valid..];
                    }
                    match err.error_len() {
                        // 一个坏序列 → lossy 换成一个 U+FFFD，与 `read_text` 同口径。
                        Some(len) => {
                            self.push_str("\u{fffd}");
                            rest = &rest[len.min(rest.len())..];
                        }
                        // 只是被切断了：留到下一块再定。
                        None => {
                            self.tail.extend_from_slice(rest);
                            return;
                        }
                    }
                }
            }
        }
    }

    /// 收尾：尾巴里剩下的是凑不齐一个字符的字节，按 lossy 换字符（与整串那条一致）。
    pub(crate) fn finish(mut self) -> ChapterSummary {
        if !self.tail.is_empty() {
            let lossy = String::from_utf8_lossy(&self.tail).into_owned();
            self.push_str(&lossy);
        }
        self.tokenizer.end();
        self.state.finish()
    }
}

/// `Tokenizer` 要把 sink 拿走，所以状态放在 `Rc` 里、外面留一份引用取结果。
struct ScanHandle(Rc<Scan>);

impl TokenSink for ScanHandle {
    type Handle = ();

    fn process_token(&self, token: Token, line: u64) -> TokenSinkResult<()> {
        self.0.process_token(token, line)
    }

    fn end(&self) {
        self.0.end();
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{ChapterScan, ChapterSummary, ImageUse, chapter_summary};
    use crate::content::document;
    use crate::content::dom::{Dom, NodeId, NodeKind};
    use crate::content::format::css::ua;
    use crate::content::format::epub::tests::published_shape_epub;
    use crate::content::format::visible_chars;

    /// 参照实现：文档序的图片引用（作者写下的原样 `src` + 它前面的可见字符数），
    /// 走 DOM 原语 —— 与扫描器那条「一次都不建 DOM」的路互为对照，两条各写一遍。
    ///
    /// 这条为什么值得写：那一列清单就是图片查看器念的「第 3 张 / 共 27 张」，
    /// 顺序错了读者滑到的就是别人的那一张，而屏幕上看不出来它错 —— 计数对得上、
    /// 图也画得出来，只是每一张都指错了地方。
    fn dom_image_uses(xhtml: &str) -> Vec<ImageUse> {
        let dom = Dom::parse(xhtml.as_bytes());
        // 正文根与 `visible_chars` 同一选法：有 `<body>` 从它往下走，没有就整棵当根。
        let root = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map_or(dom.document, |(id, _)| id);
        let mut out = Vec::new();
        dom_walk_images(&dom, root, &mut 0usize, &mut out);
        out
    }

    /// 深度优先按文档序走一遍，累加可见字符，遇到图片引用就记一条。
    fn dom_walk_images(dom: &Dom, node: NodeId, chars: &mut usize, out: &mut Vec<ImageUse>) {
        let n = dom.get(node);
        if let Some(tag) = dom.tag_name(node) {
            // 被跳过的子树整个不进（`<script>` / `<style>` / `<template>` 那一族）：
            // 那里头的标签在正文坐标系里没有位置。
            if ua::is_skipped_tag(tag) {
                return;
            }
            // `<img>` 与外来内容里的 `<image>` 才是引用；命中就不再往下递归 ——
            // 排版把一张图当**一个**行内对象，`wrap.rs::push_node` 那句
            // 「不再递归它的子节点」说的是同一件事，svg 里头那张 `image` 由这一条交出。
            if let Some(src) = dom_picture_ref(dom, node, tag) {
                out.push(ImageUse {
                    src,
                    offset: *chars,
                });
                return;
            }
        }
        if n.kind == NodeKind::Text {
            *chars += n.text.chars().count();
            return;
        }
        for &child in n.children.iter() {
            dom_walk_images(dom, child, chars, out);
        }
    }

    /// DOM 那一条认的那一族（`layout::collect::picture_src` 同形）：`<img src>` 与
    /// `<image href>`（退一步 `<image src>`）。空值不算一次引用。
    ///
    /// HTML 内容里写 `<image>` 不用另外处理：树构造按规范把它改成 `<img>` 再插，
    /// 所以这里读到的标签名已经是 `img`，与扫描器那条「不在外来内容里就只看 `src`」
    /// 是同一条规则的两种写法。
    fn dom_picture_ref(dom: &Dom, node: NodeId, tag: &str) -> Option<String> {
        let attr = |name: &str| {
            dom.attr(node, name)
                .map(str::to_string)
                .filter(|value| !value.is_empty())
        };
        match tag {
            "img" => attr("src"),
            "image" => attr("href").or_else(|| attr("src")),
            _ => None,
        }
    }

    /// 参照实现：第一个 h1/h2/h3 的文本，走 DOM 原语。
    ///
    /// 这就是 `epub` 那条**改前**的标题回落（原函数已随接线删掉，逻辑原样搬来当参照）：
    /// 全局找第一个标题元素，`text_of_subtree` 收文本，空白压成单空格。
    fn dom_heading(xhtml: &str) -> Option<String> {
        let dom = Dom::parse(xhtml.as_bytes());
        let id = dom.elements().find(|(_, n)| {
            matches!(
                n.tag.as_ref().map(|t| t.local.as_ref()),
                Some("h1") | Some("h2") | Some("h3")
            )
        })?;
        let text = dom
            .text_of_subtree(id.0)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        (!text.is_empty()).then_some(text)
    }

    /// 按字节切块喂一遍（切点会落在多字节字符中间，正是解压流给的东西）。
    fn chunked(xhtml: &str, chunk: usize) -> ChapterSummary {
        let mut scan = ChapterScan::new();
        for piece in xhtml.as_bytes().chunks(chunk) {
            scan.push(piece);
        }
        scan.finish()
    }

    /// 三条路必须给同一个答案：**流式分块** = **整串扫描** = **DOM**。
    /// 这条不是「差得不多就上」的门。
    fn same(xhtml: &str, label: &str) {
        let stream = chapter_summary(xhtml);
        let dom_chars = visible_chars(xhtml).expect("DOM 那条给的该是个数");
        let dom_heading = dom_heading(xhtml);
        let stream_chars = stream.visible_chars;
        assert_eq!(
            stream_chars, dom_chars,
            "「{label}」字数不一致：流式 {stream_chars} vs DOM {dom_chars}\n{xhtml:?}"
        );
        assert_eq!(
            stream.heading.as_deref(),
            dom_heading.as_deref(),
            "「{label}」标题不一致：流式 {:?} vs DOM {dom_heading:?}\n{xhtml:?}",
            stream.heading
        );
        // 图片清单：顺序、原样引用、以及它前面数到第几个字，三样都要逐字相同。
        let dom_uses = dom_image_uses(xhtml);
        assert_eq!(
            stream.images, dom_uses,
            "「{label}」图片清单不一致：流式 {:?} vs DOM {dom_uses:?}\n{xhtml:?}",
            stream.images
        );
        // 生产里索引那一趟走的是分块喂（`EntryReader::stream`）：切块不许改变任何一个数。
        for chunk in [1usize, 7, 4096, 65536] {
            let cut = chunked(xhtml, chunk);
            assert_eq!(
                cut, stream,
                "「{label}」按 {chunk} 字节切块喂，答案与整串不一致：{cut:?} vs {stream:?}"
            );
        }
    }

    /// 挑过形状的用例：每一条都在打上面文档里写明的一条规则。
    /// 少打一条，字数就会在一本真书上偏掉，而那种偏差是**读者看得见**的（进度条跳）。
    const CASES: &[(&str, &str)] = &[
        (
            "普通段落",
            "<html><body><p>一二三</p><p>四五六</p></body></html>",
        ),
        (
            "正文里的缩进与换行",
            "<html><body>\n  <p>\n    一二三\n  </p>\n</body></html>",
        ),
        (
            "pre 之后紧跟的换行被丢掉",
            "<html><body><pre>\n第一行\n第二行</pre></body></html>",
        ),
        (
            "listing 同样丢一个换行",
            "<html><body><listing>\n字</listing></body></html>",
        ),
        (
            "textarea 丢换行但它的文本仍算可见",
            "<html><body><textarea>\neditable 内容</textarea></body></html>",
        ),
        ("NUL 变成一个字符", "<html><body>甲\u{0}乙</body></html>"),
        (
            "head 里的 title 不进正文",
            "<html><head><title>书名</title></head><body>正文</body></html>",
        ),
        (
            "head 里的散文本会当场开 body",
            "<html><head>散在 head 的字</head><body>正文</body></html>",
        ),
        (
            "head 里的空白不开 body",
            "<html><head>\n  \n</head><body>正文</body></html>",
        ),
        (
            "head 没闭合",
            "<html><head><title>书名</title><body>正文</body>",
        ),
        ("body 之前的缩进", "\n   <html><body>正文</body></html>"),
        (
            "body 内的 script 与 style",
            "<html><body><script>var a = 1;</script><style>p{color:red}</style>字</body></html>",
        ),
        (
            "注释不进字数",
            "<html><body>甲<!--注释不进-->乙</body></html>",
        ),
        (
            "字符实体按解出来的长度算",
            "<html><body>&amp;&#20013;&nbsp;&lt;</body></html>",
        ),
        (
            "br 不吃掉后面的文本",
            "<html><body>甲<br>乙<br/>丙</body></html>",
        ),
        (
            "svg 里的自闭合 title 真的闭合",
            "<html><body><svg><title/></svg>字</body></html>",
        ),
        (
            "html 里的自闭合 title 不算闭合",
            "<html><body><title/>字</body></html>",
        ),
        ("没有 body 的片段", "<p>一段</p><p>两段</p>"),
        ("纯空白片段", "   \n\t  "),
        ("空串", ""),
        ("空段落", "<html><body><p></p><p></p></body></html>"),
        (
            "标题套标题取内层先出现的",
            "<html><body><h1>外层<h2>内层</h2></h1><p>正文</p></body></html>",
        ),
        (
            "标题里的行内标签与空白压成单空格",
            "<html><body><h3>  甲\n<span>乙</span>　丙  </h3><p>正文</p></body></html>",
        ),
        (
            "标题里混 style",
            "<html><body><h1>甲<style>x{}</style>乙</h1></body></html>",
        ),
        ("没有标题", "<html><body><p>只有正文</p></body></html>"),
        (
            "空白标题等于没有",
            "<html><body><h1>   </h1><p>正文</p></body></html>",
        ),
        (
            "大写的 h1",
            "<html><body><H1>标题</H1><p>正文</p></body></html>",
        ),
        ("标题没闭合就到结尾", "<html><body><h2>没闭合"),
        (
            "XHTML 声明与 CDATA",
            "<html><body><p>甲<![CDATA[乙]]></p></body></html>",
        ),
        (
            "bogus comment",
            "<?xml version=\"1.0\"?><html><body>字</body></html>",
        ),
        (
            "body 之后的文本仍算第一个 body",
            "<html><body>正文</body>后面也算</html>",
        ),
        (
            "template 整块跳过",
            "<html><body>甲<template>不算</template>乙</body></html>",
        ),
        (
            "属性里带尖括号",
            "<html><body><p title=\"a>b\">字</p></body></html>",
        ),
        (
            "表格里散文本（foster parent）",
            "<html><body><table>散字<tr><td>格内</td></tr></table></body></html>",
        ),
        (
            "frameset 文档",
            "<html><frameset><frame><noframes>字</noframes></frameset>",
        ),
        (
            "多字节与全角空格",
            "<html><body>中文，标点。　全角</body></html>",
        ),
        (
            "嵌套的跳过子树",
            "<html><body><div><head><style>x</style></head>字</div></body></html>",
        ),
        // ── 图片引用那一族（`ImageUse` 的那一列）───────────────────────────────
        // 每一条都在打 `layout::collect::picture_src` 认的其中一种形状，或它不该认的
        // 那一种：这一列是图片查看器「第 n 张 / 共 m 张」的唯一来源，多一张少一张
        // 读者都滑得出来。
        (
            "正文里的两张插图按文档序",
            "<html><body><p>甲</p><img src=\"a.png\"/><p>乙</p><img src=\"b.png\"></body></html>",
        ),
        (
            "同一张图用两次算两条",
            "<html><body><img src=\"a.png\"/>字<img src=\"a.png\"/></body></html>",
        ),
        (
            "svg 套一张 image（Calibre 封面那一族）",
            "<html><body><svg><image xlink:href=\"cover.jpeg\"/></svg>字</body></html>",
        ),
        (
            "svg 套两张 image",
            "<html><body><svg><image href=\"l.png\"/><image href=\"r.png\"/></svg></body></html>",
        ),
        (
            "没有 src 的 img 不算",
            "<html><body><img/><img src=\"\">字</body></html>",
        ),
        (
            "src 为空的 image 不算",
            "<html><body><svg><image href=\"\"/></svg></body></html>",
        ),
        (
            "head 里的 img 被挪进当场开出的 body",
            "<html><head><img src=\"h.png\"/></head><body>正文</body></html>",
        ),
        (
            "script 里的 img 字符串不算一张",
            "<html><body><script>var s = '<img src=\"x.png\"/>';</script>字</body></html>",
        ),
        (
            "template 里的 img 不算",
            "<html><body>甲<template><img src=\"t.png\"/></template>乙</body></html>",
        ),
        (
            "行内包一层 span 的 img",
            "<html><body><p>字<span><img src=\"i.png\"></span>后</p></body></html>",
        ),
        (
            "figure 套 figcaption 里的 img",
            "<html><body><figure><img src=\"f.png\"/><figcaption>图说</figcaption></figure></body></html>",
        ),
        (
            "表格里的 img（foster parent 会挪出来）",
            "<html><body><table><tr><td><img src=\"c.png\"/></td></tr></table>字</body></html>",
        ),
        (
            "HTML 内容里写 image 按 img 看",
            "<html><body><image src=\"g.png\"/>字</body></html>",
        ),
    ];

    #[test]
    fn the_streaming_scan_matches_the_dom_on_every_shape_we_fought() {
        for (label, xhtml) in CASES {
            same(xhtml, label);
        }
    }

    /// 清单上那两个数**钉死成字面量**的一条。
    ///
    /// 上面那条对拍拿 DOM 当参照，两条一起错它照旧绿；而这一列的 `offset` 就是
    /// 「定位原文」要跳进去的那个字 —— 它悄悄偏一格，屏幕上看不出来，只是跳过去之后
    /// 那一张不在顶上而在半屏之下。所以这一条不复用任何参照实现，数字是手算的。
    #[test]
    fn the_offsets_are_the_visible_chars_before_the_picture() {
        // 「甲乙」= 2 字，图在它们之后；「丙丁戊」= 3 字，第二张图在其后。
        let xhtml = "<html><body><p>甲乙</p><img src=\"a.png\"/><p>丙丁戊</p><img src=\"b.png\"></body></html>";
        assert_eq!(
            chapter_summary(xhtml).images,
            [
                ImageUse {
                    src: "a.png".into(),
                    offset: 2
                },
                ImageUse {
                    src: "b.png".into(),
                    offset: 5
                },
            ],
            "两张图按文档序，位置各是它前面已经数到的可见字符数"
        );
        // 图片自己不占一格：那一列的尺是可见文本，U+FFFC 那个哨兵由排版插在流里。
        let only = "<html><body><img src=\"a.png\"/></body></html>";
        assert_eq!(chapter_summary(only).images[0].offset, 0);
    }

    /// 把一本书的每一章都拿来对拍。
    fn against_a_book(path: &Path) -> (usize, usize) {
        let doc = document::open(path).unwrap_or_else(|_| panic!("{} 该打得开", path.display()));
        let count = doc
            .flow()
            .map(|flow| flow.chapters.len())
            .unwrap_or_default();
        let mut chars_total = 0usize;
        for ix in 0..count {
            let text = doc
                .chapter_source(ix)
                .unwrap_or_else(|_| panic!("第 {ix} 章正文该取得到"));
            same(
                &text,
                &format!("{} 第 {ix} 章", path.file_name().unwrap().to_string_lossy()),
            );
            chars_total += chapter_summary(&text).visible_chars;
        }
        (count, chars_total)
    }

    fn fixture(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    /// 解压流里可能撞见两种「不是合法 UTF-8」的情况：一个坏字节，和末尾被切断的半个字符。
    ///
    /// DOM 那条吃的是 `Zip::read_text` 的产物 —— 也就是 `from_utf8_lossy` 换过的串，
    /// 所以流式这边必须换出**同样**的字符数，不然同一本坏书两次入库会给出两个分母。
    #[test]
    fn broken_and_split_bytes_count_like_the_lossy_string() {
        let mut bytes: Vec<u8> = b"<html><body><h1>\xe6\x96\x87\xe7\xab\xa0".to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFE]); // 两个坏字节 → lossy 给两个 U+FFFD
        bytes.extend_from_slice("字</h1><p>正文</p>".as_bytes());
        bytes.extend_from_slice(&[0xE4, 0xB8]); // 末尾半个「一」：lossy 也是一个 U+FFFD

        let lossy = String::from_utf8_lossy(&bytes).into_owned();
        let want = visible_chars(&lossy).expect("DOM 那条给的该是个数");
        let want_heading = dom_heading(&lossy);
        for chunk in [1usize, 3, 17, 4096] {
            let mut scan = ChapterScan::new();
            for piece in bytes.chunks(chunk) {
                scan.push(piece);
            }
            let got = scan.finish();
            assert_eq!(
                got.visible_chars, want,
                "按 {chunk} 字节切：坏字节与半截字符的 lossy 口径不一致"
            );
            assert_eq!(
                got.heading.as_deref(),
                want_heading.as_deref(),
                "按 {chunk} 字节切：标题不一致"
            );
        }
    }

    /// 仓库里那两份（合成的出版书形状 + 合成小样本）：一次 `cargo test` 就要跑到的门。
    ///
    /// 这一条以前读的是 `tests/fixtures/local/` 那本真书，而那个目录在 `.gitignore` 里
    /// （`.gitignore` 顶上就写着「本地真书样本一律不入库」）—— 于是 CI 上它不是跳过而是
    /// NotFound 红。输入必须在仓库里，所以换成 `published_shape_epub` 当场写的那一份；
    /// 真书那一族进下面 `#[ignore]` 的那条。
    #[test]
    fn the_streaming_scan_matches_the_dom_chapter_by_chapter_in_the_repo_books() {
        let (chapters, chars) =
            against_a_book(&fixture("../../tests/fixtures/oss/minimal-zh.epub"));
        assert!(chapters > 0 && chars > 0, "合成书该有正文可对");
        let dir = tempfile::tempdir().unwrap();
        let (chapters, chars) = against_a_book(&published_shape_epub(&dir));
        assert_eq!(chapters, 28, "28 章都过了一遍对拍");
        assert!(chars > 250, "逐章对拍覆盖的字数太少了：{chars}");
    }

    /// 本机那两本真书不进每次跑批：一次几秒，而且输入不在仓库里，`--ignored` 单独跑。
    #[test]
    #[ignore = "全库对拍：1225 章两遍解析，几秒"]
    fn the_streaming_scan_matches_the_dom_on_the_local_books() {
        let (chapters, chars) = against_a_book(&fixture(
            "../../tests/fixtures/local/斗战狂潮校对版全本.epub",
        ));
        assert_eq!(chapters, 1225);
        assert!(chars > 4_000_000, "对拍覆盖的字数：{chars}");
        let (chapters, chars) =
            against_a_book(&fixture("../../tests/fixtures/local/shejie-mauriac.epub"));
        assert_eq!(chapters, 28, "《蛇结》28 章都过了一遍对拍");
        assert!(chars > 100_000, "对拍覆盖的字数：{chars}");
    }

    // ---- 作者样式：内联 `<style>` 与 `<link rel=stylesheet>` ----

    /// 内联样式块按文档序交出，而**一个字都不进字数**。
    ///
    /// 前半句是放置位置的闸门：capture 挪到「head 里散文本」那一支之后，`<head>` 里那块
    /// CSS 就整个丢掉、只剩 body 那块（实测过），而字数一点不动 —— 样式少一半而进度条
    /// 照旧，是最难查的那一类。所以后半句也要钉：字数直接对 DOM 那一侧
    /// （[`visible_chars`]）比，不比一个我自己算出来的数。
    #[test]
    fn inline_style_blocks_come_back_in_document_order_without_touching_the_count() {
        let xhtml = "<html><head><style>p { color: red }</style></head>\
                     <body><p>甲</p><style>q { color: blue }</style><p>乙</p></body></html>";
        let summary = chapter_summary(xhtml);
        assert_eq!(
            summary.styles,
            vec![
                "p { color: red }".to_string(),
                "q { color: blue }".to_string()
            ],
            "head 与 body 里各一块，按文档序"
        );
        assert_eq!(summary.visible_chars, 2, "两个汉字的正文，CSS 一个都不算");
        assert_eq!(
            summary.visible_chars,
            visible_chars(xhtml).expect("参照侧该解析得动"),
            "字数与 DOM 那条逐字相同"
        );

        // 样式文本本身也与 DOM 那条同串：`raw_text_of_subtree` 取 style 子树就是它，
        // 那正是 `format::html` 现在拿去做作者样式的同一份字节。
        let dom = Dom::parse(xhtml.as_bytes());
        let from_dom: Vec<String> = dom
            .elements()
            .filter(|(id, _)| dom.tag_name(*id) == Some("style"))
            .map(|(id, _)| dom.raw_text_of_subtree(id))
            .collect();
        assert_eq!(from_dom.len(), 2, "DOM 那侧也看到两块：{from_dom:?}");
        assert_eq!(summary.styles, from_dom, "两条路交出的是同一串字节");
    }

    /// `<link>` 只收样式表那几处，`rel` 按令牌列表认（大小写不敏感、任一词命中）。
    ///
    /// 空 `href` 不算一次引用，`rel` 缺位也不算 —— 与图片那一支「空值不记」同口径，
    /// 否则会拼出一条指不到任何条目的路径。
    #[test]
    fn only_links_that_are_stylesheets_are_collected() {
        let summary = chapter_summary(
            "<head><link rel=\"icon\" href=\"f.ico\">\
             <link rel=\"stylesheet\" href=\"a.css\">\
             <LINK REL=\"alternate STYLESHEET\" HREF=\"b.css\"></head><body><p>甲</p></body>",
        );
        assert_eq!(
            summary.stylesheets,
            vec!["a.css", "b.css"],
            "{:?}",
            summary.stylesheets
        );

        let none = chapter_summary("<link rel=\"stylesheet\" href=\"\"><link href=\"c.css\">");
        assert!(none.stylesheets.is_empty(), "{:?}", none.stylesheets);
    }

    /// 作者少写 `</style>`（导出的书里见得到）：到章末还开着的那一块照样交出去，
    /// 而空块（`<style> </style>`）不进清单 —— 拼接时它一个字符都不加，留着只会让
    /// 「这本书有几块内联样式」多算。
    #[test]
    fn an_unclosed_style_is_flushed_and_an_empty_one_is_dropped() {
        let open = chapter_summary("<html><head><style>p{color:red}");
        assert_eq!(open.styles, vec!["p{color:red}".to_string()]);
        assert_eq!(open.visible_chars, 0, "这段文本始终不进字数");

        let blank = chapter_summary("<style>   </style><p>甲</p>");
        assert!(blank.styles.is_empty(), "{:?}", blank.styles);
        assert_eq!(blank.visible_chars, 1);
    }
}
