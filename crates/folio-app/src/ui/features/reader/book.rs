//! 一本打开着的书：章节表、锚章、以及**锚章 ± 窗口**拼出来的一条全局坐标。
//!
//! 与 `folio_core::content::document::Document`（统一文档接口）的分工：那一边是「这本书里
//! 有什么」（章、目录、图片元数据、按 href 现取字节），这一边是「界面读到哪、
//! 排版成什么样」。一本书换章只是换这一层持有的排版状态，容器不重开。
//!
//! 连续滚动把**锚章 ± [`SPAN_WINDOW`]** 的章拼成一条全局 y 坐标（见
//! [`Self::placements`]，全书唯一的拼接公式）：滚过章尾自然接下一章，锚章跟随
//! 视口顶迁移，窗外章节裁掉、邻章提前排版入库。分页 / 双页不拼坐标，仍然只用
//! 锚章那一份（[`Self::document`]，所有既有调用点都只认它）。
//!
//! 拼接的正确性压在一条不变式上：**任何结构变动（迁移、裁剪、新章进链）之后，
//! 视口顶那一行字的全局位置必须不动**——base 动了多少，`scroll_y` 就补多少
//! （[`Self::compensate_spine`] / [`Self::migrate_spine`]）。它成立的前提是
//! **章高不可变**：图片固有尺寸在排版期就从元数据拿到，不依赖异步字节。
//!
//! 章节的源文本**已经是引擎就绪的 XHTML**（契约见 `format::Chapter`），provider
//! 在解析时就把 Markdown / TXT 包好了，所以这里没有「按格式先包一层」的分支，
//! `Format` 也不参与这条链（见 `plans/README.md` 的架构决策 ④）。

use std::collections::HashMap;
use std::sync::Arc;

use folio_core::content::document::Document as OpenedDocument;
use folio_core::content::format::TocEntry;
use folio_core::{Chapter, Locator, Reader};
use gpui_kit::{App, SharedString};

use super::book_fonts::BookFonts;
use super::document::Document;
use super::toc_tree::{self, TocRow};
use crate::ui::shared::images::{BookImages, ChapterImages, document_source};

/// 章文本缓存的半窗宽：手里留「当前章 ± 3」章的正文。
///
/// 翻章是局部行为，相邻章来回翻要免费 —— 这正是缓存的来历；从目录跳到远处则旧
/// 窗口整块让出去。3 章的余量盖住了「下一章 / 上一章」的来回折腾，千章书的正文
/// 驻留也从「整本」变成常数量级（至多 7 章）。
const TEXT_WINDOW: usize = 3;

/// 拼进连续坐标的邻章半窗宽：锚章上下各预排这么多章。
///
/// 读者会狂飙滚动：窗口给到 ±2，配合后台单线程排版队列（毫秒级一章），正常速度
/// 下接缝永远提前排好；狂飙时也至少还有一章的余量，滚到未就绪的边界只会停一拍，
/// 不会白屏。驻留上界 5 份排版，仍是常数量级。
const SPAN_WINDOW: usize = 2;

/// 拼接坐标里的一个邻章：哪一章 + 它自己的排版状态。
///
/// 没排完的章（`doc.height() == 0`）不参与坐标，`[`Book::placements`]` 会跳过它。
pub(super) struct Span {
    pub(super) index: usize,
    doc: Document,
}

/// 一章在全局坐标里的摆位（只读投影）：起点、高度，都是排版像素。
pub(super) struct Placement {
    pub(super) index: usize,
    pub(super) base_y: f32,
    pub(super) height: f32,
}

/// 一台设备上打开着的一本书：章节表、锚章、以及锚章 ± 窗口的排版状态。
pub struct Book {
    title: SharedString,
    chapters: Vec<Chapter>,
    /// 目录树，原样从文档拿（EPUB 的 nav.xhtml / toc.ncx）。单文件格式（TXT / MD / FB2）
    /// 与定版式没有它 —— 那时 [`Self::toc_rows`] 退回章节表，读者那屏目录不该是空的。
    toc: Vec<TocEntry>,
    /// 锚章：连续滚动时是「视口顶所在的那一章」。页眉章名、进度、落库全跟它走。
    chapter: usize,
    /// 整本书的图片索引，换章时按新章的目录重建视图。字节按需从文档现取。
    book_images: Arc<BookImages>,
    /// 锚章自己的排版状态。**永在** —— 分页 / 双页与全部既有调用点都只认它。
    document: Document,
    /// 邻章 Span（不含锚章），按章号升序，至多 `2 × [`SPAN_WINDOW`]` 个。
    spans: Vec<Span>,
    /// 书自己的样式表。新开 Span 要灌同一份作者层规则。
    css: String,
    /// 整本书的内嵌字体（别名表 + 已经注册进设备的家族名）。各章共用一份，
    /// 注册进设备只有一次，所以由 `Book` 持有，而不是每章自己注册一遍。
    faces: Arc<BookFonts>,
    /// 打开着的这份文档。正文不在库里，只存位置（`ChapterLocator`），所以要留着它
    /// 才能按位置回容器取。
    doc: Arc<dyn OpenedDocument>,
    /// 已经取过的章正文，只留「当前章 ± [`TEXT_WINDOW`]」。
    ///
    /// 单文件格式（TXT / MD / FB2）取一章 = 重读整本再分一次章，来回翻同一章不该付
    /// 第二次；EPUB 虽然只解一个条目，缓存一下也省一对 zip 中央目录。窗口随换章
    /// 裁剪（见 [`Self::set_anchor`]），窗外的章让出去 —— 驻留上界是常数量级，
    /// 不再随阅读进度吃下整本。
    texts: HashMap<usize, String>,
}

impl Book {
    /// 从一份已打开的文档建出一本书，定位到 `locator` 那一章。
    ///
    /// `cx` 用于注册内嵌字体（进程级文本系统），所以这一步要有 `App`。
    pub fn new(doc: Arc<dyn OpenedDocument>, prefs: Reader, locator: Locator, cx: &App) -> Self {
        // 章节表只对可重排文档有意义。定版式（CBZ）目前没有正文排版链，
        // 落到这里就是「没有章节」—— 界面照起，只是这一屏空着，不 panic。
        let chapters = match doc.flow() {
            Some(flow) => flow.chapters.clone(),
            None => Vec::new(),
        };
        // 样式表按位置清单回容器现取（索引路径下库/文档里只有清单）。
        let css = doc.stylesheet();
        let chapter = (locator.chapter as usize).min(chapters.len().saturating_sub(1));

        // 图片索引与内嵌字体别名表都从这份文档建一次；字节按需现取（`ByteSource` 持一份
        // `Arc<dyn Document>`，所以 `doc` 不必再存进 `Book`）。
        tracing::info!(
            chapters = chapters.len(),
            images = doc.images().len(),
            "Book::new: 建索引"
        );
        let book_images = Arc::new(BookImages::new(doc.images(), document_source(doc.clone())));
        let fonts = BookFonts::for_document(doc.as_ref(), &css, cx);
        let document = Document::new(
            String::new(),
            &css,
            prefs.clone(),
            Arc::new(ChapterImages::empty()),
            fonts.clone(),
        );
        let mut this = Self {
            title: doc.meta().title.clone().into(),
            chapters,
            toc: doc.toc().to_vec(),
            chapter,
            book_images,
            document,
            spans: Vec::new(),
            css,
            faces: fonts,
            doc,
            texts: HashMap::new(),
        };
        this.set_anchor(chapter, &prefs);
        this
    }

    /// 拼接坐标里两章之间的缝。跟着字号走（2em）并给一条下限：它是「上一章结束、
    /// 下一章开始」的呼吸空隙，不是装饰条。
    pub(super) fn chapter_gap(prefs: &Reader) -> f32 {
        (prefs.font_size * 2.0).max(24.0)
    }

    /// 把锚章挪到 `index`。旧锚章降级为邻居（排版结果留着，`advance(±1)` 走到
    /// 相邻章时可以直接复用），新锚章优先取窗口里现成的 Span，窗外整块裁掉。
    ///
    /// 只动结构与缓存，**不算全局坐标也不补偿滚动** —— 那是渲染帧里
    /// `relayout_spine` / `migrate_spine` 的事；调用方（跳章、恢复）要么随后挂
    /// 恢复请求，要么自己带着快照去补。
    pub(super) fn set_anchor(&mut self, index: usize, prefs: &Reader) {
        if index >= self.chapters.len() {
            return;
        }
        let old_index = self.chapter;
        self.chapter = index;
        // 新锚章在窗口里已有现成排版（`advance(±1)` 的常例）就摘下来复用；
        // **摘完**再降级旧锚章、裁窗外 —— 裁剪以「锚章不在 `spans` 里」为前提，
        // 顺序反了会把正要复用的那份排版当垃圾扔掉，锚章也会跟着来回打转。
        let reused = self.take_span(index);
        if old_index != index {
            let vacant = Document::new(
                String::new(),
                &self.css,
                prefs.clone(),
                Arc::new(ChapterImages::empty()),
                self.faces.clone(),
            );
            let previous = std::mem::replace(&mut self.document, vacant);
            self.spans.push(Span {
                index: old_index,
                doc: previous,
            });
        }
        self.prune_spans();
        match reused {
            Some(span) => self.document = span.doc,
            None => {
                let text = self.chapter_text_at(index);
                // 图片索引必须跟着换章重建：`<img src>` 是相对本章目录的，
                // 上一章的基准目录解不出下一章的路径。
                let images = self
                    .chapters
                    .get(index)
                    .map(|chapter| {
                        Arc::new(ChapterImages::new(
                            self.book_images.clone(),
                            &chapter.spine_href,
                        ))
                    })
                    .unwrap_or_else(|| Arc::new(ChapterImages::empty()));
                self.document.set_chapter(text);
                self.document.set_images(images);
            }
        }
        prune_texts(&mut self.texts, index);
    }

    /// 把锚章 ± 窗口的邻居补齐入库（0 高占位，排完自动进坐标链）。
    ///
    /// 只创建、不裁剪、不算坐标：0 高的章不进 [`Self::placements`]，已进链章的
    /// base 一个都不动，所以**不需要滚动补偿**。渲染帧开头调它，排版作业这就能
    /// 排进后台队列 —— 接缝因此总是提前铺好。
    pub(super) fn ensure_window_spans(&mut self, prefs: &Reader) {
        let lo = self.chapter.saturating_sub(SPAN_WINDOW);
        let hi = (self.chapter + SPAN_WINDOW).min(self.chapters.len().saturating_sub(1));
        for index in lo..=hi {
            if index == self.chapter || self.spans.iter().any(|span| span.index == index) {
                continue;
            }
            let span = self.make_span(index, prefs);
            self.spans.push(span);
        }
        self.spans.sort_by_key(|span| span.index);
    }

    /// 窗口之外的邻居整块让出去。锚章自己不会住在 `spans` 里。
    fn prune_spans(&mut self) {
        let lo = self.chapter.saturating_sub(SPAN_WINDOW);
        let hi = self.chapter + SPAN_WINDOW;
        self.spans
            .retain(|span| span.index != self.chapter && lo <= span.index && span.index <= hi);
    }

    fn take_span(&mut self, index: usize) -> Option<Span> {
        let position = self.spans.iter().position(|span| span.index == index)?;
        Some(self.spans.remove(position))
    }

    fn make_span(&mut self, index: usize, prefs: &Reader) -> Span {
        let text = self.chapter_text_at(index);
        let images = self
            .chapters
            .get(index)
            .map(|chapter| {
                Arc::new(ChapterImages::new(
                    self.book_images.clone(),
                    &chapter.spine_href,
                ))
            })
            .unwrap_or_else(|| Arc::new(ChapterImages::empty()));
        let mut doc = Document::new(
            String::new(),
            &self.css,
            prefs.clone(),
            images,
            self.faces.clone(),
        );
        doc.set_chapter(text);
        Span { index, doc }
    }

    /// 第 `index` 章的正文，按位置回容器取（取过就记住）。
    ///
    /// 取不到时给空串并写一条错误日志：界面能把这一屏画出来（其它章还能读），
    /// 而「源文件不在了」这条消息在日志与 `chapter_unavailable` 上看得见。
    fn chapter_text_at(&mut self, index: usize) -> String {
        if let Some(text) = self.texts.get(&index) {
            return text.clone();
        }
        match self.doc.chapter_source(index) {
            Ok(text) => {
                self.texts.insert(index, text.clone());
                text
            }
            Err(err) => {
                tracing::error!(chapter = index + 1, error = %err, "Book: 取不到正文");
                String::new()
            }
        }
    }

    /// 参与拼接的章，按章号升序摆上全局坐标。**这是拼接坐标的唯一公式**：
    /// 基线就地累计，所有读数（`chapter_at_y` / `base_of` / `content_height`）
    /// 与补偿全都走它。
    ///
    /// 只有「有排版结果」的章进链（`height() > 0`）：没排完的邻居高度记 0，
    /// 排完的那一刻内容从边界长出来 —— 滚到界先停一拍，比估一个错误高度再跳
    /// 回去体面。`set_prefs` 作废排版时行盒仍留着（上一份结果），链不会塌，
    /// 滚动位置在新结果回来前保持原值。
    pub(super) fn placements(&self, gap: f32) -> Vec<Placement> {
        let mut out: Vec<Placement> = self
            .spans
            .iter()
            .filter(|span| span.doc.height() > 0.0)
            .map(|span| Placement {
                index: span.index,
                base_y: 0.0,
                height: span.doc.height(),
            })
            .collect();
        if self.document.height() > 0.0 {
            out.push(Placement {
                index: self.chapter,
                base_y: 0.0,
                height: self.document.height(),
            });
        }
        out.sort_by_key(|placement| placement.index);
        let mut y = 0.0;
        for placement in &mut out {
            placement.base_y = y;
            y += placement.height + gap;
        }
        out
    }
    /// 全局 y 落在哪一章。两章之间的缝归**后一章**：视口顶进了缝，
    /// 前一章的最后一行已经在视口之上了。
    pub(super) fn chapter_at_y(&self, y: f32, gap: f32) -> Option<usize> {
        let placements = self.placements(gap);
        let mut owner = placements.first()?.index;
        for placement in &placements {
            if y < placement.base_y - gap {
                break;
            }
            owner = placement.index;
            if y < placement.base_y + placement.height {
                break;
            }
        }
        Some(owner)
    }

    /// 第 `index` 章在全局坐标里的起点。不在链里（没排完 / 被裁）是 `None`。
    pub(super) fn base_of(&self, index: usize, gap: f32) -> Option<f32> {
        self.placements(gap)
            .into_iter()
            .find(|placement| placement.index == index)
            .map(|placement| placement.base_y)
    }

    /// 全局内容总高：最后一条链成员的末端。谁都没排完就是 0。
    pub(super) fn content_height(&self, gap: f32) -> f32 {
        self.placements(gap)
            .last()
            .map(|placement| placement.base_y + placement.height)
            .unwrap_or(0.0)
    }

    /// 第 `index` 章的排版状态（锚章或窗口内的邻居）。
    pub(super) fn span_doc(&self, index: usize) -> Option<&Document> {
        if index == self.chapter {
            return Some(&self.document);
        }
        self.spans
            .iter()
            .find(|span| span.index == index)
            .map(|span| &span.doc)
    }

    /// 「第 `offset` 个字（`chapter` 章内坐标）」的全局行顶。恢复落点用。
    pub(super) fn global_y_of_offset(
        &self,
        chapter: usize,
        offset: usize,
        gap: f32,
    ) -> Option<f32> {
        let base = self.base_of(chapter, gap).unwrap_or(0.0);
        self.span_doc(chapter)
            .map(|doc| base + doc.y_at_offset(offset))
    }

    /// 视口顶的快照：`(哪一章, 它此刻的 base)`。排版收结果前后各读一次，
    /// 差额就是 [`Self::compensate_spine`] 要补的数。
    pub(super) fn spine_snapshot(&self, scroll_y: f32, gap: f32) -> Option<(usize, f32)> {
        let index = self.chapter_at_y(scroll_y, gap)?;
        let base = self.base_of(index, gap)?;
        Some((index, base))
    }

    /// 按快照补偿 `scroll_y`：那一章的 base 动了多少，滚动位置就补多少 ——
    /// 「视口顶那一行字」在屏幕上纹丝不动。章被裁出链（不可能发生在快照与
    /// 补偿之间）或从未进链时不动。
    pub(super) fn compensate_spine(
        &mut self,
        snapshot: Option<(usize, f32)>,
        scroll_y: &mut f32,
        gap: f32,
    ) {
        let Some((index, old)) = snapshot else {
            return;
        };
        let Some(new) = self.base_of(index, gap) else {
            return;
        };
        *scroll_y += new - old;
    }

    /// 锚章迁移：视口顶跨进了邻章，就把锚章挪过去（窗口跟着重新裁剪），并补偿
    /// 滚动位置让屏上内容不动。
    ///
    /// 锚章自己还没排完时**不迁移**：那时「视口顶在哪一章」没有可信答案 ——
    /// 改字号的重排在飞、跳章刚落地都走这里，等锚章排完的那一帧自然会答。
    pub(super) fn migrate_spine(&mut self, scroll_y: &mut f32, gap: f32, prefs: &Reader) {
        if !self.document.is_current() {
            return;
        }
        let Some(owner) = self.chapter_at_y(*scroll_y, gap) else {
            return;
        };
        if owner == self.chapter {
            return;
        }
        let snapshot = self.base_of(owner, gap).map(|base| (owner, base));
        self.set_anchor(owner, prefs);
        self.compensate_spine(snapshot, scroll_y, gap);
    }

    /// 拼进坐标窗口的章（含锚章）。测试读：窗口必须钉在锚章 ± [`SPAN_WINDOW`]。
    #[cfg(test)]
    pub(in crate::ui::features::reader) fn span_indices(&self) -> Vec<usize> {
        let mut out: Vec<usize> = self.spans.iter().map(|span| span.index).collect();
        out.push(self.chapter);
        out.sort_unstable();
        out
    }

    /// 全部排版状态（锚章优先，邻居随后）。渲染帧逐个收排版结果用：
    /// 锚章先提交，恢复落点才不用多等一帧。
    pub(super) fn docs_mut(&mut self) -> impl Iterator<Item = &mut Document> {
        std::iter::once(&mut self.document).chain(self.spans.iter_mut().map(|span| &mut span.doc))
    }

    /// 读者偏好变了：所有进窗的章一起作废重排。
    pub(super) fn set_prefs_on_all(&mut self, prefs: Reader) {
        self.document.set_prefs(prefs.clone());
        for span in &mut self.spans {
            span.doc.set_prefs(prefs.clone());
        }
    }

    /// 往某个方向换一章。真的换了返回 `true`，到头返回 `false`（按钮据此置灰）。
    pub fn advance(&mut self, delta: isize, prefs: &Reader) -> bool {
        let target = self.chapter as isize + delta;
        if target < 0 || target as usize >= self.chapters.len() {
            return false;
        }
        self.set_anchor(target as usize, prefs);
        true
    }

    /// 跳到第 `index` 章。越界与「已经在这章」都返回 `false` 且不动任何东西 ——
    /// 目录里点当前章不该把读者已经滚到的位置甩回章首。
    pub fn goto(&mut self, index: usize, prefs: &Reader) -> bool {
        if index >= self.chapters.len() || index == self.chapter {
            return false;
        }
        self.set_anchor(index, prefs);
        true
    }

    /// 当前章的标题，没有章节表时是空串。
    pub fn chapter_title(&self) -> &str {
        self.chapters
            .get(self.chapter)
            .map(|chapter| chapter.title.as_str())
            .unwrap_or_default()
    }

    pub fn title(&self) -> &SharedString {
        &self.title
    }

    /// 章节数（定版式或空文档是 0）。
    pub fn chapter_count(&self) -> usize {
        self.chapters.len()
    }

    /// 当前章下标（锚章）。
    pub fn chapter_index(&self) -> usize {
        self.chapter
    }

    /// 第 `index` 章的标题；越界给空串。
    pub fn chapter_title_at(&self, index: usize) -> String {
        self.chapters
            .get(index)
            .map(|chapter| chapter.title.clone())
            .unwrap_or_default()
    }

    /// 「容器路径 → 章号」。目录条目靠它对上正文：`TocEntry::href` 与
    /// `Chapter::spine_href` 同形（都已百分号解码、去掉 fragment），所以是一次查表
    /// 而不是模糊匹配。
    ///
    /// 空 `spine_href` 不进表：单文件格式（TXT / MD / FB2）每一章都是空串，进了表
    /// 就等于「所有目录条目都指向第 0 章」—— 那是比跳错更糟的错，因为它看着是好的。
    /// 同一条 href 出现两次时留文档序在前的那一章。
    fn toc_hrefs(&self) -> HashMap<String, usize> {
        let mut map = HashMap::new();
        for (index, chapter) in self.chapters.iter().enumerate() {
            if chapter.spine_href.is_empty() {
                continue;
            }
            map.entry(chapter.spine_href.clone()).or_insert(index);
        }
        map
    }

    /// 目录那一屏要的行，带层级。**每次问都重算**：树只有几十到几百条，
    /// 而缓存它要处理的失效情形（换书、重排、导入后重新解析）比这点开销贵得多。
    ///
    /// 书自己没有目录时退回章节表：TXT 一本十万行没有 nav 是常态，
    /// 给读者一屏空白等于把「跳到某一章」这个功能拿走。
    pub fn toc_rows(&self) -> Vec<TocRow> {
        if self.toc.is_empty() {
            return (0..self.chapters.len())
                .map(|index| TocRow {
                    ix: index,
                    parent: None,
                    depth: 0,
                    title: self.chapters[index].title.clone(),
                    branch: false,
                    chapter: Some(index),
                    fragment: None,
                })
                .collect();
        }
        toc_tree::build(&self.toc, &self.toc_hrefs())
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn document_mut(&mut self) -> &mut Document {
        &mut self.document
    }
}

/// 把章文本缓存裁到「`current ± [`TEXT_WINDOW`]`」。
///
/// 独立的纯函数是为了测试：[`Book`] 要 `App` 才能造出来，窗口数学不必陪它。
/// 当前章按构造就在窗口里，当前章的正文永远不会被裁掉。
fn prune_texts(texts: &mut HashMap<usize, String>, current: usize) {
    let lo = current.saturating_sub(TEXT_WINDOW);
    let hi = current.saturating_add(TEXT_WINDOW);
    texts.retain(|index, _| lo <= *index && *index <= hi);
}

#[cfg(test)]
mod tests {
    use super::prune_texts;
    use std::collections::HashMap;

    fn cache(chapters: &[usize]) -> HashMap<usize, String> {
        chapters
            .iter()
            .map(|ix| (*ix, format!("第 {ix} 章")))
            .collect()
    }

    fn sorted_keys(texts: &HashMap<usize, String>) -> Vec<usize> {
        let mut keys: Vec<usize> = texts.keys().copied().collect();
        keys.sort_unstable();
        keys
    }

    /// 窗口的三个边：中间、贴着第 0 章的下边、隔着空洞跳过的章。
    /// 千章书通读一遍，手上最多只有 2×窗口 + 1 章的正文。
    #[test]
    fn texts_outside_the_window_around_the_current_chapter_are_evicted() {
        let mut texts = cache(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        prune_texts(&mut texts, 5);
        assert_eq!(
            sorted_keys(&texts),
            vec![2, 3, 4, 5, 6, 7, 8],
            "窗外整块让出去"
        );

        let mut texts = cache(&[0, 1, 2, 3]);
        prune_texts(&mut texts, 0);
        assert_eq!(
            sorted_keys(&texts),
            vec![0, 1, 2, 3],
            "第 0 章时下半窗为空，一个不多裁"
        );

        let mut texts = cache(&[0, 3, 5, 7, 9]);
        prune_texts(&mut texts, 7);
        assert_eq!(
            sorted_keys(&texts),
            vec![5, 7, 9],
            "窗口只看下标，不看连续性"
        );
    }
}
