//! 正文、页表与封面：整本落库 / 读回，以及正文的压缩。
//!
//! 「一个事务保证不留半本」这条只在这里：`save_flow_in` 先删光再重插，中途失败
//! 回滚成上一版，而不是留下一本半个章节的书。
//!
//! **正文根本不在这里**：这一层存的是它在容器里的位置（`chapters.locator`），
//! 打开时按位置回容器取，见 `crate::content::document::Document::chapter_source`。

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use super::Store;
use crate::Result;
use crate::content::format::{
    Chapter, ChapterLocator, ChapterText, Content, FixedDoc, FixedPage, FlowDoc, Format, ImageRes,
    locator_for,
};

/// `books.css` 列的载荷。正文出库之后样式表同样「库里只存位置」：
/// 导入索引只记容器里的 href 清单（[`CssStored::At`]），打开时回容器现取；
/// 文本真的在手上（`from_content`、旧库、手改）才落 [`CssStored::Inline`]。
#[derive(serde::Serialize, serde::Deserialize)]
enum CssStored {
    Inline(String),
    At(Vec<String>),
}

impl Store {
    /// 整本替换章节、CSS、目录、封面与图片，一个事务保证不留半本。
    pub fn save_flow(&self, book_id: i64, doc: &FlowDoc, format: Format) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Self::save_flow_in(&conn, book_id, doc, format)
    }

    /// `save_flow` 的不上锁版本；只由已经持了锁的一方调用（公开方法与 `save_document`）。
    ///
    /// `format` 是**算位置**用的，而且**必须给**：名单式容器按 `spine_href` 认条目、
    /// 单文件按序号认，而 `FlowDoc` 自己不知道是哪种格式。没有它就只能把正文本身
    /// 写进位置列（`Inline`）—— 那正是这个改造要去掉的。
    ///
    /// 输入那份 `doc.chapter_locators` 在写的时候**不看**：位置由格式现算，
    /// 免得「provider 交上来的空表」和「读回来的表」两种来源各写一遍。
    fn save_flow_in(conn: &Connection, book_id: i64, doc: &FlowDoc, format: Format) -> Result<()> {
        let tx = conn.unchecked_transaction()?;
        tx.execute("DELETE FROM chapters WHERE book_id = ?1", params![book_id])?;
        tx.execute("DELETE FROM images WHERE book_id = ?1", params![book_id])?;
        for (idx, ch) in doc.chapters.iter().enumerate() {
            // 正文本身**不进库**：存的是它在容器里的位置。这一句是整条不变量的落点 ——
            // 它在写路径上没有第二个分支，所以「正文被存进库」在结构上不可能发生。
            let at = locator_for(format, ch, idx);
            // 字数随索引那一趟一起进来（`plans/导入管线.md` §八 第 3 条：需要读完
            // 整本书才知道的数，全部在导入时算完）。`None` 原样写 NULL，不折成 0 ——
            // 「不知道」与「这一章是空的」必须分得开。
            let char_len: Option<i64> = ch.char_len().map(|n| n as i64);
            // 图片清单与字数同一趟扫出来，也就同一趟落库：「整本书那一列图」要能在
            // 读者走到那一章之前就把数报出来，靠的就是这一列（见 `ImageUse`）。
            // 空表写成 `[]` 而不是 NULL —— 这一格没有「还没算」那一档，老行由
            // `INDEX_VERSION` 的 bump 触发重扫来填。
            let images_json = serde_json::to_string(&ch.images)?;
            tx.execute(
                "INSERT INTO chapters (book_id, idx, title, locator, char_len, images_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    book_id,
                    idx as i64,
                    ch.title,
                    serde_json::to_string(&at)?,
                    char_len,
                    images_json,
                ],
            )?;
        }
        for img in &doc.images {
            tx.execute(
                "INSERT INTO images (book_id, href, source, media_type, width, height)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    book_id,
                    img.href,
                    img.source,
                    img.media_type,
                    img.width as i64,
                    img.height as i64
                ],
            )?;
        }
        // 样式表与正文同一立场：有位置清单就只存位置，文本在手上才内联。
        let css = if doc.css_hrefs.is_empty() {
            CssStored::Inline(doc.css.clone())
        } else {
            CssStored::At(doc.css_hrefs.clone())
        };
        // 封面写两格描述（`plans/导入管线.md` §八 第 4 条）：`covers/<hash>.<ext>` 的
        // 文件名由这两格拼出来。**字节不进库**（v16 删掉了 `books.cover`）：
        // 那是 MB 级的东西，而书架每次列书目都要过一遍 books 表 —— 几百本书
        // 就是几百 MB 的库，只为架上那一格画什么。
        //
        // 哈希是那一份**字节**的身份而不是这本书的，所以两本书共用一张封面时
        // 磁盘上只有一个文件（`format::cover::write` 那边保证）。
        tx.execute(
            "UPDATE books
                SET css = ?2, toc_json = ?3, index_version = ?4,
                    cover_hash = ?5, cover_mime = ?6, progression_rtl = ?7,
                    rules_hash = ?8
              WHERE id = ?1",
            params![
                book_id,
                serde_json::to_string(&css)?,
                serde_json::to_string(&doc.toc)?,
                crate::content::format::INDEX_VERSION,
                doc.cover_ref.as_ref().map(|c| c.hash.clone()),
                doc.cover_ref.as_ref().map(|c| c.mime.clone()),
                doc.progression_rtl as i64,
                // 「这份索引按哪一套规则切的」。除 `.txt` 以外一律 NULL —— 那些格式的
                // 切法写死在代码里，由 `index_version` 那一格管。判据只有一处定义
                // （[`crate::content::format::rules_hash`]），这一趟不自己猜哪些格式看规则表。
                crate::content::format::rules_hash(format),
            ],
        )?;
        tx.commit()?;
        tracing::debug!(
            "store: 第 {book_id} 本落库 {} 章、{} 张图、css {} 字节、目录 {} 项",
            doc.chapters.len(),
            doc.images.len(),
            doc.css.len(),
            doc.toc.len()
        );
        // 重导入是一删一插：旧正文与旧图片元数据刚变成空页，当场还给系统。
        // 不在事务里调的理由见 [`Store::reclaim`]。
        Self::reclaim(conn)?;
        Ok(())
    }

    pub fn load_flow(&self, book_id: i64) -> Result<Option<FlowDoc>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Self::load_flow_in(&conn, book_id)
    }

    /// `load_flow` 的不上锁版本；只由已经持了锁的一方调用（公开方法与 `load_content`）。
    fn load_flow_in(conn: &Connection, book_id: i64) -> Result<Option<FlowDoc>> {
        // `char_len` 可空：NULL = 这一章的字数还没算过（旧库升上来的行、
        // 或 provider 那一趟只读了清单）。读取端要把 NULL 与 0 分开 ——
        // 合成一个就会让进度条把「没算过」当成「这一章是空的」。
        // `images_json` 不给可空：「这本书这一章没有图」与「这一版还没重扫过」在界面上
        // 是同一个表现（查看器那一列里少一张），而 bump `INDEX_VERSION` 就是为了让
        // 老行全部重扫一次，读侧不必再分第三档。
        let mut stmt = conn.prepare(
            "SELECT title, locator, char_len, images_json FROM chapters WHERE book_id = ?1 ORDER BY idx",
        )?;
        let mut chapters = Vec::new();
        let rows = stmt
            .query_map(params![book_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (title, locator, char_len, images_json) in rows {
            let char_len = char_len.and_then(|n| usize::try_from(n).ok());
            let images =
                match serde_json::from_str::<Vec<crate::content::scan::ImageUse>>(&images_json) {
                    Ok(images) => images,
                    Err(err) => {
                        // 解不开只是这一章没有图片清单，正文照读 —— 与 `toc_json` 同一条立场，
                        // 也必须同样出声：界面那边看不出「清单坏了」与「这本书真没图」。
                        tracing::warn!(
                            "store: 第 {book_id} 本有一章的 images_json 解不开，当没有图：{err}"
                        );
                        Vec::new()
                    }
                };
            // 认不出来的位置（以后加的变体、手改过的库）当作「位置不明」：
            // 打开时回容器重读一遍，而不是把这一章当空的。
            let at = serde_json::from_str::<ChapterLocator>(&locator).unwrap_or_default();
            // `spine_href`（相对资源的基准目录）从位置推：条目名就是它。
            let spine_href = match &at {
                ChapterLocator::Entry(href) => href.clone(),
                _ => String::new(),
            };
            let chapter = if let ChapterLocator::Inline(text) = &at {
                // 唯一的例外：正文真的在库里（`from_content` 那一档、手改过的库）。
                // 正文在手，字数由 `inline` 自己算 —— 库里那一列这时是多余的。
                Chapter::inline(title, text.clone()).with_spine_href(spine_href)
            } else {
                Chapter {
                    title,
                    spine_href,
                    // 索引那一趟落的字数原样带回来：正文不在手上时它是**唯一**
                    // 能算出全局字符进度的东西（见 `plans/Locator.md` 第二节）。
                    char_len,
                    // 同一趟扫描落的图片清单也原样带回来 —— 它是「整本书那一列图」
                    // 在读者走到那一章之前的唯一来源（见 `ImageUse`）。
                    images,
                    text: ChapterText::At(at),
                }
            };
            chapters.push(chapter);
        }
        if chapters.is_empty() {
            tracing::debug!("store: 第 {book_id} 本没有入库的章节，回 None");
            return Ok(None);
        }
        let (css, toc_json, cover_hash, cover_mime, progression_rtl) = conn.query_row(
            "SELECT css, toc_json, cover_hash, cover_mime, progression_rtl
               FROM books WHERE id = ?1",
            params![book_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    // v18 之前入库的行是 0，读回「按左翻」正是那一版唯一可能的答案。
                    r.get::<_, i64>(4)?,
                ))
            },
        )?;
        // 两格要么都有（拼得出文件名），要么都没有。只剩一格说明那一列是手工改过的，
        // 拼不出路径 —— 当没有处理，退回 BLOB，别拿半个描述去猜路径。
        let cover_ref = match (cover_hash, cover_mime) {
            (Some(hash), Some(mime)) if !hash.is_empty() && !mime.is_empty() => {
                Some(crate::content::format::cover::CoverRef { hash, mime })
            }
            _ => None,
        };
        let (css, css_hrefs) = match serde_json::from_str::<CssStored>(&css) {
            Ok(CssStored::Inline(css)) => (css, Vec::new()),
            Ok(CssStored::At(hrefs)) => (String::new(), hrefs),
            Err(err) => {
                // 旧档：css 列就是拼接文本本身（正文出库之前的那一版）。当内联样式
                // 兜底，下次重扫（index_version 升级触发）会改写成新格式。
                tracing::debug!("store: 第 {book_id} 本的 css 列是旧格式，当内联样式：{err}");
                (css, Vec::new())
            }
        };
        let toc = match serde_json::from_str(&toc_json) {
            Ok(toc) => toc,
            Err(err) => {
                // 目录读不出来只是没有目录，正文照读 —— 但这一句必须留下，
                // 否则「这本书没有目录」将来查起来和「它本来就没目录」完全一样。
                tracing::warn!("store: 第 {book_id} 本的 toc_json 解不开，当没有目录：{err}");
                Vec::new()
            }
        };
        let mut img_stmt = conn.prepare(
            "SELECT href, source, media_type, width, height FROM images
             WHERE book_id = ?1 ORDER BY id",
        )?;
        let images = img_stmt
            .query_map(params![book_id], |r| {
                let href: String = r.get(0)?;
                let source: String = r.get(1)?;
                Ok(ImageRes {
                    // 空串 = 与 `href` 同一条：EPUB 以外的那些格式写空，v17 之前入库的老行
                    // 也读成这一档。这一格记的是「去哪儿读」，不是多一份事实。
                    source: if source.is_empty() {
                        href.clone()
                    } else {
                        source
                    },
                    href,
                    media_type: r.get(2)?,
                    width: r.get::<_, i64>(3)? as u32,
                    height: r.get::<_, i64>(4)? as u32,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Some(FlowDoc {
            chapters,
            css,
            css_hrefs,
            toc,
            // 字节不在库里（v16）：封面只留描述，读那一侧按它去 `covers/` 取文件。
            cover: None,
            images,
            cover_ref,
            progression_rtl: progression_rtl != 0,
        }))
    }

    /// 存一本定版式文档。与 [`Store::save_flow`] 对称：封面与目录跟着一起写。
    pub fn save_fixed(&self, book_id: i64, doc: &FixedDoc) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Self::save_fixed_in(&conn, book_id, doc)
    }

    /// 读一本定版式文档。没存过、或存的是可重排时返回 `None`。
    ///
    /// 坏 JSON 一律当「没有」而不是让整本书打不开：这一列可能来自以后的版本。
    /// 图片表由页表**重建**（href + 尺寸），不单独存一遍。
    pub fn load_fixed(&self, book_id: i64) -> Result<Option<FixedDoc>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Self::load_fixed_in(&conn, book_id)
    }

    /// 按内容家族存。界面拿到 [`Content`] 直接丢进来，不必自己分流。
    pub fn save_content(&self, book_id: i64, content: &Content, format: Format) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        match content {
            Content::Reflow(flow) => Self::save_flow_in(&conn, book_id, flow, format),
            Content::Fixed(fixed) => Self::save_fixed_in(&conn, book_id, fixed),
        }
    }

    /// 把一份已解析的文档落库。
    ///
    /// 与 [`Self::save_content`] 同一件事，只是入口收在「文档」而不是「内容」：
    /// 导入那一步手上只有一份 `dyn Document`（`document::open` 的产物），
    /// 让它自己 match 家族等于把 `Content` 的形状又散给调用方一次。
    pub fn save_document(
        &self,
        book_id: i64,
        doc: &dyn crate::content::document::Document,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        match doc.content() {
            crate::content::document::ContentRef::Reflow(flow) => {
                Self::save_flow_in(&conn, book_id, flow, doc.format())
            }
            crate::content::document::ContentRef::Fixed(fixed) => {
                Self::save_fixed_in(&conn, book_id, fixed)
            }
        }
    }

    /// 读一本已入库的书。先试可重排、再试定版式 —— 两边的「有没有」互斥
    /// （有章节的必定是可重排），不会歧义。
    pub fn load_content(&self, book_id: i64) -> Result<Option<Content>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(flow) = Self::load_flow_in(&conn, book_id)? {
            return Ok(Some(Content::Reflow(flow)));
        }
        Ok(Self::load_fixed_in(&conn, book_id)?.map(Content::Fixed))
    }

    /// 一本书的章标题，按章号对齐成一份可按 `[chapter]` 下标取用的表。
    ///
    /// 导出要用：导出的时机往往没有排版、甚至书已经不在这台机器上，标题却还在库里。
    /// 缺号（索引残了的老库）与空标题都补成 [`Chapter::inline`] 的空行 —— 标题真值
    /// 由导出那一头的兜底（「第 N 章」）接着答，这里不猜。
    pub fn chapter_titles(&self, book_id: i64) -> Result<Vec<Chapter>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare("SELECT idx, title FROM chapters WHERE book_id = ?1 ORDER BY idx")?;
        let rows = stmt
            .query_map(params![book_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        for (idx, title) in rows {
            // 中间缺号补空章：下标必须与章号对齐，错一位后面全部指错章。
            while out.len() < idx as usize {
                out.push(Chapter::inline("", ""));
            }
            out.push(Chapter::inline(title, ""));
        }
        Ok(out)
    }

    /// [`Store::save_fixed`] 的不上锁版本；只由已经持了锁的一方调用。
    fn save_fixed_in(conn: &Connection, book_id: i64, doc: &FixedDoc) -> Result<()> {
        let pages = serde_json::to_string(&doc.pages)?;
        // `index_version` 必须盖：不盖的话 `stale_reason` 永远说这份定版式索引是旧的
        // （定版式的页表同样由「当前这套算法」产出），于是**每次开书都白重扫一遍**。
        //
        // 封面写那两格描述（`cover_hash` / `cover_mime`），与可重排那一路同一条式子：
        // 字节不进库（v16），文件在 `covers/<hash>.<ext>`，库里只留拼文件名的两个数。
        // 这两列以前**不在这一条 UPDATE 里**，于是 `land_cover` 写进磁盘的那份首页图
        // 没有哪一列指向它 —— 漫画与 PDF 的书架封面就是这么整个丢掉的。
        conn.execute(
            "UPDATE books
                SET fixed_json = ?2, toc_json = ?3, index_version = ?4,
                    cover_hash = ?5, cover_mime = ?6
              WHERE id = ?1",
            params![
                book_id,
                pages,
                serde_json::to_string(&doc.toc)?,
                crate::content::format::INDEX_VERSION,
                doc.cover_ref.as_ref().map(|cover| cover.hash.clone()),
                doc.cover_ref.as_ref().map(|cover| cover.mime.clone()),
            ],
        )?;
        tracing::debug!(
            "store: 第 {book_id} 本落库 {} 页（定版式）",
            doc.pages.len()
        );
        Ok(())
    }

    /// [`Store::load_fixed`] 的不上锁版本；只由已经持了锁的一方调用。
    fn load_fixed_in(conn: &Connection, book_id: i64) -> Result<Option<FixedDoc>> {
        let row: Option<(String, String, Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT fixed_json, toc_json, cover_hash, cover_mime FROM books WHERE id = ?1",
                params![book_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((pages_json, toc_json, cover_hash, cover_mime)) = row else {
            return Ok(None);
        };
        if pages_json.trim().is_empty() {
            return Ok(None);
        }
        let pages: Vec<FixedPage> = match serde_json::from_str(&pages_json) {
            Ok(pages) => pages,
            Err(err) => {
                tracing::warn!("store: 第 {book_id} 本的 fixed_json 解不开，当没有：{err}");
                return Ok(None);
            }
        };
        let toc = serde_json::from_str(&toc_json).unwrap_or_default();
        let (width, height) = pages.first().map_or((0, 0), |p| (p.width, p.height));
        let images = pages.iter().flat_map(page_images).collect();
        // 封面字节不在库里（v16）：这两格描述拼出去年的那个文件，与可重排那一路同一条读法
        // —— 两格要么都有，只剩一格就当没有（拿半个描述去猜路径是更糟的那个错）。
        let cover_ref = match (cover_hash, cover_mime) {
            (Some(hash), Some(mime)) if !hash.is_empty() && !mime.is_empty() => {
                Some(crate::content::format::cover::CoverRef { hash, mime })
            }
            _ => None,
        };
        Ok(Some(FixedDoc {
            pages,
            toc,
            cover: None,
            cover_ref,
            images,
            width,
            height,
        }))
    }
}

/// 定版式的一页 → 图片元数据（主图 + 附加图）。渲染型（PDF）没有 href，不进图片表。
fn page_images(page: &FixedPage) -> Vec<ImageRes> {
    let mut out = Vec::with_capacity(1 + page.extra_images.len());
    if let Some(res) = image_res(page.image_href(), page.width, page.height) {
        out.push(res);
    }
    for extra in &page.extra_images {
        if let Some(res) = image_res(Some(&extra.href), extra.width, extra.height) {
            out.push(res);
        }
    }
    out
}

fn image_res(href: Option<&str>, width: u32, height: u32) -> Option<ImageRes> {
    let href = href?.to_string();
    let ext = Path::new(&href)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    Some(ImageRes {
        href: href.clone(),
        // 定版式的页没有「声明了但解不动」这一档：条目名就是字节所在。
        source: href,
        media_type: crate::content::format::media_type_of(&ext).to_string(),
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    //! 定版式持久化：v7 迁移、页表往返、按内容家族读写。

    use std::path::{Path, PathBuf};

    use crate::Store;
    use crate::content::format::{
        Chapter, Content, DocMeta, FixedDoc, FixedPage, FlowDoc, Format, PageImage, PageSource,
    };
    use crate::store::schema::MIGRATIONS;

    fn meta(title: &str) -> DocMeta {
        DocMeta {
            title: title.into(),
            authors: vec!["作者".into()],
            format: Format::Epub,
            size: 1,
            ..DocMeta::default()
        }
    }

    fn path(name: &str) -> PathBuf {
        Path::new("/books").join(name)
    }

    fn page(href: &str) -> FixedPage {
        FixedPage {
            source: PageSource::Image {
                href: href.to_string(),
            },
            width: 800,
            height: 1200,
            extra_images: Vec::new(),
        }
    }

    fn fixed_doc() -> FixedDoc {
        FixedDoc {
            pages: vec![page("1.png"), page("2.png")],
            toc: Vec::new(),
            cover: Some(vec![1, 2, 3]),
            // 存的时候会由页表重建，这里故意留空。
            images: Vec::new(),
            // 封面落盘之后的那两格描述（`covers/<hash>.<ext>` 的拼名材料）。
            cover_ref: Some(crate::content::format::cover::CoverRef {
                hash: "0123456789abcdef".into(),
                mime: "image/png".into(),
            }),
            width: 800,
            height: 1200,
        }
    }

    fn flow_doc() -> FlowDoc {
        FlowDoc {
            chapters: vec![Chapter::inline("一", "正文").with_spine_href("#0")],
            ..Default::default()
        }
    }

    /// 已发布的 v6 库要能升到 v7，而且它原有的书一本不能少。
    #[test]
    fn a_v6_database_upgrades_and_keeps_its_books() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("folio.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            for sql in &MIGRATIONS[..6] {
                conn.execute_batch(sql).unwrap();
            }
            conn.pragma_update(None, "user_version", 6).unwrap();
            conn.execute(
                "INSERT INTO books (path, title, authors, format, size, added_at)
                 VALUES ('a.epub', 'A', '', 'epub', 0, 0)",
                [],
            )
            .unwrap();
        }

        let store = Store::open_at(&db).unwrap();
        assert_eq!(store.user_version().unwrap(), MIGRATIONS.len() as i64);
        assert_eq!(store.book(1).unwrap().unwrap().title, "A");
        assert!(
            store.load_fixed(1).unwrap().is_none(),
            "升级上来的书没有定版式，该是 None 而不是空页表"
        );
    }

    /// 页表要逐字段往返，图片表由它重建（不单独存一遍）。
    #[test]
    fn a_fixed_document_round_trips_and_rebuilds_its_image_table() {
        let store = Store::in_memory().unwrap();
        let book = store.add_book(&path("c.cbz"), &meta("漫画")).unwrap();
        let doc = fixed_doc();
        store.save_fixed(book, &doc).unwrap();

        let got = store.load_fixed(book).unwrap().expect("刚存的该读得回来");
        assert_eq!(got.pages, doc.pages);
        // 封面字节不进库（v16），**描述进**：读回来的是那两格，界面按它去 `covers/` 取文件。
        // 这两列以前不在 `save_fixed_in` 的 UPDATE 里，于是定版式整族（漫画、PDF 的首页
        // 栅格）算完的封面没有哪一列指向它 —— 书架上就是一片占位。
        assert_eq!(got.cover, None, "封面字节不该跟着库回来");
        assert_eq!(
            got.cover_ref, doc.cover_ref,
            "封面的两格描述该原样回来，否则书架找不着那个文件"
        );
        assert_eq!((got.width, got.height), (800, 1200));
        assert_eq!(got.images.len(), 2, "图片表由页表重建");
        assert_eq!(got.images[0].href, "1.png");
        assert_eq!(got.images[0].media_type, "image/png");
        assert_eq!((got.images[0].width, got.images[0].height), (800, 1200));
    }

    #[test]
    fn content_dispatch_reads_back_the_right_family() {
        let store = Store::in_memory().unwrap();
        let reflow = store.add_book(&path("a.md"), &meta("文")).unwrap();
        store
            .save_content(reflow, &Content::Reflow(flow_doc()), Format::Text)
            .unwrap();
        assert!(matches!(
            store.load_content(reflow).unwrap(),
            Some(Content::Reflow(_))
        ));

        let comic = store.add_book(&path("b.cbz"), &meta("图")).unwrap();
        store
            .save_content(comic, &Content::Fixed(fixed_doc()), Format::Cbz)
            .unwrap();
        assert!(matches!(
            store.load_content(comic).unwrap(),
            Some(Content::Fixed(_))
        ));
        assert!(store.load_content(9999).unwrap().is_none(), "没这本书");
    }

    /// 多图页的两张图都要从页表重建进图片表 —— 库里没有独立的图片列，页表就是唯一来源。
    ///
    /// 只收主图的话，第二张在界面上取不到字节（画出来那块是空的），而页表明明记着它。
    #[test]
    fn a_multi_image_page_rebuilds_both_image_rows() {
        let store = Store::in_memory().unwrap();
        let book = store.add_book(&path("spread.epub"), &meta("跨页")).unwrap();
        let doc = FixedDoc {
            pages: vec![FixedPage {
                source: PageSource::Image {
                    href: "Images/left.png".into(),
                },
                width: 800,
                height: 1200,
                extra_images: vec![PageImage {
                    href: "Images/right.png".into(),
                    width: 600,
                    height: 1200,
                }],
            }],
            ..Default::default()
        };
        store.save_fixed(book, &doc).unwrap();
        let got = store.load_fixed(book).unwrap().unwrap();
        assert_eq!(got.pages[0].extra_images.len(), 1, "附加图要原样读回来");
        let hrefs: Vec<&str> = got.images.iter().map(|i| i.href.as_str()).collect();
        assert_eq!(
            hrefs,
            vec!["Images/left.png", "Images/right.png"],
            "两张都得建表，主图在前"
        );
        assert_eq!(
            (got.images[1].width, got.images[1].height),
            (600, 1200),
            "附加图带它自己的尺寸，不是主图的"
        );
    }

    /// 渲染型（PDF）没有 href，不该被塞进图片表。
    #[test]
    fn a_render_page_does_not_become_an_image_row() {
        let store = Store::in_memory().unwrap();
        let book = store.add_book(&path("d.pdf"), &meta("PDF")).unwrap();
        let doc = FixedDoc {
            pages: vec![FixedPage {
                source: PageSource::Render { index: 0 },
                width: 595,
                height: 842,
                extra_images: Vec::new(),
            }],
            ..Default::default()
        };
        store.save_fixed(book, &doc).unwrap();
        let got = store.load_fixed(book).unwrap().unwrap();
        assert_eq!(got.pages.len(), 1);
        assert!(got.images.is_empty(), "渲染型没有 href，不进图片表");
    }

    /// 样式表信封的往返：有清单就只存清单（正文出库之后 CSS 与正文同一立场）；
    /// 文本在手上才内联；旧档（css 列是裸文本的那一版）当内联兜底，等重扫改写。
    #[test]
    fn css_stores_as_locators_and_legacy_text_still_loads() {
        let store = Store::in_memory().unwrap();
        let book = store.add_book(&path("a.epub"), &meta("样式")).unwrap();

        // 索引路径：只有清单，库里不该有样式文本。
        let mut indexed = flow_doc();
        indexed.css_hrefs = vec![
            "OEBPS/Styles/style.css".into(),
            "OEBPS/Styles/cover.css".into(),
        ];
        store.save_flow(book, &indexed, Format::Epub).unwrap();
        let got = store.load_flow(book).unwrap().unwrap();
        assert!(got.css.is_empty(), "清单路径下不该带样式文本");
        assert_eq!(got.css_hrefs, indexed.css_hrefs, "清单要原样往返");

        // 内联路径（from_content / 测试 / 手改）：文本在手上，原样往返。
        let mut inline = flow_doc();
        inline.css = "p { margin: 0 }".into();
        store.save_flow(book, &inline, Format::Epub).unwrap();
        let got = store.load_flow(book).unwrap().unwrap();
        assert_eq!(got.css, "p { margin: 0 }");
        assert!(got.css_hrefs.is_empty(), "内联路径不该长出清单");

        // 旧档：css 列是裸文本（正文出库之前的那一版），当内联兜底而不是整本打不开。
        use rusqlite::params;
        {
            let conn = store.conn.lock().unwrap_or_else(|e| e.into_inner());
            conn.execute(
                "UPDATE books SET css = 'p { color: red }' WHERE id = ?1",
                params![book],
            )
            .unwrap();
        }
        let got = store.load_flow(book).unwrap().unwrap();
        assert_eq!(got.css, "p { color: red }", "旧档当内联样式");
        assert!(got.css_hrefs.is_empty());
    }
}
