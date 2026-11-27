//! intent 送进来的文件入口：把 `services::intent_import` 队列里的 `content://` 落成
//! 临时文件、交导入管线。挂在这一层是因为导入卡片与进度都住在架面；渲染帧里调
//! （[`run`](self::run)），空队列零开销。临时副本读完即弃，与选择器副本同一条口径。

use gpui_kit::{Context, WeakEntity};

use super::ShelfView;
pub(in crate::ui::features::shelf) fn run(cx: &mut Context<ShelfView>) {
    let uris = crate::ui::services::intent_import::drain();
    if uris.is_empty() {
        return;
    }
    cx.spawn(async move |this: WeakEntity<ShelfView>, cx| {
        let paths = cx
            .background_executor()
            .spawn(async move {
                uris.iter()
                    .filter_map(|uri| crate::ui::platform::import_uri_to_temp(uri).ok().flatten())
                    .collect::<Vec<_>>()
            })
            .await;
        if paths.is_empty() {
            return;
        }
        let _ = this.update(cx, |this, cx| {
            this.dispatch_import(
                Box::new(folio_core::ingest::import::PathsSource::picked_temporary(
                    paths,
                )),
                cx,
            )
        });
    })
    .detach();
}
