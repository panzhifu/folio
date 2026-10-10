//! 一本打开着的书：章节表、当前章、以及**当前章的排版状态**。
//!
//! 与 `folio_core::document::Document`（统一文档接口）的分工：那一边是「这本书里
//! 有什么」（章、目录、图片元数据、按 href 现取字节），这一边是「界面读第几章、
//! 排版成什么样」。一本书换章只是换这一层持有的 [`Document`]，容器不重开。
//!
//! 章节的源文本**已经是引擎就绪的 XHTML**（契约见 `format::Chapter`），provider
//! 在解析时就把 Markdown / TXT 包好了，所以这里没有「按格式先包一层」的分支，
//! `Format` 也不参与这条链（见 `plans/README.md` 的架构决策 ④）。

use std::sync::Arc;

use folio_core::document::Document as OpenedDocument;
use folio_core::{Chapter, Locator, Reader};
use gpui_kit::{App, SharedString};

use super::book_fonts::BookFonts;
use super::document::Document;
use crate::ui::shared::images::{BookImages, ChapterImages, document_source};

/// 一台设备上打开着的一本书：章节表、当前章、以及当前章的排版状态。
pub struct Book {
    title: SharedString,
    chapters: Vec<Chapter>,
    chapter: usize,
    /// 整本书的图片索引，换章时按新章的目录重建视图。字节按需从文档现取。
    book_images: Arc<BookImages>,
    /// 当前章的排版状态。
    document: Document,
}

impl Book {
    /// 从一份已打开的文档建出一本书，定位到 `locator` 那一章。
    ///
    /// `cx` 用于注册内嵌字体（进程级文本系统），所以这一步要有 `App`。
    pub fn new(doc: Arc<dyn OpenedDocument>, prefs: Reader, locator: Locator, cx: &App) -> Self {
        // 章节表与 CSS 只对可重排文档有意义。定版式（CBZ）目前没有正文排版链，
        // 落到这里就是「没有章节」—— 界面照起，只是这一屏空着，不 panic。
        let (chapters, css) = match doc.flow() {
            Some(flow) => (flow.chapters.clone(), flow.css.clone()),
            None => (Vec::new(), String::new()),
        };
        let chapter = (locator.chapter as usize).min(chapters.len().saturating_sub(1));

        // 图片索引与内嵌字体别名表都从这份文档建一次；字节按需现取（`ByteSource` 持一份
        // `Arc<dyn Document>`，所以 `doc` 不必再存进 `Book`）。
        let book_images = Arc::new(BookImages::new(doc.images(), document_source(doc.clone())));
        let fonts = BookFonts::for_document(doc.as_ref(), &css, cx);
        let document = Document::new(
            String::new(),
            &css,
            prefs.clone(),
            Arc::new(ChapterImages::empty()),
            fonts,
        );
        let mut this = Self {
            title: doc.meta().title.clone().into(),
            chapters,
            chapter,
            book_images,
            document,
        };
        this.show_chapter(&prefs);
        this
    }

    /// 把当前章的源文本与图片索引灌进排版状态。
    ///
    /// 图片索引必须跟着换章重建：`<img src>` 是相对本章目录的，
    /// 上一章的基准目录解不出下一章的路径。
    fn show_chapter(&mut self, prefs: &Reader) {
        let Some(chapter) = self.chapters.get(self.chapter) else {
            return;
        };
        // `Chapter::source` 就是引擎就绪的 XHTML，源文本包装已在 provider 里做完。
        let xhtml = chapter.source.clone();
        let images = Arc::new(ChapterImages::new(
            self.book_images.clone(),
            &chapter.spine_href,
        ));
        self.document.set_prefs(prefs.clone());
        self.document.set_chapter(xhtml);
        self.document.set_images(images);
    }

    /// 往某个方向换一章。真的换了返回 `true`，到头返回 `false`（按钮据此置灰）。
    pub fn advance(&mut self, delta: isize, prefs: &Reader) -> bool {
        let target = self.chapter as isize + delta;
        if target < 0 || target as usize >= self.chapters.len() {
            return false;
        }
        self.chapter = target as usize;
        self.show_chapter(prefs);
        true
    }

    /// 跳到第 `index` 章。越界与「已经在这章」都返回 `false` 且不动任何东西 ——
    /// 目录里点当前章不该把读者已经滚到的位置甩回章首。
    pub fn goto(&mut self, index: usize, prefs: &Reader) -> bool {
        if index >= self.chapters.len() || index == self.chapter {
            return false;
        }
        self.chapter = index;
        self.show_chapter(prefs);
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

    /// 当前章下标。
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

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn document_mut(&mut self) -> &mut Document {
        &mut self.document
    }
}
