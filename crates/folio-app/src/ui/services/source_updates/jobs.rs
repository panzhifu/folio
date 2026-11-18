//! 有界进度桥：中间读数可合并，终态必须送达；取消后等待后台到达检查点。

use std::io;

use folio_core::{TaskError, TaskEvent, TaskHandle};
use gpui_kit::Context;

use super::SourceUpdates;

enum Event<R> {
    Progress(Option<u8>),
    Finished(Result<R, TaskError>),
    Cancelled,
}

pub(super) fn follow<R: Send + 'static>(
    service: &mut SourceUpdates,
    handle: TaskHandle<R>,
    book: Option<i64>,
    finish: impl FnOnce(&mut SourceUpdates, Result<R, TaskError>, &mut Context<SourceUpdates>) + 'static,
    cx: &mut Context<SourceUpdates>,
) {
    service.busy = true;
    service.operation_book = book;
    service.percent = None;
    service.last_message = None;
    service.cancel = Some(handle.cancel.clone());
    let (tx, rx) = async_channel::bounded(8);
    let failed = tx.clone();
    let spawn = std::thread::Builder::new()
        .name("folio-source-events".into())
        .spawn(move || {
            let mut previous = None;
            for event in handle.events {
                let event = match event {
                    TaskEvent::Bytes(bytes) => {
                        let percent = bytes.total.filter(|total| *total > 0).map(|total| {
                            ((bytes.done as u128 * 100 / total as u128).min(100)) as u8
                        });
                        if previous == Some(percent) {
                            continue;
                        }
                        previous = Some(percent);
                        let _ = tx.try_send(Event::Progress(percent));
                        continue;
                    }
                    TaskEvent::Finished(result) => Event::Finished(result),
                    TaskEvent::Cancelled => Event::Cancelled,
                    TaskEvent::Started | TaskEvent::Progress(_) => continue,
                };
                let _ = tx.send_blocking(event);
                break;
            }
        });
    if let Err(error) = spawn {
        if let Some(cancel) = &service.cancel {
            cancel.trigger();
        }
        let _ = failed.send_blocking(Event::Finished(Err(error.into())));
    }
    drop(failed);
    service.pump = Some(cx.spawn(async move |this, cx| {
        loop {
            let event = rx.recv().await;
            if let Ok(Event::Progress(percent)) = event {
                if this
                    .update(cx, |this, cx| {
                        if this.percent != percent {
                            this.percent = percent;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
                continue;
            }
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.percent = None;
                this.cancel = None;
                let result = match event {
                    Ok(Event::Finished(result)) => result,
                    _ => {
                        this.last_message = Some("source_update.cancelled".into());
                        // 取消可能晚于原子发布，重读数据库以显示已完成的真实结果。
                        this.epoch = this.epoch.wrapping_add(1);
                        Err(io::Error::other("source_update.cancelled").into())
                    }
                };
                finish(this, result, cx);
                this.operation_book = None;
                cx.notify();
            });
            break;
        }
    }));
    cx.notify();
}
