//! 一遍标记扫描就得出章节摘要：可见字数 + 第一个标题的文本。
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

use crate::content::format::css::ua;

/// 一章扫出来的两样东西。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChapterSummary {
    /// 与 `anchor::TextMap` 同口径的可见字符数。
    pub visible_chars: usize,
    /// 文档序第一个 h1/h2/h3 的文本（空白压成单空格）；没有或全空是 `None`。
    pub heading: Option<String>,
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
        }
    }
}

impl Scan {
    /// 这一段字符进不进可见文本：正文根里、且不在被跳过的子树里。
    fn counting(&self) -> bool {
        self.root.get() == Root::Open && self.skip.borrow().is_empty()
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
            if !self_closed {
                if let Some(skipped) = ua::skipped_tag(name) {
                    // 空元素不进打开栈（`br` 就在跳过名单里，必须排掉）。
                    if Self::known(VOID_TAGS, name).is_none() {
                        self.skip.borrow_mut().push(skipped);
                    }
                }
                self.open_heading(name);
            }
        } else {
            if Self::known(FOREIGN_TAGS, name).is_some() {
                self.foreign.set(self.foreign.get().saturating_sub(1));
            }
            // `</body>` 什么都不做：它不把 body 弹出打开栈，之后的文本仍归第一个 body。
            if self.skip.borrow().last() == Some(&name) {
                self.skip.borrow_mut().pop();
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

    use super::{ChapterScan, ChapterSummary, chapter_summary};
    use crate::content::document;
    use crate::content::dom::Dom;
    use crate::content::format::visible_chars;

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
    ];

    #[test]
    fn the_streaming_scan_matches_the_dom_on_every_shape_we_fought() {
        for (label, xhtml) in CASES {
            same(xhtml, label);
        }
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

    /// 合成书与那本 0.6 MB 的真书：一次 `cargo test` 就要跑到的门。
    #[test]
    fn the_streaming_scan_matches_the_dom_chapter_by_chapter_in_the_repo_books() {
        let (chapters, chars) =
            against_a_book(&fixture("../../tests/fixtures/oss/minimal-zh.epub"));
        assert!(chapters > 0 && chars > 0, "合成书该有正文可对");
        let (chapters, chars) =
            against_a_book(&fixture("../../tests/fixtures/local/shejie-mauriac.epub"));
        assert_eq!(chapters, 28, "《蛇结》28 章都过了一遍对拍");
        assert!(chars > 100_000, "对拍覆盖的字数太少了：{chars}");
    }

    /// 大部头那本（1225 章 / 430 万字）不进每次跑批：一次几秒，`--ignored` 单独跑。
    #[test]
    #[ignore = "全库对拍：1225 章两遍解析，几秒"]
    fn the_streaming_scan_matches_the_dom_on_the_big_book() {
        let (chapters, chars) = against_a_book(&fixture(
            "../../tests/fixtures/local/斗战狂潮校对版全本.epub",
        ));
        assert_eq!(chapters, 1225);
        assert!(chars > 4_000_000, "对拍覆盖的字数：{chars}");
    }
}
