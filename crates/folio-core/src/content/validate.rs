//! 文档格式校验：在 provider 解析之后、排版之前，对文档结构做一轮完整性检查。
//!
//! 住在 [`crate::content`] 而不是 [`crate::ingest`]：它吃的是一份**已经解析好的**
//! `Content`，不碰容器、不碰文件系统、不碰库，而调用它的是开书那一路
//! （`document::BookDocument::open_with` / `from_content`）—— 导入只是顺带也走那一趟。
//! `content` 那一区的分区判据写的是「不依赖 `store` 与 `ingest`」，从前这两句调用
//! 正好是这条判据上唯一的破口。
//!
//! 目标不是「拒绝坏书」（provider 已经做了「坏一处不整本失败」的降级），而是：
//! - **尽早发现结构矛盾**：manifest 列了但 zip 里没有、spine 指了 manifest 里不存在的 id、
//!   目录锚点指着一章里不存在的 `id` —— 这些在排版期会变成空白页或跳错位置，
//!   但报错时离出错点已经隔了好几层。
//! - **给日志一个可搜索的信号**：`validation: EPUB "xxx.epub" 3 issues` 比
//!   「打开后某一页是空的」好查十倍。
//! - **不阻塞阅读**：校验结果只写日志、不计入 `Result`，书仍然照开。
//!
//! 校验按格式分流，每种格式一个 `check_*` 函数，返回 [`Issues`]。

use std::collections::HashSet;
use std::path::Path;

use crate::content::format::{Content, FixedDoc, FlowDoc, TocEntry};

/// 一份文档校验出的问题表。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Issues {
    /// 具体问题，按发现顺序排。
    pub items: Vec<Issue>,
}

impl Issues {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// 加一条问题。`path` 是出问题的那份文档在容器里的路径。
    pub fn push(&mut self, kind: IssueKind, path: impl Into<String>, detail: impl Into<String>) {
        self.items.push(Issue {
            kind,
            path: path.into(),
            detail: detail.into(),
        });
    }
}

/// 一条具体问题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub kind: IssueKind,
    pub path: String,
    pub detail: String,
}

/// 问题严重程度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IssueKind {
    /// 信息级：不影响阅读，但不符合规范（如缺少可选的 nav）。
    Info,
    /// 警告级：可能影响部分功能（如目录锚点失效、图片取不到）。
    Warn,
    /// 错误级：结构矛盾，会导致空白页或跳转失败（如 spine 指向不存在的 manifest 条目）。
    Error,
}

/// 校验一份文档。返回的问题只写日志，不阻塞阅读。
pub fn check(content: &Content, path: &Path) -> Issues {
    let mut issues = Issues::default();
    match content {
        Content::Reflow(flow) => check_flow(flow, &mut issues),
        Content::Fixed(fixed) => check_fixed(fixed, &mut issues),
    }
    if !issues.is_empty() {
        let file = path.file_name().map_or_else(
            || path.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        let mut errors = 0;
        let mut warns = 0;
        let mut infos = 0;
        for issue in &issues.items {
            match issue.kind {
                IssueKind::Error => errors += 1,
                IssueKind::Warn => warns += 1,
                IssueKind::Info => infos += 1,
            }
        }
        tracing::warn!(
            "validation: {file} {} issues ({errors} errors, {warns} warnings, {infos} infos)",
            issues.len()
        );
        for issue in &issues.items {
            let level = match issue.kind {
                IssueKind::Info => "INFO",
                IssueKind::Warn => "WARN",
                IssueKind::Error => "ERROR",
            };
            tracing::debug!(
                "  [{level}] {}: {} ({})",
                issue.path,
                issue.detail,
                issue.kind.as_str()
            );
        }
    }
    issues
}

impl IssueKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

// ---- 可重排文档 ----

fn check_flow(flow: &FlowDoc, issues: &mut Issues) {
    // 1. 章节表非空。
    if flow.chapters.is_empty() {
        issues.push(IssueKind::Error, "<doc>", "文档没有章节");
        return;
    }

    // 2. 每章的 spine_href 唯一。
    let mut seen = HashSet::new();
    for (ix, chapter) in flow.chapters.iter().enumerate() {
        if !seen.insert(&chapter.spine_href) {
            issues.push(
                IssueKind::Warn,
                &chapter.spine_href,
                format!("第 {ix} 章的 spine_href 与前面重复"),
            );
        }
    }

    // 3. 目录锚点指向的章文件能在 spine 里找到。
    let spine_set: HashSet<&str> = flow
        .chapters
        .iter()
        .map(|c| c.spine_href.as_str())
        .collect();
    check_toc_targets(&flow.toc, &spine_set, issues);

    // 4. 图片索引里的 href 非空。
    for (ix, img) in flow.images.iter().enumerate() {
        if img.href.is_empty() {
            issues.push(IssueKind::Warn, format!("images[{ix}]"), "图片 href 为空");
        }
        if img.width == 0 || img.height == 0 {
            issues.push(
                IssueKind::Info,
                &img.href,
                format!("图片固有尺寸缺失 ({}x{})", img.width, img.height),
            );
        }
    }

    // 5. 章节源文本非空。
    for (ix, chapter) in flow.chapters.iter().enumerate() {
        // 正文可能还没取（索引那一趟只有位置，见 `ChapterText::At`）——
        // 那种情况说不了「这一章是空的」，只有正文真在手上且真为空才算。
        if matches!(&chapter.text, crate::content::format::ChapterText::Inline(text) if text.trim().is_empty())
        {
            issues.push(
                IssueKind::Warn,
                &chapter.spine_href,
                format!("第 {ix} 章源文本为空"),
            );
        }
    }
}

/// 递归检查目录锚点是否能对上 spine。
fn check_toc_targets(entries: &[TocEntry], spine: &HashSet<&str>, issues: &mut Issues) {
    for entry in entries {
        if !entry.href.is_empty() && !spine.contains(entry.href.as_str()) {
            // 只比文件名段（丢目录段、大小写不敏感），与 book.rs 同一条规则。
            let base = entry.href.rsplit('/').next().unwrap_or(&entry.href);
            let matched = spine.iter().any(|s| {
                let s_base = s.rsplit('/').next().unwrap_or(s);
                s_base.eq_ignore_ascii_case(base)
            });
            if !matched {
                issues.push(
                    IssueKind::Warn,
                    &entry.href,
                    format!("目录条目「{}」指向的章文件不在 spine 里", entry.title),
                );
            }
        }
        if let Some(ref fragment) = entry.fragment
            && fragment.is_empty()
        {
            issues.push(
                IssueKind::Info,
                &entry.href,
                format!("目录条目「{}」的 fragment 为空", entry.title),
            );
        }
        check_toc_targets(&entry.children, spine, issues);
    }
}

// ---- 定版式文档 ----

fn check_fixed(fixed: &FixedDoc, issues: &mut Issues) {
    if fixed.pages.is_empty() {
        issues.push(IssueKind::Error, "<doc>", "定版式文档没有页面");
        return;
    }

    // 1. 每页都有图源。
    for (ix, page) in fixed.pages.iter().enumerate() {
        if page.image_href().is_none() {
            issues.push(
                IssueKind::Warn,
                format!("pages[{ix}]"),
                "页面没有图源（Render 型尚未支持）",
            );
        }
        if page.width == 0 || page.height == 0 {
            issues.push(
                IssueKind::Info,
                format!("pages[{ix}]"),
                format!("页面尺寸缺失 ({}x{})", page.width, page.height),
            );
        }
    }

    // 2. 图片索引与页表一致。
    let page_hrefs: HashSet<&str> = fixed.pages.iter().filter_map(|p| p.image_href()).collect();
    let index_hrefs: HashSet<&str> = fixed.images.iter().map(|i| i.href.as_str()).collect();

    for href in &page_hrefs {
        if !index_hrefs.contains(href) {
            issues.push(IssueKind::Warn, *href, "页表引用了图片索引里没有的 href");
        }
    }
    for href in &index_hrefs {
        if !page_hrefs.contains(href) {
            issues.push(IssueKind::Info, *href, "图片索引里有但页表未引用");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::content::format::Chapter;
    #[test]
    fn empty_chapters_is_an_error() {
        let flow = FlowDoc::default();
        let mut issues = Issues::default();
        check_flow(&flow, &mut issues);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues.items[0].kind, IssueKind::Error);
    }

    #[test]
    fn duplicate_spine_href_is_a_warning() {
        let flow = FlowDoc {
            chapters: vec![
                Chapter::inline("A", "<p>a</p>").with_spine_href("chap.xhtml"),
                Chapter::inline("B", "<p>b</p>").with_spine_href("chap.xhtml"),
            ],
            ..Default::default()
        };
        let mut issues = Issues::default();
        check_flow(&flow, &mut issues);
        assert!(issues.items.iter().any(|i| i.kind == IssueKind::Warn));
    }

    #[test]
    fn toc_pointing_outside_spine_is_a_warning() {
        let flow = FlowDoc {
            chapters: vec![
                Chapter::inline("唯一一章", "<p>唯一一章</p>").with_spine_href("chap1.xhtml"),
            ],
            toc: vec![TocEntry {
                title: "不存在的章".into(),
                href: "nowhere.xhtml".into(),
                fragment: None,
                page: None,
                children: vec![],
            }],
            ..Default::default()
        };
        let mut issues = Issues::default();
        check_flow(&flow, &mut issues);
        assert!(
            issues
                .items
                .iter()
                .any(|i| i.kind == IssueKind::Warn && i.detail.contains("不在 spine"))
        );
    }

    #[test]
    fn fixed_with_no_pages_is_an_error() {
        let fixed = FixedDoc::default();
        let mut issues = Issues::default();
        check_fixed(&fixed, &mut issues);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues.items[0].kind, IssueKind::Error);
    }

    #[test]
    fn fixed_page_missing_image_is_a_warning() {
        use crate::content::format::{FixedPage, PageSource};

        let fixed = FixedDoc {
            pages: vec![FixedPage {
                source: PageSource::Render { index: 0 },
                width: 0,
                height: 0,
                extra_images: Vec::new(),
            }],
            ..Default::default()
        };
        let mut issues = Issues::default();
        check_fixed(&fixed, &mut issues);
        assert!(
            issues
                .items
                .iter()
                .any(|i| i.kind == IssueKind::Warn && i.detail.contains("没有图源"))
        );
    }
}
