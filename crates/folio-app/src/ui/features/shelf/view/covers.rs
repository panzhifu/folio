//! 架面上的封面：**只解真正上屏的那几格**。
//!
//! 架面是虚拟列表，一屏十几本；[`ShelfView::reload`] 只建卡、不问封面字节，等到某一排
//! 真被画出来，才由 [`ShelfView::cover_visible`] 派一次后台读盘 + 解码。冷启动时每个文件
//! 第一次被打开都要过一遍杀软（实测 1000 个文件只开不读就要 4.5 s），所以「少开文件」
//! 比「把文件变小」更值钱：先读缩略图那一份，原图只在缩略图缺席时兜底。
//!
//! 组卡（[`ShelfView::tile_cards`]）仍同步解：一组一张，数量由组数决定，不由书数决定。

use gpui_kit::{AppContext as _, Context};

use super::ShelfView;
use crate::ui::features::shelf::components::book_card::{GroupCard, TILE_SLOTS};

impl ShelfView {
    /// 架面那几张组卡：每个组一张，卡上是这个组最后丢进去的那几本的封面。
    ///
    /// 封面走的是 [`Self::resolve_cover`] 那一条，与书卡同一份缓存：组卡与下面那片书
    /// 十有八九是同一批封面，解两遍是白解。
    ///
    /// 查不动就是一片空的：架面少一排卡看着像「还没建组」，比摆几张假卡或整屏报错好。
    pub(super) fn tile_cards(&mut self) -> Vec<GroupCard> {
        let Some(store) = &self.store else {
            return Vec::new();
        };
        let Ok(tiles) = store.collection_tiles(TILE_SLOTS) else {
            tracing::error!("ShelfView: 查组卡失败，架面那一排卡是空的");
            return Vec::new();
        };
        tiles
            .into_iter()
            .map(|(collection, items)| {
                let covers = items.iter().map(|item| self.resolve_cover(item)).collect();
                GroupCard {
                    id: collection.id,
                    group: format!("shelf-tile-{}", collection.id).into(),
                    face_id: format!("shelf-tile-face-{}", collection.id).into(),
                    name: collection.name.into(),
                    count: collection.book_count,
                    // 这一组的「最近被翻开」取它自己那条序的头一本：`collection_tiles` 里
                    // 那一串走的就是 `SHELF_ORDER`（最近在先），头一本的 `last_opened_at`
                    // 就是库给组卡排序用的那个 `MAX(last_opened_at)` —— 同一把尺，不是近似。
                    opened_at: items.first().and_then(|item| item.last_opened_at),
                    covers,
                }
            })
            .collect()
    }

    /// 可见行的一格封面：命中缓存就直接贴回去，没解过就派一个后台作业，这一帧先画占位。
    ///
    /// 只在 `uniform_list` 的 item 闭包里调 —— 那里拿到的正是**这一帧真要上屏**的
    /// 那几本。铺满整架去解是一千次文件打开，而冷态每次首开都在毫秒级（本机实测：
    /// 1000 个 30KB 文件的首读 4.1 s，其中几乎全是「第一次打开」那一笔）。
    ///
    /// 读文件与解码都不在界面线程上。缩略图是首选来源（几 KB），缺席才回库取原图
    /// 兜底 —— 老库、`.thumbs` 被清掉这两种情况才有这一趟。回来时若水墨档或补齐
    /// epoch 已经变了，这一份按旧档解的丢掉，下一帧按新档重解。
    pub(super) fn cover_visible(&mut self, at: usize, cx: &mut Context<Self>) {
        self.refresh_cover_generation();
        let id = self.items[at].id;
        if let Some(cached) = self.covers.get(&id) {
            self.items[at].cover = cached.clone();
            return;
        }
        if self.covers_pending.contains(&id) {
            return;
        }
        let thumb = self.items[at].thumb.clone();
        let store = self.store.clone();
        if thumb.is_none() && store.is_none() {
            // 没有封面描述、手上也没有库：这一格永远不会有图。记住失败，别再问。
            self.covers.insert(id, None);
            return;
        }
        self.covers_pending.insert(id);
        let ink = crate::ui::foundation::images::ink_images();
        let epoch = folio_core::cover_epoch();
        let work = cx.background_spawn(async move {
            if let Some(path) = thumb.as_deref()
                && let Ok(bytes) = std::fs::read(path)
            {
                return crate::ui::foundation::images::cover_image(&bytes);
            }
            let store = store?;
            let bytes = store.book_cover(id).ok().flatten()?;
            crate::ui::foundation::images::cover_image(&bytes)
        });
        cx.spawn(async move |this, cx| {
            let decoded = work.await;
            let _ = this.update(cx, |this, cx| {
                this.covers_pending.remove(&id);
                // 作业在跑的时候读者翻了水墨档、或补齐又把封面写回了一遍：这一份
                // 是按旧档解的，写进缓存会被下一帧原样贴出来。
                if crate::ui::foundation::images::ink_images() == ink
                    && folio_core::cover_epoch() == epoch
                {
                    this.covers.insert(id, decoded);
                }
                cx.notify();
            });
        })
        .detach();
    }
}
