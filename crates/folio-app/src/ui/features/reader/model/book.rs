//! 一本打开着的书：章节表、锚章、以及**锚章 ± 窗口**拼出来的一条全局坐标。
//!
//! 与 `folio_core::content::document::Document`（统一文档接口）的分工：那一边是「这本书里
//! 有什么」（章、目录、图片元数据、按 href 现取字节），这一边是「界面读到哪、
//! 排版成什么样」。一本书换章只是换这一层持有的排版状态，容器不重开。
//!
//! 连续滚动把锚章周围**一屏多**的章拼成一条全局 y 坐标（见 [`Book::window_range`]：
//! 至少锚章 ± [`SPAN_WINDOW`]，再按视口顶往前铺够一屏多；拼接公式只有
//! [`Self::placements`] 一处）：滚过章尾自然接下一章，锚章跟随视口顶迁移，
//! 窗外章节裁掉、邻章提前排版入库。分页 / 双页不拼坐标，仍然只用
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
use folio_core::content::format::css::RuleSet;
use folio_core::content::format::{TocEntry, dir_of, fragment_of, resolve_href};
use folio_core::layout::ImageProvider;
use folio_core::{Chapter, Locator, Reader};
use gpui_kit::{App, SharedString};

use crate::ui::features::reader::components::image_viewer::{ImageSlot, Place, file_name};
use crate::ui::features::reader::model::book_fonts::BookFonts;
use crate::ui::features::reader::model::chapter_layout::{ChapterSource, Document, author_rules};
use crate::ui::features::reader::model::toc_tree::{self, TocRow};
use crate::ui::foundation::images::{BookImages, ByteSource, ChapterImages, document_source};

/// 拼进连续坐标的邻章半窗宽：锚章上下**至少**预排这么多章。
///
/// 读者会狂飙滚动：窗口给到 ±2，配合后台单线程排版队列（毫秒级一章），正常速度
/// 下接缝永远提前排好；狂飙时也至少还有一章的余量，滚到未就绪的边界只会停一拍，
/// 不会白屏。这是条**下限**：真正的边界由 [`Book::window_range`] 按像素预算再往外铺，
/// 上界是单侧 [`SPAN_MAX_AHEAD`] 章 —— 常数量级，不随书的长度长。
const SPAN_WINDOW: usize = 2;

/// 拼接窗口**前向**的像素预算，按屏给：**视口底**再往下还要铺这么多，读者才滚得动、
/// 锚章才迁移。它必须不止一屏：迁移的判据是「视口顶进了下一章」，而视口顶最远只走得到
/// 链尾减一屏 —— 链尾不伸出下一章一屏，那个判据就永远够不着，读者钉在链尾那一屏上
/// （见 [`Book::window_range`]）。
const SPAN_AHEAD_SCREENS: f32 = 1.5;
/// 拼接窗口**后向**的像素预算，按屏给。视口顶往上铺够这么多，往回滚不至于两下就到链头。
const SPAN_BEHIND_SCREENS: f32 = 0.5;
/// 前向窗口的**章数**上限：像素预算之外再兜一道，防一屏能放下几十个一行章时铺太多。
const SPAN_MAX_AHEAD: usize = 16;

/// 拼接坐标里的一个邻章：哪一章 + 它自己的排版状态。
///
/// 没排完的章（`doc.height() == 0`）不参与坐标，`[`Book::placements`]` 会跳过它。
pub(in crate::ui::features::reader) struct Span {
    pub(in crate::ui::features::reader) index: usize,
    doc: Document,
}

/// 一章在全局坐标里的摆位（只读投影）：起点、高度，都是排版像素。
pub(in crate::ui::features::reader) struct Placement {
    pub(in crate::ui::features::reader) index: usize,
    pub(in crate::ui::features::reader) base_y: f32,
    pub(in crate::ui::features::reader) height: f32,
}

/// 正文里一条 `<a href>` 的去向（[`Book::resolve_link`] 的结果）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// 书内位置：某一章，加上可选的章内锚点（`#id`）。
    Internal {
        chapter: usize,
        fragment: Option<String>,
    },
    /// 书外地址（`http:` / `mailto:` / `tel:` 等）。一期只解出来，收不收由调用方定。
    External { url: String },
}

/// `before` 里 `:` 前若是一个 URL scheme 就返回它，否则 `None`。
///
/// 判定照 RFC 3986：首字符字母，其余是字母数字或 `+` `-` `.`。文件名里出现 `:`
/// 极少见，而真正要拦的是 `http://…` 这种 —— 不拦就会被当相对路径去解、解不出。
fn external_scheme(before: &str) -> Option<&str> {
    let (scheme, _) = before.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then_some(scheme)
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
    /// 邻章 Span（不含锚章），按章号升序。条数不是一个固定值：至少锚章 ±
    /// [`SPAN_WINDOW`]，再多铺到「前后各盖过一两屏」为止，见 [`Self::window_range`]。
    spans: Vec<Span>,
    /// 上一帧量到的视口高（逻辑像素）。拼接窗口按它折成「前后各铺几章」——
    /// 见 [`Self::window_range`]。默认给一屏常用值，第一帧之前也有个像样的窗口。
    window_px: f32,
    /// 书自己的样式表。`LayoutInputs` 要按它算一次哈希，而规则表用的是下面那一格。
    css: String,
    /// 作者层规则：**整本书解析一次**，每一章的 `Document` 分同一个 `Arc`。
    ///
    /// 为什么由 `Book` 拿着：从前是每建一章在 UI 线程重解析一遍整本 CSS（真机拆开
    /// 量到 1.5–1.9 ms / 章，跳一次章一次铺四章就是 7 ms）。见 `document::author_rules`。
    rules: Arc<RuleSet>,
    /// 整本书的内嵌字体（别名表 + 已经注册进设备的家族名）。各章共用一份，
    /// 注册进设备只有一次，所以由 `Book` 持有，而不是每章自己注册一遍。
    faces: Arc<BookFonts>,
    /// 打开着的这份文档。正文不在库里，只存位置（`ChapterLocator`），所以要留着它
    /// 才能按位置回容器取。
    ///
    /// 它还随每一个排版作业搬进后台线程（`ChapterSource::Fetch`）：取正文那一趟现在
    /// 在作业里做，而 `content::document::Document: Send + Sync` 正是那一趟能出渲染帧的
    /// 前提。从前这里只服务界面线程上那句现取（`chapter_text_at`），连带一份
    /// 「当前章 ± 3 章」的正文缓存 —— 那一格 2026-10-15 起没有存在理由了，
    /// 上游 crengine 连解压后的条目都不缓存（每次读重走一遍 inflate）。
    doc: Arc<dyn OpenedDocument>,
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
        // 整本书的作者层规则只解析这一次，之后每一章的 `Document` 分同一份 `Arc`。
        let rules = author_rules(&css);
        // 锚章的排版状态按**位置**建，正文由排版作业自己去取。图片索引这里先给空的：
        // 紧跟着的 `set_anchor` 会按本章目录重建它。
        let document = Document::fetching_with_rules(
            doc.clone(),
            chapter,
            &css,
            rules.clone(),
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
            // 第一帧 `ensure_window_spans` 就会用真实视口高盖掉它。
            window_px: 800.0,
            css,
            rules,
            faces: fonts,
            doc,
        };
        this.set_anchor(chapter, &prefs);
        this
    }

    /// 整本书的那一列图：各章的图片清单按**章序、章内文档序**接起来。
    ///
    /// 清单本身在库里（`chapters.images_json`，导入那一趟扫出来的，见
    /// `content::scan::ImageUse`）—— 排版只认识已经排好的那一章，而这一列要在读者
    /// 走到那儿之前就把「一共几张、这一张在第几章第几个字」报出来。
    ///
    /// 每一条目都过一遍 [`ChapterImages::resolve`]，与排版取那张图**同一句**解析，
    /// 所以「列里有这张」与「正文里画得出这张」不会是两本账。解不出来的（作者写了
    /// 一个图片表里没有的 `src`）当场丢掉：让读者滑到一张永远画不出来的黑纸上，
    /// 比那一列少一张糟得多。
    pub(in crate::ui::features::reader) fn build_image_sequence(&self) -> Arc<[ImageSlot]> {
        let mut out: Vec<ImageSlot> = Vec::new();
        for (chapter, ch) in self.chapters.iter().enumerate() {
            if ch.images.is_empty() {
                continue;
            }
            // 本章的视图：`src` 要按这一章在容器里的目录解，所以一章换一个。
            let view = ChapterImages::new(Arc::clone(&self.book_images), &ch.spine_href);
            for used in &ch.images {
                let Some(resolved) = view.resolve(&used.src) else {
                    tracing::trace!(
                        "图片列: 第 {chapter} 章的 {:?} 在图片表里解不出来，不进那一列",
                        used.src
                    );
                    continue;
                };
                let Some(request) = view.decode_request(&resolved.href) else {
                    continue;
                };
                let mime = view.media_type(&resolved.href).unwrap_or("").to_string();
                let index = out.len() + 1;
                out.push(ImageSlot {
                    href: resolved.href.clone(),
                    name: file_name(index, &resolved.href, &mime),
                    request,
                    natural: (resolved.width, resolved.height),
                    mime,
                    place: Place::Reflow {
                        chapter,
                        offset: used.offset,
                    },
                });
            }
        }
        tracing::debug!("图片列: 这本书有 {} 张画得出来的插图", out.len());
        out.into()
    }

    /// 取字节那条链子（分享 / 下载那一趟要在后台线程上拿它读原图）。
    pub(in crate::ui::features::reader) fn image_source(&self) -> ByteSource {
        self.book_images.source()
    }

    /// 拼接坐标里两章之间的缝。跟着字号走（2em）并给一条下限：它是「上一章结束、
    /// 下一章开始」的呼吸空隙，不是装饰条。
    pub(in crate::ui::features::reader) fn chapter_gap(prefs: &Reader) -> f32 {
        (prefs.font_size * 2.0).max(24.0)
    }

    /// 把锚章挪到 `index`。旧锚章降级为邻居（排版结果留着，`advance(±1)` 走到
    /// 相邻章时可以直接复用），新锚章优先取窗口里现成的 Span，窗外整块裁掉。
    ///
    /// 只动结构与缓存，**不算全局坐标也不补偿滚动** —— 那是渲染帧里
    /// `relayout_spine` / `migrate_spine` 的事；调用方（跳章、恢复）要么随后挂
    /// 恢复请求，要么自己带着快照去补。
    pub(in crate::ui::features::reader) fn set_anchor(&mut self, index: usize, prefs: &Reader) {
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
            let vacant = Document::new_with_rules(
                String::new(),
                &self.css,
                self.rules.clone(),
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
        // 先把新锚章的排版装回去，**再**裁窗外：`window_range` 要读锚章的高度，
        // 而那一刻它若还是空的（占位 Document），窗口会被按「一整屏」估出来、
        // 一刀裁短 —— 下一帧内容高缩水、滚动被夹回去，锚章又迁回原处，
        // 短章书就在两个锚章之间来回抖（`advance`/`migrate` 都走这条）。
        match reused {
            Some(span) => self.document = span.doc,
            None => {
                let source = self.chapter_source(index);
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
                self.document.set_chapter(source);
                self.document.set_images(images);
            }
        }
        self.prune_spans(Self::chapter_gap(prefs));
    }

    /// 把锚章附近的邻居补齐入库（0 高占位，排完自动进坐标链），窗外的整块让出去。
    ///
    /// 新章以 0 高占位进库、**不动已进链章的 base**，所以这一步本身不需要滚动补偿；
    /// 窗外那几章按算出来的同一个 `[lo, hi]` 一并让出去。渲染帧开头调它，排版作业
    /// 这就能排进后台队列 —— 接缝因此总是提前铺好。
    ///
    /// `viewport_height` 与 `viewport_top` 一起决定窗口铺多远（见 [`Self::window_range`]）：
    /// 短章要多铺几章才盖得过一屏，而读者停在长章中后段时，锚章自己已经吃掉了好几屏，
    /// 前方那几章照样得够 —— 两件事都要这两个数才答得出来。
    ///
    /// 这是**每帧按当帧读数**量窗口的那一处：读者停在哪里，窗口就该从哪里往前铺。
    /// 换锚章那一处（`set_anchor`）只做一次保守的窗外清理，判据见 [`Self::prune_spans`]。
    pub(in crate::ui::features::reader) fn ensure_window_spans(
        &mut self,
        prefs: &Reader,
        viewport_height: f32,
        viewport_top: f32,
    ) {
        self.window_px = viewport_height.max(1.0);
        let gap = Self::chapter_gap(prefs);
        let (lo, hi) = self.window_range(gap, viewport_top);
        for index in lo..=hi {
            if index == self.chapter || self.spans.iter().any(|span| span.index == index) {
                continue;
            }
            let span = self.make_span(index, prefs);
            self.spans.push(span);
        }
        self.spans.sort_by_key(|span| span.index);
        self.retain_window(lo, hi);
    }

    /// 拼接窗口的边界 `[lo, hi]`（含锚章）。两条规则一起用：**至少** `锚章 ± SPAN_WINDOW`
    /// （长章书的驻留上界还是它），再多铺到「视口顶往前盖过一屏多、往回盖过半屏」为止。
    ///
    /// 前向预算是**从视口顶算起**的，不是从锚章章首：读者停在长章中后段时，章首到视口
    /// 那段已经滚过去了，若还从章首起算，「一屏半」全花在了视口**上方**，前方就只剩
    /// [`SPAN_WINDOW`] 那两章的兜底。那两章若都是短页（献词页、整页图），加起来不够一屏，
    /// 于是 `content_height` 到它们末尾就没了：视口顶最远只走得到链尾减一屏，永远进不了
    /// 下一章 —— 锚章不迁移、窗口不往前长、`max_scroll` 也不再变大，读者就卡在那一屏上
    /// （实测：《未来简史》第 3 节那个目录章比手机视口略高，后面接着献词页与整页图，
    /// 两页连缝一共 442px，而视口有 750px —— 屏幕停在图与图说上，页脚念 100%）。
    ///
    /// 后向同一条道理：视口顶之上那几屏多半还在锚章肚子里，锚章自己就盖住了，
    /// 只有视口顶贴近锚章章首时才需要往上够一够。
    ///
    /// 没排完的章按一屏估：宁可多铺一章，也不要少铺。
    fn window_range(&self, gap: f32, viewport_top: f32) -> (usize, usize) {
        let last = self.chapters.len().saturating_sub(1);
        let height = |index: usize| -> f32 {
            self.span_doc(index)
                .map(|doc| doc.height())
                .filter(|h| *h > 0.0)
                .unwrap_or(self.window_px)
        };
        // 视口顶在锚章里的位置。锚章还没排完（base 问不出来）时按章首算：两条夹取
        // 都往「少要一点」那一侧靠，多铺的那几章下一帧自己会补齐。
        let top = self
            .base_of(self.chapter, gap)
            .map(|base| (viewport_top - base).clamp(0.0, height(self.chapter)))
            .unwrap_or(0.0);
        // 前向：一屏（视口自己）+ `SPAN_AHEAD_SCREENS` 屏的余量，都从视口顶往下量。
        let ahead = top + (self.window_px * (1.0 + SPAN_AHEAD_SCREENS)).max(1.0);
        let behind = (self.window_px * SPAN_BEHIND_SCREENS).max(1.0);

        let mut hi = self.chapter;
        let mut covered = height(self.chapter);
        while hi < last && (hi - self.chapter) < SPAN_MAX_AHEAD && covered < ahead {
            hi += 1;
            covered += height(hi) + gap;
        }
        hi = hi.max((self.chapter + SPAN_WINDOW).min(last));

        // 后向：视口顶之上已经铺了 `above` 像素（锚章露在视口上方的那一截先算进去）。
        let mut lo = self.chapter;
        let mut above = top;
        while lo > 0 && (self.chapter - lo) < SPAN_MAX_AHEAD && above < behind {
            lo -= 1;
            above += height(lo) + gap;
        }
        lo = lo.min(self.chapter.saturating_sub(SPAN_WINDOW));

        (lo, hi)
    }

    /// 只留 `[lo, hi]` 里的邻居（锚章自己不会住在 `spans` 里）。
    fn retain_window(&mut self, lo: usize, hi: usize) {
        self.spans
            .retain(|span| span.index != self.chapter && lo <= span.index && span.index <= hi);
    }

    /// 窗口之外的邻居整块让出去（换锚章时随手做一次）。
    ///
    /// `viewport_top` 这里传 0：换锚章那一刻手上没有当帧的视口顶 —— 迁移发生在渲染帧
    /// 中段、跳章更要等下一帧的恢复请求落地才准。按「视口顶就在锚章章首」算，落在
    /// **保守**的那一侧（前向预算从章首起算满一屏多，只会多留、不会少留），而窗外那几章
    /// 也真的当场就让出去了，不必多留一帧。每帧开头的 [`Self::ensure_window_spans`]
    /// 随后会用当帧读数再量一遍，那才是说了算的那一次。
    fn prune_spans(&mut self, gap: f32) {
        let (lo, hi) = self.window_range(gap, 0.0);
        self.retain_window(lo, hi);
    }

    fn take_span(&mut self, index: usize) -> Option<Span> {
        let position = self.spans.iter().position(|span| span.index == index)?;
        Some(self.spans.remove(position))
    }

    fn make_span(&self, index: usize, prefs: &Reader) -> Span {
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
        // 正文按位置交给排版作业去取：这一趟连一次解压都不做，更不克隆一整章。
        let doc = Document::fetching_with_rules(
            self.doc.clone(),
            index,
            &self.css,
            self.rules.clone(),
            prefs.clone(),
            images,
            self.faces.clone(),
        );
        Span { index, doc }
    }

    /// 第 `index` 章正文的**来路**：按位置回容器取，而那一趟取在排版作业里做。
    ///
    /// 从前这一格是 `chapter_text_at` —— 当场 `doc.chapter_source(index)` 取回一整章
    /// （一次解压 + 一次解码，实测 7–9.5 ms），在 UI 线程的渲染帧里付，还要再存进
    /// 一份「当前章 ± 3」的正文缓存。现在 `Book` 只递「去哪儿取」，取正文与
    /// 取不到的降级都在 `document::typeset_job` 里。
    fn chapter_source(&self, index: usize) -> Arc<ChapterSource> {
        ChapterSource::fetch(self.doc.clone(), index)
    }

    /// 参与拼接的章，按章号升序摆上全局坐标。**这是拼接坐标的唯一公式**：
    /// 基线就地累计，所有读数（`chapter_at_y` / `base_of` / `content_height`）
    /// 与补偿全都走它。
    ///
    /// 只有「有排版结果」的章进链（`height() > 0`）：没排完的邻居高度记 0，
    /// 排完的那一刻内容从边界长出来 —— 滚到界先停一拍，比估一个错误高度再跳
    /// 回去体面。`set_prefs` 作废排版时行盒仍留着（上一份结果），链不会塌，
    /// 滚动位置在新结果回来前保持原值。
    pub(in crate::ui::features::reader) fn placements(&self, gap: f32) -> Vec<Placement> {
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
    pub(in crate::ui::features::reader) fn chapter_at_y(&self, y: f32, gap: f32) -> Option<usize> {
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
    pub(in crate::ui::features::reader) fn base_of(&self, index: usize, gap: f32) -> Option<f32> {
        self.placements(gap)
            .into_iter()
            .find(|placement| placement.index == index)
            .map(|placement| placement.base_y)
    }

    /// 全局内容总高：最后一条链成员的末端。谁都没排完就是 0。
    pub(in crate::ui::features::reader) fn content_height(&self, gap: f32) -> f32 {
        self.placements(gap)
            .last()
            .map(|placement| placement.base_y + placement.height)
            .unwrap_or(0.0)
    }

    /// 第 `index` 章的排版状态（锚章或窗口内的邻居）。
    pub(in crate::ui::features::reader) fn span_doc(&self, index: usize) -> Option<&Document> {
        if index == self.chapter {
            return Some(&self.document);
        }
        self.spans
            .iter()
            .find(|span| span.index == index)
            .map(|span| &span.doc)
    }

    /// 「第 `offset` 个字（`chapter` 章内坐标）」的全局行顶。恢复落点用。
    pub(in crate::ui::features::reader) fn global_y_of_offset(
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
    pub(in crate::ui::features::reader) fn spine_snapshot(
        &self,
        scroll_y: f32,
        gap: f32,
    ) -> Option<(usize, f32)> {
        let index = self.chapter_at_y(scroll_y, gap)?;
        let base = self.base_of(index, gap)?;
        Some((index, base))
    }

    /// 按快照补偿 `scroll_y`：那一章的 base 动了多少，滚动位置就补多少 ——
    /// 「视口顶那一行字」在屏幕上纹丝不动。章被裁出链（不可能发生在快照与
    /// 补偿之间）或从未进链时不动。
    pub(in crate::ui::features::reader) fn compensate_spine(
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
    pub(in crate::ui::features::reader) fn migrate_spine(
        &mut self,
        scroll_y: &mut f32,
        gap: f32,
        prefs: &Reader,
    ) {
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

    /// 拼进坐标窗口的章（含锚章）。测试读：窗口**至少**钉在锚章 ± [`SPAN_WINDOW`]，
    /// 短章处会铺得更远（见 [`Self::window_range`]）。
    #[cfg(test)]
    pub(in crate::ui::features::reader) fn span_indices(&self) -> Vec<usize> {
        let mut out: Vec<usize> = self.spans.iter().map(|span| span.index).collect();
        out.push(self.chapter);
        out.sort_unstable();
        out
    }

    /// 全部排版状态（锚章优先，邻居随后）。渲染帧逐个收排版结果用：
    /// 锚章先提交，恢复落点才不用多等一帧。
    pub(in crate::ui::features::reader) fn docs_mut(
        &mut self,
    ) -> impl Iterator<Item = &mut Document> {
        std::iter::once(&mut self.document).chain(self.spans.iter_mut().map(|span| &mut span.doc))
    }

    /// 读者偏好变了：所有进窗的章一起作废重排。
    pub(in crate::ui::features::reader) fn set_prefs_on_all(&mut self, prefs: Reader) {
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

    /// 正文里一条 `<a href>` 的去向。
    ///
    /// 只分「书内」与「书外」两类。书内落成 `(章号, 可选锚点)` —— 与目录条目走的是
    /// 同一套坐标（`goto_entry` / `pending_anchor`），所以正文链接与目录链接跳得一样。
    pub fn resolve_link(&self, chapter: usize, href: &str) -> Option<Link> {
        let href = href.trim();
        if href.is_empty() {
            return None;
        }
        let fragment = fragment_of(href);
        // `#` 之前那一截：空 = 纯章内锚点（`#m1`），否则是路径或外部地址。
        // 纯锚点是**先**判的：`resolve_href` 把 `#…` 整条剥掉之后会拿 base 目录
        // 当结果，对纯锚点给出一个指不到任何章的假路径。
        let before = href.split('#').next().unwrap_or("").trim();
        if before.is_empty() {
            return Some(Link::Internal { chapter, fragment });
        }
        if external_scheme(before).is_some() {
            return Some(Link::External {
                url: href.to_string(),
            });
        }
        // 相对路径按**这一章**所在目录解析（书内目录页写的是 `part0004.xhtml`，
        // 不是容器全路径），再回章节表查目标章。
        let base = self
            .chapters
            .get(chapter)
            .map(|c| dir_of(&c.spine_href))
            .unwrap_or_default();
        let path = resolve_href(&base, href);
        self.toc_hrefs()
            .get(&path)
            .copied()
            .map(|target| Link::Internal {
                chapter: target,
                fragment,
            })
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

    /// 整本的翻页方向：`true` = 从右往左翻。
    ///
    /// 只有 EPUB 的 `<spine page-progression-direction="rtl">` 会给 `true`；其余格式 provider
    /// 那一格恒为 `false`（见 `folio_core::content::format::FlowDoc::progression_rtl`）。
    /// 问的是**手**的事 —— 点屏幕哪一侧算下一页、进度条两端那两颗箭头按下去往哪走；
    /// 正文往哪边流是另一件事（CSS 的 `direction`），两格各读各的，不许互相推。
    pub fn progression_rtl(&self) -> bool {
        self.doc.flow().is_some_and(|flow| flow.progression_rtl)
    }

    pub fn document_mut(&mut self) -> &mut Document {
        &mut self.document
    }

    /// 打开着的这份文档。正文不在库里，只存位置，所以「把整本书读一遍」的活儿
    /// （全文检索那一趟）要拿着它去后台线程 —— `content::document::Document: Send + Sync`
    /// 正是那一趟能出界面线程的前提，与排版作业带的同一份、同一条理由。
    pub(in crate::ui::features::reader) fn opened(&self) -> Arc<dyn OpenedDocument> {
        self.doc.clone()
    }

    /// 分页/双页的邻章预排版入口：把第 `index` 章作为 `Span` 建出来（不动锚章）。
    ///
    /// 连续滚动走 [`Self::ensure_window_spans`]，那里按像素窗口一次铺一串；分页模式不需要
    /// 一条拼接链，只需要「下一章第一页」与「上一章最后一页」能在一帧里被画出来 ——
    /// 跨章滑动要两张纸同时在屏上，目标章必须先排版、先切页。
    pub(in crate::ui::features::reader) fn ensure_paged_span(
        &mut self,
        index: usize,
        prefs: &Reader,
    ) {
        if index >= self.chapters.len() || index == self.chapter {
            return;
        }
        if self.spans.iter().any(|span| span.index == index) {
            return;
        }
        let span = self.make_span(index, prefs);
        self.spans.push(span);
        self.spans.sort_by_key(|span| span.index);
    }

    /// 第 `index` 章的排版状态（当前章取 `document`，邻章取 `spans`）。
    pub(in crate::ui::features::reader) fn span_doc_mut(
        &mut self,
        index: usize,
    ) -> Option<&mut Document> {
        if index == self.chapter {
            return Some(&mut self.document);
        }
        self.spans
            .iter_mut()
            .find(|span| span.index == index)
            .map(|span| &mut span.doc)
    }
}
