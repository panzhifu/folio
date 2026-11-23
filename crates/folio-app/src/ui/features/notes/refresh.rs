//! 这一屏的取数：一次快照（批注 + 书签），加逐本封面字节。
//!
//! 两件事分开落地：清单（含每本封面的**编码字节**）在后台线程上读，回来才投影、
//! 解码、换上新清单。清单按「有笔记的书」来，一本来一封封面；冷态下每次首开是
//! 毫秒级，而这一趟若压在界面线程上，切进这一屏的那一帧就得等它读完。
//!
//! 取数只在 [`NotesView::refresh`] 里发生（外壳在切页/关书时叫，本屏删改后自己叫）；
//! 渲染帧只读 [`NotesView::data`] 与 [`NotesView::covers`]。

use folio_core::store::notes;
use gpui_kit::{AppContext as _, Context};

use super::{NotesView, projection};
use crate::ui::foundation::images::cover_image;
use crate::ui::services::app_store;

impl NotesView {
    /// 重取一次快照。切进这一屏（外壳叫）、关掉一本书（外壳叫）、本屏删改之后
    /// （自己叫）都走这里。
    ///
    /// 取数（含逐本封面字节的读盘）在后台线程上跑；回来才投影、解码、换上新清单。
    /// 清单按「有笔记的书」来，一本来一封封面 —— 冷态下每次首开都是毫秒级（实测见
    /// `doc/储存.md`），把这一趟放在界面线程上，切屏那一帧就得等它读完。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(store) = app_store::handle(cx) else {
            self.store_missing = true;
            self.data = None;
            cx.notify();
            return;
        };
        self.load_seq += 1;
        let seq = self.load_seq;
        let work = cx.background_spawn(async move { notes::load_groups(&store) });
        cx.spawn(async move |this, cx| {
            let loaded = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.load_seq != seq {
                    return;
                }
                match loaded {
                    Ok(mut groups) => {
                        // 投影要查库与本地化，留在这一侧；它不读文件。
                        if let Some(store) = app_store::handle(cx) {
                            projection::apply(&store, &mut groups, cx);
                        }
                        this.store_missing = false;
                        this.covers = groups
                            .iter()
                            .filter_map(|group| {
                                let bytes = group.cover.as_ref()?;
                                let image = cover_image(bytes)?;
                                Some((group.book_id, image))
                            })
                            .collect();
                        this.data = Some(groups);
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "笔记: 读批注与书签失败");
                        this.store_missing = true;
                        this.data = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
