//! AZW4（Kindle Print Replica）：PalmDB 容器里装着一份 PDF。
//!
//! 它不是第四种 PDF 引擎，而是「PDF 字节从哪儿来」的第二种答案：普通 `.pdf` 从文件
//! 打开，`.azw4` 先从 PalmDB 的记录里把 PDF 取出来，再交给同一个 [`Pdf`](hayro::hayro_syntax::Pdf)。
//!
//! 判据只看**记录内容**：找一条以 `%PDF-` 开头的记录。不靠文件扩展名，也不靠
//! Mobipocket 头的版本号 —— 后者在真书之间并不一致。
//!
//! 页表、书签、书目、封面全部复用 [`super::pdf::read_pdf`]；这里只负责「取 PDF 字节」
//! 与「把格式报成 Azw4」。

use std::path::Path;
use std::sync::Arc;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;

use super::palmdb::PalmDb;
use super::pdf::{PdfHandle, failure, page_text, read_pdf, render_png};
use super::{Content, DocMeta, DocumentProvider, Format, PageText, Passphrase, stem_of};
use crate::Result;

pub struct Azw4Adapter {
    /// 这一本书的解析句柄，与 [`super::pdf::PdfAdapter`] 同形（含同一份负缓存与同一套
    /// 口令作废规则，实现见 [`super::pdf::PdfHandle`]）。
    ///
    /// 从前这一族每问一次都重新从 PalmDB 里拷一遍 payload 再解一遍结构，理由是「hayro 要
    /// 一份独立的整本字节，导入时为一张封面多拷一份不值」。换成一台引擎之后这条理由不成立
    /// 了 —— 一次打开就是一份句柄，之后取页、抽字、封面都共用它，峰值驻留反而只有一份。
    document: PdfHandle,
    /// 读者给的那份口令。AZW4 里那份 PDF 完全可以带 `/Encrypt`，所以这一族与 `.pdf`
    /// 共用同一条口令路 —— 两条路各判一次「要不要问口令」，屏上就是同一本书在两种来源下
    /// 一种能问一种不能。
    passphrase: Option<Passphrase>,
    /// 与 `document` 配套的解释器设置（克隆只是几条 `Arc`）。
    settings: InterpreterSettings,
}

impl Default for Azw4Adapter {
    fn default() -> Self {
        Self {
            document: PdfHandle::new(),
            passphrase: None,
            settings: InterpreterSettings::default(),
        }
    }
}

impl Azw4Adapter {
    fn document(&self, path: &Path) -> Option<Arc<Pdf>> {
        self.document.get(|| open(path, self.passphrase.as_ref()))
    }
}

impl DocumentProvider for Azw4Adapter {
    fn format(&self) -> Format {
        Format::Azw4
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(super::extension_of(path).as_deref(), Some("azw4"))
    }

    fn set_passphrase(&mut self, passphrase: Option<Passphrase>) {
        // 与 `.pdf` 同一条：先作废再存，否则下一次问会用旧口令重开并把结果当作这一代的。
        self.document.invalidate();
        self.passphrase = passphrase;
    }

    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let (db, _) = pdf_payload(path)?;
        // 与 `.pdf` 那边同一条：`get_or_try` 的 `Err` 只在刚失败这一趟出现，
        // 而失败本身记进负缓存（`None`），别每帧重试。
        let document = self
            .document
            .get_or_try(|| open(path, self.passphrase.as_ref()))?
            .ok_or_else(|| {
                crate::Error::UnsupportedFormat(format!(
                    "{}：结构读不出来，PalmDB 里那份 PDF 可能被截断过",
                    file_name(path)
                ))
            })?;
        let title = if db.db_name().trim().is_empty() {
            stem_of(path)
        } else {
            db.db_name().to_string()
        };
        read_pdf(
            path,
            &document,
            Format::Azw4,
            title,
            std::fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or(0),
            // 封面这一趟只在导入时跑一次。封面与阅读页走同一条链（`render_png`），
            // 字节也共用同一份句柄 —— 不再有「另一台引擎要独立字节」那一笔。
            render_png(&document, 0, super::pdf::COVER_EDGE, &self.settings),
        )
    }

    /// 取页：与 `.pdf` 同一个句柄、同一条栅格化。
    fn render_page(&self, path: &Path, page: usize, edge: u32) -> Option<Vec<u8>> {
        let document = self.document(path)?;
        render_png(&document, page, edge, &self.settings)
    }

    /// 文字层同一份：Kindle 印刷版里那份 PDF 与普通 `.pdf` 只差字节从哪儿来。
    fn page_text(&self, path: &Path, page: usize) -> Option<PageText> {
        let document = self.document(path)?;
        page_text(&document, page, &self.settings)
    }

    /// 整本一次：句柄开一趟，然后逐页抽字。搜索那一趟走的必须是这一条。
    fn page_texts(&self, path: &Path) -> Option<Vec<String>> {
        let document = self.document(path)?;
        Some(
            (0..document.pages().len())
                .map(|page| super::pdf::page_texts_one(&document, page, &self.settings))
                .collect(),
        )
    }
}

/// 记录里那条 PDF 的起始记录号；判定层与本适配器共用。
pub(crate) fn pdf_record(db: &PalmDb) -> Option<usize> {
    db.find_record_starting_with(b"%PDF-")
}

/// 从 PalmDB 里取出那份 PDF 并打开，带上读者手上那份口令。
///
/// 取字节这一趟与口令无关：`/Encrypt` 是 PDF 自己那一层的事，藏在记录里那串字节
/// 读出来是同一份，所以只有 `Pdf::new_with_password` 那一问需要口令。
fn open(path: &Path, passphrase: Option<&Passphrase>) -> Result<Pdf> {
    let (_, bytes) = pdf_payload(path)?;
    super::pdf::PdfAdapter::from_bytes_with(bytes, passphrase).map_err(|err| failure(path, err))
}

/// 打开容器并取出 PDF 记录。
fn pdf_payload(path: &Path) -> Result<(PalmDb, Vec<u8>)> {
    let db = PalmDb::open(path)?;
    let index = pdf_record(&db).ok_or_else(|| {
        crate::Error::UnsupportedFormat(format!(
            "{}：PalmDB 里找不到以 %PDF- 开头的记录",
            file_name(path)
        ))
    })?;
    let bytes = db.read(index).ok_or_else(|| {
        crate::Error::UnsupportedFormat(format!(
            "{}：第 {index} 条 PDF 记录读不出来",
            file_name(path)
        ))
    })?;
    Ok((db, bytes))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use super::Azw4Adapter;
    use crate::{Content, DocumentProvider, Format, palmdb::PalmDb};

    const HEADER: usize = 78;

    /// 一份公版/合成的 PDF 夹具（与 `pdf.rs` 的测试共用）。
    fn pdf_fixture() -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/pdf/three-pages.pdf");
        std::fs::read(path).expect("缺 PDF 样本")
    }

    /// 摆一个最小 PalmDB：头 + 记录表 + 记录内容。
    /// `kind` 是偏移 60 那 8 个字节（`BOOKMOBI` 就是 MOBI 家族）。
    fn db_at(dir: &Path, name: &str, db_name: &str, records: &[&[u8]]) -> PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        let mut head = vec![0u8; HEADER];
        head[..db_name.len().min(32)].copy_from_slice(&db_name.as_bytes()[..db_name.len().min(32)]);
        head[60..64].copy_from_slice(b"BOOK");
        head[64..68].copy_from_slice(b"MOBI");
        head[76..78].copy_from_slice(&(records.len() as u16).to_be_bytes());
        file.write_all(&head).unwrap();
        let mut start = HEADER + records.len() * 8;
        let mut table = Vec::with_capacity(records.len() * 8);
        for record in records {
            table.extend_from_slice(&(start as u32).to_be_bytes());
            table.push(0);
            table.extend_from_slice(&[0, 0, 0]);
            start += record.len();
        }
        file.write_all(&table).unwrap();
        for record in records {
            file.write_all(record).unwrap();
        }
        path
    }

    #[test]
    fn an_azw4_is_a_palmdb_with_a_pdf_record() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = pdf_fixture();
        // 记录 0 只放一段像 MOBI 头的字节；判定不看它，适配器只找 `%PDF-`。
        let path = db_at(dir.path(), "书.azw4", "印刷版样书", &[b"MOBI-HEADER", &pdf]);
        let db = PalmDb::open(&path).unwrap();
        assert_eq!(super::pdf_record(&db), Some(1));

        let (meta, content) = Azw4Adapter::default().read(&path).unwrap();
        assert_eq!(meta.format, Format::Azw4);
        // PDF 自己的 `/Title` 优先于 PalmDB 名；夹具里写的是「测试合成本」。
        assert_eq!(meta.title, "测试合成本");
        let Content::Fixed(fixed) = content else {
            panic!("AZW4 该读成定版式");
        };
        assert_eq!(fixed.page_count(), 3);
        assert!(fixed.cover.is_some(), "首页该栅格化成封面");
        assert!(matches!(
            fixed.pages[0].source,
            super::super::PageSource::Render { .. }
        ));
    }

    #[test]
    fn a_palmdb_without_a_pdf_record_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_at(
            dir.path(),
            "坏.azw4",
            "没有 PDF",
            &[b"MOBI-HEADER", b"not a pdf"],
        );
        assert!(Azw4Adapter::default().read(&path).is_err());
    }
}
