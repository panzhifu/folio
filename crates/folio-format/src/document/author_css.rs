//! 作者样式表的装配：清单样式逐份取文、展开 `@import`、拼成一份进层叠的文本。
//!
//! 从 `book.rs` 切出来只因为 book.rs 在 FOLIO-GOV-004 的债务基线上，只许变短；
//! 这一整段只吃容器读取，不碰书的其余状态。

use super::ContentRef;
use crate::{container::Container, provider::DocumentProvider};

/// 拼好的一份作者样式文本。口径（清单优先、每份用自己的目录折、`@import` 展开
/// 的防环与降级）写在每一段的现场注释里。
pub(super) fn assemble(
    content: &ContentRef<'_>,
    shared: Option<&Container>,
    provider: &dyn DocumentProvider,
    path: &std::path::Path,
) -> String {
    let ContentRef::Reflow(flow) = content else {
        // 定版式没有作者样式表。
        return String::new();
    };
    if !flow.css.is_empty() {
        // 文本就在手上（`read` 全文路径、`from_content`、旧库兜底）—— 直接用。
        return flow.css.clone();
    }
    if flow.css_hrefs.is_empty() {
        return String::new();
    }
    let mut css = String::new();
    let mut seen = std::collections::HashSet::new();
    for href in &flow.css_hrefs {
        // 与取图同一条：容器先试（几份样式表共用一次打开），再退回 provider。
        let text = shared
            .and_then(|container| container.read_text(href))
            .or_else(|| provider.entry_text(path, href));
        let Some(text) = text else {
            tracing::debug!("document: 样式表 {href} 不在容器里，跳过");
            continue;
        };
        let dir = crate::dir_of(href);
        // `@import` 按这份样式表自己的目录回容器展开，防环的 seen 一份书用到底。
        let text = crate::css::expand_css_imports(&text, &dir, 0, &mut seen, &mut |p| {
            shared
                .and_then(|container| container.read_text(p))
                .or_else(|| provider.entry_text(path, p))
        });
        // 展开返回的文本 url() 已折好，不再过 absolutize。
        css.push_str(&text);
        css.push('\n');
    }
    css
}
