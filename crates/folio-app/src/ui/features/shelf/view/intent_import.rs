//! intent 送进来的文件入口：把 `services::intent_import` 队列里的 `content://`
//! 交给 [`DocumentsSource`] 流式捕获进暂存区（不经 cache 预拷贝），再交导入管线。
//! 挂在这一层是因为导入卡片与进度都住在架面；渲染帧里调（[`run`](self::run)），
//! 空队列零开销。

use gpui_kit::{Context, WeakEntity};

use super::ShelfView;
pub(in crate::ui::features::shelf) fn run(cx: &mut Context<ShelfView>) {
    let uris = crate::ui::services::intent_import::drain();
    if uris.is_empty() {
        return;
    }
    cx.spawn(async move |this: WeakEntity<ShelfView>, cx| {
        // stat（display_name / 大小）是 worker-only 的 JNI：整体丢到后台。
        let docs = cx
            .background_executor()
            .spawn(async move { crate::ui::platform::intent_documents(&uris) })
            .await;
        if docs.is_empty() {
            return;
        }
        let _ = this.update(cx, |this, cx| {
            this.dispatch_import(
                Box::new(folio_core::ingest::import::DocumentsSource::new(
                    docs,
                    crate::ui::platform::source_access(),
                )),
                cx,
            )
        });
    })
    .detach();
}
