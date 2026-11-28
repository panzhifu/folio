//! 阅读会话的缓存账与回收。注册字体与解码在途按估算口径入账；当前文档/渲染器内部
//! 占用/allocator 不冒充已测值。

use std::time::{Duration, Instant};

use folio_core::{EvictStep, MemoryBudget, Pressure};
use gpui_kit::{App, Context};

use crate::ui::features::reader::model::session::{ReaderSession, Source};

#[derive(Clone, Copy, Debug, Default)]
pub(in crate::ui::features::reader) struct CacheUsage {
    /// 像素缓冲实际字节数；GPU、副本与 allocator 不含在此值内。
    pub cpu_pixels: usize,
    /// 已上传像素的字节估算，不含纹理对齐和页面捕获。
    pub gpu_pixels_estimate: usize,
    pub reusable_layout_estimate: usize,
    pub font_cache_estimate: usize,
    /// 在途解码任务数。
    pub decoding: usize,
    /// 在途解码字节的估算：请求自估的瓦片字节（解完落地按真像素数换账）。
    pub decoding_bytes_estimate: usize,
    /// 随书内嵌字体注册进设备的驻留字节 + 用户字体按字节注册的那一支。
    /// **不进** [`total_estimate`](Self::total_estimate)：字体注册不可回收，
    /// 计入压力总数只会让回收反复空跑；这里只报数。
    pub registered_fonts_estimate: u64,
}

impl CacheUsage {
    fn total_estimate(self) -> u64 {
        [
            self.cpu_pixels,
            self.gpu_pixels_estimate,
            self.reusable_layout_estimate,
            self.font_cache_estimate,
        ]
        .into_iter()
        .fold(0u64, |total, value| total.saturating_add(value as u64))
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(in crate::ui::features::reader) struct CachePeaks {
    pub cpu_pixels: usize,
    pub total_estimate: u64,
    pub decoding: usize,
    pub decoding_bytes: usize,
}

impl CachePeaks {
    fn observe(&mut self, usage: CacheUsage) {
        self.cpu_pixels = self.cpu_pixels.max(usage.cpu_pixels);
        self.total_estimate = self.total_estimate.max(usage.total_estimate());
        self.decoding = self.decoding.max(usage.decoding);
        self.decoding_bytes = self.decoding_bytes.max(usage.decoding_bytes_estimate);
    }
}

pub(in crate::ui::features::reader) struct CacheControl {
    budget: MemoryBudget,
    checked: Option<Instant>,
    prefetch_after: Option<Instant>,
    physical_bytes: Option<u64>,
    peaks: CachePeaks,
    reported: Option<Instant>,
}

impl Default for CacheControl {
    fn default() -> Self {
        let physical_bytes = crate::ui::host::physical_memory_bytes();
        Self {
            budget: MemoryBudget::for_device(physical_bytes),
            checked: None,
            prefetch_after: None,
            physical_bytes,
            peaks: CachePeaks::default(),
            reported: None,
        }
    }
}

impl ReaderSession {
    pub(in crate::ui::features::reader) fn cache_usage(&self, cx: &App) -> CacheUsage {
        let (cpu_pixels, gpu_pixels_estimate, decoding, decoding_bytes_estimate) =
            self.atlas.read(cx).memory_usage();
        let reusable_layout_estimate = match &self.source {
            Source::Book(book) => book.estimated_cache_bytes(),
            _ => 0,
        };
        let registered_fonts_estimate = match &self.source {
            Source::Book(book) => book.fonts_resident_bytes(),
            _ => 0,
        }
        .saturating_add(crate::ui::platform::user_fonts::resident_bytes());
        CacheUsage {
            cpu_pixels,
            gpu_pixels_estimate,
            reusable_layout_estimate,
            font_cache_estimate: self.font_cache.estimated_cache_bytes(),
            decoding,
            decoding_bytes_estimate,
            registered_fonts_estimate,
        }
    }

    /// 每秒至多盘点一次，避免逐帧遍历字形表。这里只操作缓存，不触发重绘。
    pub(in crate::ui::features::reader) fn enforce_cache_budget(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self
            .cache_control
            .checked
            .is_some_and(|last| now.duration_since(last) < Duration::from_secs(1))
        {
            return;
        }
        self.cache_control.checked = Some(now);
        let textures = self.cache_control.budget.share(EvictStep::Textures) as usize;
        self.atlas
            .update(cx, |atlas, _| atlas.set_session_budget(textures));
        let usage = self.cache_usage(cx);
        self.cache_control.peaks.observe(usage);
        if self
            .cache_control
            .reported
            .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(30))
        {
            self.cache_control.reported = Some(now);
            self.report_cache_usage(usage, 0);
        }
        if self
            .cache_control
            .budget
            .pressure_for(usage.total_estimate())
            != Pressure::Normal
            && self.cache_prefetch_allowed()
        {
            self.trim_caches(cx);
        }
    }

    pub(in crate::ui::features::reader) fn cache_prefetch_allowed(&self) -> bool {
        self.cache_control
            .prefetch_after
            .is_none_or(|until| Instant::now() >= until)
    }

    pub(in crate::ui::features::reader) fn trim_caches(&mut self, cx: &mut Context<Self>) {
        let before = self.cache_usage(cx);
        self.cache_control.peaks.observe(before);
        self.cache_control.prefetch_after = Some(Instant::now() + Duration::from_secs(5));
        self.atlas
            .update(cx, |atlas, cx| atlas.trim_for_memory_pressure(cx));
        self.font_cache.release_cache_memory();
        if let Source::Book(book) = &mut self.source {
            book.release_cache_memory();
        }
        let after = self.cache_usage(cx);
        self.cache_control.peaks.observe(after);
        self.report_cache_usage(
            after,
            before
                .total_estimate()
                .saturating_sub(after.total_estimate()),
        );
    }

    fn report_cache_usage(&self, usage: CacheUsage, released_estimate: u64) {
        tracing::debug!(
            cpu_pixels = usage.cpu_pixels,
            gpu_pixels_estimate = usage.gpu_pixels_estimate,
            reusable_layout_estimate = usage.reusable_layout_estimate,
            font_cache_estimate = usage.font_cache_estimate,
            total_estimate = usage.total_estimate(),
            decoding = usage.decoding,
            decoding_bytes_estimate = usage.decoding_bytes_estimate,
            registered_fonts_estimate = usage.registered_fonts_estimate,
            peak_cpu_pixels = self.cache_control.peaks.cpu_pixels,
            peak_total_estimate = self.cache_control.peaks.total_estimate,
            peak_decoding = self.cache_control.peaks.decoding,
            peak_decoding_bytes = self.cache_control.peaks.decoding_bytes,
            released_estimate,
            budget = self.cache_control.budget.total,
            physical_bytes = self.cache_control.physical_bytes,
            unknown = "current_document,renderer,allocator",
            "阅读缓存账（CPU像素实测，其余为估算；不代表进程RSS）"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::ui::features::reader::model::session::ReaderSession;
    use crate::ui::services::config::ConfigHolder;
    use folio_core::content::document::{Document, from_content};
    use folio_core::infra::config::Config;
    use folio_core::{
        Chapter, ChapterExt, Content, DocMeta, FlowDoc, Locator, MemoryBudget, Reader,
    };
    use gpui_kit::{AppContext as _, Context, Render, Styled, TestAppContext, div, px, size};

    struct Blank;
    impl Render for Blank {
        fn render(
            &mut self,
            _: &mut gpui_kit::Window,
            _: &mut Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            div().size_full()
        }
    }

    #[gpui_kit::test]
    fn session_budget_reclaims_real_layout_caches_without_losing_the_current_page(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let reader = cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            let doc: Arc<dyn Document> = Arc::from(from_content(
                DocMeta::default(),
                Content::Reflow(FlowDoc {
                    chapters: vec![Chapter::inline(
                        "chapter",
                        "<p>缓存回收保留当前页面的排版与字符坐标。</p>".repeat(20),
                    )],
                    ..Default::default()
                }),
            ));
            cx.new(|cx| {
                ReaderSession::with_document(
                    doc,
                    Reader::default(),
                    None,
                    None,
                    Locator::start(),
                    cx,
                )
            })
        });
        let window = cx.open_window(size(px(600.0), px(800.0)), |_, _| Blank);
        cx.update_window(window.into(), |_, window, cx| {
            reader.update(cx, |reader, cx| {
                reader
                    .book_mut()
                    .unwrap()
                    .document_mut()
                    .ensure_layout(window, cx, 0.0);
                let document = reader.book().unwrap().document();
                let geometry = (document.height(), document.lines().len());
                assert!(geometry.0 > 0.0 && geometry.1 > 0);
                let before = reader.cache_usage(cx);
                assert!(
                    before.reusable_layout_estimate > 0,
                    "前提：真实排版已经生成块级缓存"
                );
                assert_eq!(
                    (before.decoding, before.decoding_bytes_estimate),
                    (0, 0),
                    "纯文字会话没有在途解码"
                );
                assert_eq!(
                    before.registered_fonts_estimate, 0,
                    "合成文档没有内嵌字体、用户字体清单为空，注册字节应为零"
                );
                reader.cache_control.budget = MemoryBudget::for_device(Some(1));
                reader.enforce_cache_budget(cx);
                let after = reader.cache_usage(cx);
                assert!(after.reusable_layout_estimate < before.reusable_layout_estimate);
                let document = reader.book().unwrap().document();
                assert_eq!((document.height(), document.lines().len()), geometry);
                assert!(!reader.cache_prefetch_allowed());
                assert!(reader.cache_control.peaks.total_estimate >= before.total_estimate());
                // 冷却期内不再清一次，避免下一帧重建的当前页字形缓存被反复丢弃。
                let checked = reader.cache_control.checked;
                reader.enforce_cache_budget(cx);
                assert_eq!(reader.cache_control.checked, checked);
            });
        })
        .unwrap();
    }
}
