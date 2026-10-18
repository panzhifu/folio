//! 正文里的链接：点中 `<a href>` 就按它去的地方跳。
//!
//! 命中口径与选中共用同一套（`offset_at` → 章内字符偏移 → `ChapterDoc::link_href_at`），
//! 所以「点在哪条链接上」与「这个字属于哪段文字」永远不会分叉。去向的解析
//! （相对路径 → 容器路径 → 章号 + 锚点）在 [`crate::ui::features::reader::model::book::Book::resolve_link`]，
//! 这一层只负责「屏幕点 → 链接 → 跳」这三步。
//!
//! 跳转**复用目录那一条路**（[`ReaderSession::goto_entry`]）：正文链接与目录条目
//! 落到同一个 `(章号, 锚点)` 坐标上，锚点同样由排版做完那一帧的 `land_at_anchor` 落地。

use gpui_kit::{App, Context, Pixels, Point, Window};

use crate::ui::features::reader::model::book::Link;
use crate::ui::features::reader::model::session::ReaderSession;

impl ReaderSession {
    /// 正文上的一点若落在链接里就跟着跳过去，返回这一击**有没有被链接吃掉**。
    ///
    /// 顺序上它排在分区判定**之前**（见 `page.rs::on_body_tap`）：点中一条链接是比
    /// 「点中间开浮层 / 点两侧翻页」更明确的意图，读者不会想「点了一条目录项结果翻了一页」。
    pub fn open_link_at(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        at: Point<Pixels>,
    ) -> bool {
        let Some((chapter, href)) = self.link_href_at(window, cx, at) else {
            return false;
        };
        let Some(link) = self
            .book()
            .and_then(|book| book.resolve_link(chapter, &href))
        else {
            // 解不出来（指向的资源不在 spine 里）＝这一击当没点中链接，落回分区逻辑。
            return false;
        };
        match link {
            Link::Internal { chapter, fragment } => {
                self.goto_entry(chapter, fragment, cx);
                true
            }
            Link::External { url } => {
                // 一期不开外部链接：记一条日志，明说「认出来了但没接」，而不是静默吞掉
                // —— 静默会把一次点击变成「点了没反应」，最难查。
                tracing::info!(target: "folio::reader", url = %url, "正文链接指向书外，一期不打开");
                true
            }
        }
    }

    /// 屏幕点 → 命中的那个链接的 `(章号, href)`（不在链接里就是 `None`）。
    fn link_href_at(
        &self,
        window: &Window,
        cx: &App,
        at: Point<Pixels>,
    ) -> Option<(usize, String)> {
        let (chapter, offset) = self.offset_at(window, cx, at)?;
        let href = self.book()?.span_doc(chapter)?.link_href_at(offset)?;
        Some((chapter, href))
    }
}
