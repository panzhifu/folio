//! MHTML（`.mht` / `.mhtml`，RFC 2557）：**一个文件装着整张网页** —— HTML、CSS、图片，
//! 用一个 MIME 存档包在一起。
//!
//! 它补的缺口很具体：浏览器「网页，全部」另存出来是一个**目录**（不在范围内，那是
//! [`super::html`] 明确不读的那一档 —— 「哪份是主文档」要猜，而猜错的代价是整本书只读到
//! 一半），而「网页，单个文件」、邮件里转出来的正文存档、Word 存的 `.mht` 是**一个文件**。
//!
//! 形状上它就是 [`super::htmlz`] 换了容器：HTMLZ 是「ZIP 里几份 HTML 与一堆文件」，
//! MHTML 是「一个文本存档里几段带头的部分」。所以两边同一套坐标 —— 每一份 `text/html`
//! 是一章（[`super::ChapterLocator::Entry`]，按部分键现取），`<img src>` 与 CSS 的 `url()` 折成
//! **容器内的规范键**，字节不入库，画的时候按 [`DocumentProvider::image_bytes`] 现解那一段。
//!
//! ## 信封与部分
//!
//! ```text
//! MIME-Version: 1.0
//! Content-Type: multipart/related; type="text/html"; boundary=----=_NextPart_000_0CFB
//! Content-Location: file:///C:/Users/me/Desktop/报告.htm
//!
//! ------=_NextPart_000_0CFB
//! Content-Location: file:///C:/Users/me/Desktop/报告.htm
//! Content-Transfer-Encoding: quoted-printable
//! Content-Type: text/html; charset="utf-8"
//!
//! <html>…<img src="报告_files/image001.png">…
//! ------=_NextPart_000_0CFB--
//! ```
//!
//! 部分靠**行首**那条 `--<boundary>` 分（不是子串匹配 —— 正文里出现同一串字面量不算
//! 分隔符），`--<boundary>--` 结束。每部分自己头里三样要紧：`Content-Location`（这一部分
//! 的地址）、`Content-Transfer-Encoding`（`quoted-printable` / `base64` / `7bit` / `8bit`）、
//! `Content-Type` 里那个 `charset`（交给 [`super::charset::decode`] 当「容器报的码页」那一档）。
//!
//! ## 键怎么定
//!
//! 生产者写下的 `Content-Location` 是**来源机器上的地址**（IE/Word 写 `file:///C:/…`、
//! Chrome 写原来那个 `https://…`），落地后它就是别人机器上的一个路径，指不到容器里的东西。
//! 所以这一层把两边都折到主文档那个目录的相对坐标上（[`anchor_key`]）：剥 scheme、authority
//! 与盘符，反斜杠换正斜杠，百分号解码，折 `./` 与 `../`；落在主文档目录下取相对，落在外面
//! 退到文件名那一格，撞名的补一层目录、再撞加后缀。正文里的引用过**同一个函数**
//! （[`Archive::locate`]），两侧才会在那一张表上相遇。
//!
//! ## 一期不做
//!
//! - 只解**一层** `multipart/*`：邮件那种 `multipart/mixed` 套 `alternative`（同一份正文的
//!   plain 与 HTML 两个版本）里，内层的头会被当成一个部分、它下面的部分各自照收，症状是那封
//!   邮件多出/plain 那一版的一章 —— 而不是整本读不出来。
//! - `uuencode` 那一种码头不认（按 `7bit` 原样收，症状是那种图字节是乱码而解不出 = 不登记）。
//! - 表单、脚本、`<frame>`：引擎不执行，正文里本来就没有它们的位置。

use std::path::Path;

use encoding_rs::Encoding;

use super::charset;
use super::{
    Chapter, Content, DocMeta, DocumentProvider, FlowDoc, Format, ImageRes, absolutize_css_urls,
    decode_base64, dir_of, extension_of, guess_media_type, image_dims, is_decodable_bitmap,
    percent_decode, resolve_href, stem_of,
};
use crate::Result;
use crate::content::dom::Dom;
use crate::content::scan::ImageUse;
use crate::infra::error::IoResultExt as _;

pub struct MhtmlAdapter;

impl DocumentProvider for MhtmlAdapter {
    fn format(&self) -> Format {
        Format::Mhtml
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(extension_of(path).as_deref(), Some("mhtml") | Some("mht"))
    }

    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let archive = open(path)?;
        let title = super::html::document_title(&archive.parts[archive.html[0]].text(&archive))
            .unwrap_or_else(|| stem_of(path));
        let mut css = String::new();
        let mut images: Vec<ImageRes> = Vec::new();
        let mut chapters = Vec::with_capacity(archive.html.len());
        for index in &archive.html {
            let part = &archive.parts[*index];
            let raw = part.text(&archive);
            if raw.trim().is_empty() {
                continue;
            }
            let head = super::html::document_title(&raw)
                .or_else(|| file_stem(&part.key))
                .unwrap_or_else(|| title.clone());
            css.push_str(&archive.styles_of(&raw, &part.key));
            archive.register_images(&raw, &part.key, &mut images);
            chapters.push(
                Chapter::entry(head, part.key.clone())
                    .with_char_len(super::visible_chars(&raw))
                    .with_images(image_uses(&archive, &raw, &part.key)),
            );
        }
        if chapters.is_empty() {
            return Err(unsupported(path, "切得出部分，却没有一份读得出正文"));
        }
        let meta = DocMeta {
            title,
            format: Format::Mhtml,
            size: archive.size,
            ..DocMeta::default()
        };
        Ok((
            meta,
            Content::Reflow(FlowDoc {
                chapters,
                css,
                images,
                ..Default::default()
            }),
        ))
    }

    fn entry_text(&self, path: &Path, entry: &str) -> Option<String> {
        let archive = open(path).ok()?;
        let index = archive.by_key(entry)?;
        let part = &archive.parts[index];
        Some(part.text(&archive))
    }

    fn read_texts(&self, path: &Path) -> Option<Vec<String>> {
        let archive = open(path).ok()?;
        Some(
            archive
                .html
                .iter()
                .map(|index| archive.parts[*index].text(&archive))
                .collect(),
        )
    }

    /// 字节按需现解一次：那一段 base64 / quoted-printable 就在文件里，与 EPUB 每次重开
    /// 容器解一个条目、与 FB2 每次重解一段 `<binary>` 是同一笔账。
    fn image_bytes(&self, path: &Path, href: &str) -> Option<Vec<u8>> {
        let archive = open(path).ok()?;
        let index = archive.resolve(href, "")?;
        archive.parts[index].decoded()
    }
}

/// 部分的传输编码。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Transfer {
    Raw,
    Base64,
    QuotedPrintable,
}

struct Part {
    /// 容器内的规范键（见模块头那一节）。
    key: String,
    /// `Content-ID`（两侧尖括号已去掉），邮件转出来的存档按 `cid:` 引用它。
    cid: Option<String>,
    media_type: String,
    /// 这一部分自己声明的码页（`charset=`），当「容器报的码页」那一档交给 `charset::decode`。
    charset: Option<&'static Encoding>,
    transfer: Transfer,
    /// 部分体在文件里的那一段字节（传输编码还没解）。
    body: Vec<u8>,
}

impl Part {
    /// 解掉传输编码 → 字节。
    fn decoded(&self) -> Option<Vec<u8>> {
        match self.transfer {
            Transfer::Raw => Some(self.body.clone()),
            // base64 解不出（截断、混进了非 base64 字符）就是这一份没有：与 FB2 的
            // `<binary>` 同一立场，不让一个坏部分带崩整本书。
            Transfer::Base64 => decode_base64(&String::from_utf8_lossy(&self.body)),
            Transfer::QuotedPrintable => Some(quoted_printable(&self.body)),
        }
    }

    fn text(&self, archive: &Archive) -> String {
        let bytes = self.decoded().unwrap_or_default();
        charset::decode(&bytes, self.charset.or(archive.envelope_charset))
    }
}

/// 一份 MHTML 存档：部分表 + 主文档那一份的下标。
struct Archive {
    parts: Vec<Part>,
    /// HTML 部分的下标：主文档第一，其余按文件里的序。
    html: Vec<usize>,
    /// 定键时剥掉的那个前缀（`locate` 要把来源地址折回同一个坐标，两处必须用同一个）。
    anchor: String,
    /// 信封自己报的码页（`Content-Type: …; charset=`）。部分头里那条优先。
    envelope_charset: Option<&'static Encoding>,
    size: u64,
}

impl Archive {
    fn by_key(&self, key: &str) -> Option<usize> {
        self.parts.iter().position(|part| part.key == key)
    }

    /// 把一条写下来的引用（`src` / `href`）落到某一部分上。
    ///
    /// 四步都是**同一张表**上的查找，顺序是「先信得过的、再降级」：
    /// 1. `cid:` → 按 `Content-ID`；
    /// 2. 绝对形式（带 scheme 或带盘符）→ [`absolute_key`] 折过再剥锚点；
    ///    相对形式 → 按**引用它那一份**的目录合并（与 EPUB 的 spine 项同一套坐标）；
    /// 3. 上面没中就退到文件名那一格 —— 生产者把同一张图写成 `file:///…/a.png` 与
    ///    `images/a.png` 两种形态是真的会同时出现；
    /// 4. 还是没有就 `None`：那一格空着，与 HTMLZ / EPUB 对「读不到字节的那张图」同一立场。
    fn resolve(&self, reference: &str, base: &str) -> Option<usize> {
        let reference = reference.trim();
        if reference.is_empty()
            || reference.starts_with('#')
            || starts_with_scheme(reference, "data")
            || starts_with_scheme(reference, "javascript")
            || starts_with_scheme(reference, "mailto")
        {
            return None;
        }
        if let Some(cid) = strip_ci_prefix(reference, "cid:") {
            let cid = cid.trim().trim_matches(|c| c == '<' || c == '>');
            return self
                .parts
                .iter()
                .position(|part| part.cid.as_deref() == Some(cid));
        }
        let absolute = reference.contains("://") || has_drive(reference);
        let path = if absolute {
            absolute_key(reference)
        } else {
            // 相对的那一条按**引用它那一份**的目录合并（与 EPUB 的 spine 项、HTMLZ 的条目
            // 同一套坐标），而不是按主文档的目录：章节可以在子目录里。
            resolve_href(base, &percent_decode(reference))
        };
        // 从最具体的往最宽的对：完整路径 → 折过锚点的那一格 → 文件名。倒过来会在
        // 「锚点外有两张同名图」那一档上把后那一张指到前一张的字节上 —— 那是画错图，
        // 不是少一张图。
        let stripped = anchor_key(&path, &self.anchor);
        let base_name = path.rsplit('/').next().unwrap_or(&path);
        self.by_key(&path)
            .or_else(|| self.by_key(&stripped))
            .or_else(|| self.by_key(base_name))
    }

    /// 一份 HTML 带来的作者样式：`<style>` 那一块 + `<link rel="stylesheet">` 指到的部分。
    ///
    /// 「算不算样式表」用 [`crate::content::scan::is_stylesheet_rel`] 那一份（单文件 HTML /
    /// CHM / HTMLZ 同一判据）；每一**份**样式表按它**自己的**目录折 `url()` —— 折错了的表现
    /// 是「样式里明明写着这张图，就是取不到字节」。
    fn styles_of(&self, raw: &str, entry: &str) -> String {
        let dom = Dom::parse(raw.as_bytes());
        let base = dir_of(entry);
        let mut css = String::new();
        for (id, _) in dom.elements() {
            match dom.tag_name(id) {
                Some("style") => {
                    let text = dom.raw_text_of_subtree(id);
                    if !text.trim().is_empty() {
                        css.push_str(&absolutize_css_urls(&text, &base));
                        css.push('\n');
                    }
                }
                Some("link") => {
                    if !crate::content::scan::is_stylesheet_rel(
                        dom.attr(id, "rel").unwrap_or_default(),
                    ) {
                        continue;
                    }
                    let Some(href) = dom.attr(id, "href") else {
                        continue;
                    };
                    let Some(index) = self.resolve(href, &base) else {
                        continue;
                    };
                    let part = &self.parts[index];
                    let text = part.text(self);
                    css.push_str(&absolutize_css_urls(&text, &dir_of(&part.key)));
                    css.push('\n');
                }
                _ => {}
            }
        }
        css
    }

    /// 一份 HTML 里 `<img>` 指到的图，登记进那张总表（跨章去重）。
    fn register_images(&self, raw: &str, entry: &str, images: &mut Vec<ImageRes>) {
        let dom = Dom::parse(raw.as_bytes());
        let base = dir_of(entry);
        for (id, _) in dom.elements() {
            if dom.tag_name(id) != Some("img") {
                continue;
            }
            let Some(src) = dom.attr(id, "src") else {
                continue;
            };
            let Some(index) = self.resolve(src, &base) else {
                continue;
            };
            let key = self.parts[index].key.clone();
            if images.iter().any(|image| image.href == key) {
                continue;
            }
            let part = &self.parts[index];
            let Some(bytes) = part.decoded() else {
                continue;
            };
            // 声明优先、按字节头兜底；两条都不中就是解不动的格式（WMF / EMF 那一族），
            // 不登记 —— 登记成 0×0 会让排版按容器宽度硬拉，反而把版面撑坏。
            let guessed = guess_media_type(&bytes);
            let declared = (!part.media_type.is_empty()
                && part.media_type != "application/octet-stream")
                .then_some(part.media_type.as_str());
            let Some(media_type) = declared
                .filter(|mime| is_decodable_bitmap(mime))
                .or(guessed)
            else {
                continue;
            };
            let (width, height) = image_dims(&bytes);
            images.push(ImageRes {
                href: key.clone(),
                source: key,
                media_type: media_type.to_string(),
                width,
                height,
            });
        }
    }
}

/// 读文件、扫信封、定键。
fn open(path: &Path) -> Result<Archive> {
    let bytes = std::fs::read(path).io_path(path)?;
    let size = bytes.len() as u64;
    let scan = scan(&bytes);
    if !scan.envelope {
        return Err(unsupported(path, "没有 MIME 信封，不是网页存档"));
    }
    if scan.parts.is_empty() {
        return Err(unsupported(
            path,
            "信封说得出 boundary，而一个部分都切不出来",
        ));
    }
    assemble(scan, size).ok_or_else(|| unsupported(path, "切得出部分，却没有一份 `text/html`"))
}

/// 扫信封与部分。**行首**那条 `--<boundary>` 才算分隔符。
fn scan(bytes: &[u8]) -> Scan {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let lines: Vec<&[u8]> = bytes
        .split(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .collect();
    let Some(blank) = lines.iter().position(|line| line.is_empty()) else {
        return Scan::default();
    };
    let envelope = headers(&lines[..blank]);
    let content_type = value_of(&envelope, "content-type").unwrap_or_default();
    if !content_type.starts_with("multipart/") {
        return Scan::default();
    }
    let Some(boundary) = parameter(&content_type, "boundary") else {
        return Scan::default();
    };
    let separator = format!("--{boundary}");
    let mut scan = Scan {
        envelope: true,
        parts: Vec::new(),
        envelope_location: value_of(&envelope, "content-base")
            .or_else(|| value_of(&envelope, "content-location")),
        envelope_charset: value_of(&envelope, "content-type")
            .as_deref()
            .and_then(|content_type| parameter(content_type, "charset"))
            .and_then(|name| Encoding::for_label(name.as_bytes())),
    };
    let mut current: Option<Vec<&[u8]>> = None;
    for line in &lines[blank + 1..] {
        if line.starts_with(separator.as_bytes()) {
            let tail = &line[separator.len()..];
            if tail.starts_with(b"--") {
                break;
            }
            // 分隔符那一行只许在 `--boundary` 后面留空白（后面跟着别的字就不是分隔符）。
            if tail.iter().all(u8::is_ascii_whitespace) {
                if let Some(body) = current.take() {
                    push_part(&mut scan, body);
                }
                current = Some(Vec::new());
                continue;
            }
        }
        if let Some(body) = current.as_mut() {
            body.push(line);
        }
    }
    if let Some(body) = current.take() {
        push_part(&mut scan, body);
    }
    scan
}

#[derive(Default)]
struct Scan {
    /// 读得出一个 `multipart/*` 的信封（带 `boundary`）。
    envelope: bool,
    parts: Vec<Part>,
    /// 信封里的 `Content-Base` / `Content-Location`：定锚点的第一优先。
    envelope_location: Option<String>,
    envelope_charset: Option<&'static Encoding>,
}

fn push_part(scan: &mut Scan, lines: Vec<&[u8]>) {
    let cut = lines
        .iter()
        .position(|line| line.is_empty())
        .unwrap_or(lines.len());
    let head = headers(&lines[..cut]);
    let content_type = value_of(&head, "content-type").unwrap_or_default();
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or(content_type.as_str())
        .trim()
        .to_ascii_lowercase();
    let mut body = Vec::new();
    for (index, line) in lines[cut..].iter().enumerate() {
        if index > 0 {
            body.push(b'\n');
        }
        body.extend_from_slice(line);
    }
    scan.parts.push(Part {
        // 键要等锚点定了才能算（要先把所有部分的路径看一遍），这里先放来源地址。
        key: value_of(&head, "content-location").unwrap_or_default(),
        // 尖括号是头部的写法（`Content-ID: <img1>`），不是标识符的一部分：存进来就剥掉，
        // 引用那一边同样剥（`Archive::resolve`），两侧才是同一个形状。
        cid: value_of(&head, "content-id").map(|cid| {
            cid.trim()
                .trim_matches(|c| c == '<' || c == '>')
                .to_string()
        }),
        media_type,
        charset: parameter(&content_type, "charset")
            .and_then(|name| Encoding::for_label(name.as_bytes())),
        transfer: match value_of(&head, "content-transfer-encoding")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "base64" => Transfer::Base64,
            "quoted-printable" => Transfer::QuotedPrintable,
            _ => Transfer::Raw,
        },
        body,
    });
}

/// 定键并挑出 HTML 部分（主文档排第一）。`None` = 一份 HTML 都没有。
fn assemble(mut scan: Scan, size: u64) -> Option<Archive> {
    let html_like =
        |part: &Part| part.media_type == "text/html" || part.media_type == "application/xhtml+xml";
    let paths: Vec<String> = scan
        .parts
        .iter()
        .map(|part| absolute_key(&part.key))
        .collect();
    // 主文档：信封那条地址指的那一份，认不到就取文件里第一份 HTML（两派生产者都把
    // 主文档写在最前，这一格只管邮件那种「先 plain 后 html」的形状）。
    let main = scan
        .envelope_location
        .as_deref()
        .map(absolute_key)
        .and_then(|wanted| paths.iter().position(|path| *path == wanted))
        .or_else(|| scan.parts.iter().position(html_like))
        .unwrap_or(0);
    let anchor = dir_of(&paths[main]);
    let mut taken: Vec<String> = Vec::with_capacity(scan.parts.len());
    for (index, part) in scan.parts.iter_mut().enumerate() {
        // 有些部分根本没有地址（邮件里那一版纯文本、只有 `Content-ID` 的图）：给一个
        // 合成键，让「引用不到」与「没有地址」在这一张表上分开 —— 后者还能按 `cid:` 找到。
        let folded = if paths[index].is_empty() {
            format!("part-{index}")
        } else {
            anchor_key(&paths[index], &anchor)
        };
        part.key = unique_key(folded, &paths[index], &taken);
        taken.push(part.key.clone());
    }
    let mut html: Vec<usize> = scan
        .parts
        .iter()
        .enumerate()
        .filter(|(_, part)| html_like(part))
        .map(|(index, _)| index)
        .collect();
    if html.is_empty() {
        return None;
    }
    if let Some(at) = html.iter().position(|index| *index == main) {
        let main = html.remove(at);
        html.insert(0, main);
    }
    Some(Archive {
        parts: scan.parts,
        html,
        anchor: anchor.trim_end_matches('/').to_string(),
        envelope_charset: scan.envelope_charset,
        size,
    })
}

/// 一条来源地址 → 容器内的规范路径：剥 scheme + authority + 盘符、反斜杠换正斜杠、
/// 百分号解码、折 `./` 与 `../`，去掉 `?query` 与 `#fragment`。
fn absolute_key(location: &str) -> String {
    let mut text = location.trim();
    if let Some((_, after)) = text.split_once("://") {
        // authority 到第一个 `/` 为止（`example.com/docs/a.png` → `/docs/a.png`）。
        text = after.split_once('/').map_or("", |(_, path)| path);
    }
    if has_drive(text) {
        text = &text[2..];
    }
    let text = text.replace('\\', "/");
    let text = text.trim_start_matches('/');
    resolve_href("", text.split(['?', '#']).next().unwrap_or(text))
}

/// 把一条已经规范过的路径折到主文档那个目录的相对坐标上。
fn anchor_key(path: &str, anchor: &str) -> String {
    let anchor = anchor.trim_end_matches('/');
    if anchor.is_empty() {
        return path.to_string();
    }
    match path.strip_prefix(&format!("{anchor}/")) {
        Some(rest) => rest.to_string(),
        // 落在主文档目录之外（真见过：图被写进 `C:/Windows/Temp/`）：退到文件名那一格。
        None => path.rsplit('/').next().unwrap_or(path).to_string(),
    }
}

/// 撞名的那一格：**退回完整的那条路径**，还撞才加后缀。**不许静默覆盖** —— 后那一份要是
/// 顶掉了前一份，正文里那张图就指到别人的字节上，那症状是「画错图」而不是「少一张图」。
///
/// 退到完整路径而不是「补一层目录」：锚点外那两格同名的图（真见过：一张在
/// `C:/Windows/Temp/`、一张在 `C:/Users/me/Desktop/`）各自带着自己那条路径，而目录只有一层
/// 的时候「补一层」与「完整路径」是同一个东西，多几层时补的那一层反而还是可能撞。
fn unique_key(key: String, full: &str, taken: &[String]) -> String {
    if !taken.contains(&key) {
        return key;
    }
    if !full.is_empty() && !taken.iter().any(|taken| taken == full) {
        return full.to_string();
    }
    match key.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}-2.{ext}"),
        _ => format!("{key}-2"),
    }
}

/// 章内的图片引用清单：`src` 换成容器内的键（偏移仍是可见文本那一把尺）。
fn image_uses(archive: &Archive, raw: &str, entry: &str) -> Vec<ImageUse> {
    let base = dir_of(entry);
    crate::content::scan::chapter_summary(raw)
        .images
        .into_iter()
        .filter_map(|image| {
            let index = archive.resolve(&image.src, &base)?;
            Some(ImageUse {
                src: archive.parts[index].key.clone(),
                offset: image.offset,
            })
        })
        .collect()
}

/// 部分文件名那一格（书名最后的兜底）。
fn file_stem(key: &str) -> Option<String> {
    let name = key.rsplit('/').next().unwrap_or(key);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    (!stem.is_empty()).then(|| stem.to_string())
}

/// 头部行折回成 `(小写名, 值)`。
///
/// RFC 822 的折叠（以空白开头的行接在上一行后面）真会出现：IE 那条又长又带百分号的
/// `Content-Type` 经邮件客户端一转就被折断，而折断的那一半里恰好写着 `charset`。
fn headers(lines: &[&[u8]]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for raw in lines {
        let line = String::from_utf8_lossy(raw);
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = out.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        out.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    out
}

fn value_of(head: &[(String, String)], want: &str) -> Option<String> {
    head.iter()
        .find(|(name, _)| name == want)
        .map(|(_, value)| value.clone())
}

/// 取 `a=1; b="x;y"` 那种参数表里的一个值：**引号里的分号不算分隔**（boundary 真能含分号）。
fn parameter(text: &str, want: &str) -> Option<String> {
    let mut rest = text;
    loop {
        let (head, tail) = match split_on_unquoted_semicolon(rest) {
            Some(cut) => cut,
            // 最后一段没有尾随的分号。
            None => (rest, ""),
        };
        if let Some((name, value)) = head.split_once('=')
            && name.trim().eq_ignore_ascii_case(want)
        {
            let value = value.trim();
            let value = value.strip_prefix('"').unwrap_or(value);
            return Some(value.strip_suffix('"').unwrap_or(value).to_string());
        }
        if tail.is_empty() {
            return None;
        }
        rest = tail;
    }
}

fn split_on_unquoted_semicolon(text: &str) -> Option<(&str, &str)> {
    let mut quoted = false;
    for (index, ch) in text.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            ';' if !quoted => return Some((&text[..index], &text[index + 1..])),
            _ => {}
        }
    }
    None
}

/// quoted-printable：`=XX` 折回字节、行尾那个 `=` 是软换行（连换行一起跳掉）、其余原样。
///
/// `_` **不**折成空格：那是 RFC 2047 的 encoded-word 才有的写法，RFC 2557 的部分体用的是
/// RFC 2045 的 QP，那里 `_` 就是下划线。认不出的 `=zz` 也原样留着，不猜作者想写什么。
fn quoted_printable(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'=' {
            out.push(bytes[index]);
            index += 1;
            continue;
        }
        // 软换行。
        if bytes.get(index + 1) == Some(&b'\n') {
            index += 2;
            continue;
        }
        let pair = bytes.get(index + 1..index + 3);
        let byte = pair
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|text| u8::from_str_radix(text, 16).ok());
        match byte {
            Some(byte) => {
                out.push(byte);
                index += 3;
            }
            None => {
                out.push(b'=');
                index += 1;
            }
        }
    }
    out
}

fn starts_with_scheme(text: &str, scheme: &str) -> bool {
    text.get(..scheme.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
        && text.get(scheme.len()..scheme.len() + 1) == Some(":")
}

fn strip_ci_prefix<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &text[prefix.len()..])
}

/// `file:///C:/…` 与 `C:\…` 那种带盘符的形态。
fn has_drive(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
}

/// 判定层那一问：这一份是不是一个读得开的网页存档。
pub(crate) enum Probe {
    /// 没有信封，或有信封而里面一份 HTML 都没有（一封纯文本邮件就是这一档）。
    NotMhtml,
    /// 信封说得出 `boundary` 而一个部分都切不出来：截断，或边界与实际不符。
    Corrupt,
    /// 一个读得开的网页存档。
    Book,
}

/// 判据与 provider 用的是**同一份** [`scan`]：两处各判一次迟早会在同一个文件上分叉，
/// 而分叉的症状是「判定收下了、provider 说不是」那种说错的话。
pub(crate) fn probe(path: &Path) -> Probe {
    let Ok(bytes) = std::fs::read(path) else {
        return Probe::NotMhtml;
    };
    let scan = scan(&bytes);
    if !scan.envelope {
        return Probe::NotMhtml;
    }
    if scan.parts.is_empty() {
        return Probe::Corrupt;
    }
    if scan
        .parts
        .iter()
        .any(|part| part.media_type == "text/html" || part.media_type == "application/xhtml+xml")
    {
        Probe::Book
    } else {
        Probe::NotMhtml
    }
}

fn unsupported(path: &Path, why: &str) -> crate::Error {
    crate::Error::UnsupportedFormat(format!(
        "{}：{why}",
        path.file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([9, 8, 7, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    fn b64(bytes: &[u8]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// 每一行都按 CRLF 折：IE / Word 存的 `.mht` 就是这样，而 [`scan`] 是按行切部分的。
    fn write_mht(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body.replace('\n', "\r\n")).unwrap();
        path
    }

    /// IE / Word 那一派的形状：`----=_NextPart_` 边界、quoted-printable 的正文、
    /// `file:///C:/…` 的 `Content-Location`，而正文里的引用是**相对**路径。
    fn ie(dir: &Path, bytes: &[u8]) -> PathBuf {
        write_mht(
            dir,
            "存档.mht",
            &format!(
                r#"MIME-Version: 1.0
Content-Type: multipart/related; type="text/html"; boundary=----=_NextPart_000_0CFB
Content-Location: file:///C:/Users/me/Desktop/%E9%A1%B5.htm

------=_NextPart_000_0CFB
Content-Location: file:///C:/Users/me/Desktop/页.htm
Content-Transfer-Encoding: quoted-printable
Content-Type: text/html; charset="utf-8"

<html><head><title>报告</title><style>p {{ text-indent: 2em=
 }}</style><link rel="stylesheet" href="页_files/a.css"></head>
<body><h1>第一章</h1><p>正文=20与=3D号</p><img src="页_files/image001.png"></body></html>

------=_NextPart_000_0CFB
Content-Location: file:///C:/Users/me/Desktop/页_files/a.css
Content-Transfer-Encoding: 8bit
Content-Type: text/css; charset="utf-8"

h1 {{ font-weight: 900 }}
------=_NextPart_000_0CFB
Content-Location: file:///C:/Users/me/Desktop/页_files/image001.png
Content-Transfer-Encoding: base64
Content-Type: image/png

{b}

------=_NextPart_000_0CFB--
"#,
                b = b64(bytes)
            ),
        )
    }

    /// Chrome 那一派：`----=_mhtml_` 边界、`8bit` 的正文、原来那个 `https://…` 当地址，
    /// 而正文里的引用是**绝对** URL —— 同一张图，两派写出来的地址完全不同。
    fn chrome(dir: &Path, bytes: &[u8]) -> PathBuf {
        write_mht(
            dir,
            "网页.mhtml",
            &format!(
                r#"MIME-Version: 1.0
Content-Type: multipart/related; boundary="----=_mhtml_1j000000"

------=_mhtml_1j000000
Content-Location: https://example.com/docs/page.html
Content-Type: text/html; charset=utf-8
Content-Transfer-Encoding: 8bit

<html><head><title>网页</title></head><body><h1>第一节</h1><p>正文</p><img src="https://example.com/docs/img/a.png"></body></html>
------=_mhtml_1j000000
Content-Location: https://example.com/docs/img/a.png
Content-Type: image/png
Content-Transfer-Encoding: base64

{b}
------=_mhtml_1j000000--
"#,
                b = b64(bytes)
            ),
        )
    }

    /// 走 `read` 那一路：`index` 交回的章只带**位置**（`source()` 按契约就是空串，
    /// 正文出库那条），而这些门要打在正文上。
    fn reflow(path: &Path) -> (DocMeta, FlowDoc) {
        let (meta, content) = MhtmlAdapter
            .read(path)
            .unwrap_or_else(|err| panic!("{} 读不出来：{err}", path.display()));
        let Content::Reflow(flow) = content else {
            panic!("网页存档该读成可重排");
        };
        (meta, flow)
    }

    /// IE 那一派走完全程：书名、章、quoted-printable 的三个坑（`=20` 空格、`=3D` 等号、
    /// 行尾那个 `=` 是软换行不是字符）、外链样式表那一份部分、以及按相对路径找到那张图。
    #[test]
    fn an_ie_save_decodes_its_parts_and_finds_its_image() {
        let dir = tempfile::tempdir().unwrap();
        let png = png(20, 10);
        let path = ie(dir.path(), &png);
        let (meta, flow) = reflow(&path);
        assert_eq!(meta.format, Format::Mhtml);
        assert_eq!(meta.title, "报告", "书名从主文档那一份的 <title> 来");
        assert_eq!(flow.chapters.len(), 1);
        assert_eq!(flow.chapters[0].title, "报告");
        assert_eq!(
            flow.chapters[0].spine_href, "页.htm",
            "键是折到主文档目录的相对坐标"
        );
        let body = flow.chapters[0].source();
        assert!(body.contains("正文 与=号"), "QP 的三种写法都要落地：{body}");
        assert!(!body.contains("=20"), "转义不许留在正文里：{body}");
        // 样式：内联那块（软换行接回来的）+ `<link>` 指到的那一份部分。
        assert!(
            flow.css.contains("text-indent: 2em }"),
            "QP 软换行折回一行：{}",
            flow.css
        );
        assert!(
            flow.css.contains("font-weight: 900"),
            "<link> 的那一份样式表在部分里，也要进层叠：{}",
            flow.css
        );
        // 图：正文里写的是相对路径，容器里的地址是绝对路径 —— 两侧必须落在同一个键上。
        assert_eq!(flow.images.len(), 1);
        assert_eq!(flow.images[0].href, "页_files/image001.png");
        assert_eq!(flow.images[0].media_type, "image/png");
        assert_eq!((flow.images[0].width, flow.images[0].height), (20, 10));
        // 章内那一列按落库那一路问（见上面那条注释）：`src` 已经从相对引用折成容器内的键。
        let (_, content) = MhtmlAdapter.index(&path).unwrap();
        let Content::Reflow(indexed) = content else {
            panic!("该读成可重排");
        };
        assert_eq!(indexed.chapters[0].images.len(), 1, "章内清单也按同一个键");
        assert_eq!(
            indexed.chapters[0].images[0].src, "页_files/image001.png",
            "相对引用要折成容器内的键：{:?}",
            indexed.chapters[0].images
        );
        assert_eq!(
            MhtmlAdapter
                .image_bytes(&path, "页_files/image001.png")
                .unwrap(),
            png
        );
        assert!(
            MhtmlAdapter
                .entry_text(&path, "页.htm")
                .is_some_and(|text| text.contains("<h1>第一章</h1>")),
            "按部分键现取一章"
        );
    }

    /// Chrome 那一派：正文里写的是绝对 URL，而部分自己的地址也是绝对 URL —— 折过锚点之后
    /// 与 IE 那一派落在**同一张表**上（键都是相对主文档目录的那一格）。
    #[test]
    fn a_chrome_save_resolves_its_absolute_reference_to_the_same_kind_of_key() {
        let dir = tempfile::tempdir().unwrap();
        let png = png(8, 6);
        let path = chrome(dir.path(), &png);
        let (meta, flow) = reflow(&path);
        assert_eq!(meta.title, "网页");
        assert_eq!(flow.chapters[0].spine_href, "page.html");
        assert_eq!(
            flow.images.len(),
            1,
            "绝对 URL 要指到那一份上：{:?}",
            flow.images
        );
        assert_eq!(flow.images[0].href, "img/a.png");
        assert_eq!(MhtmlAdapter.image_bytes(&path, "img/a.png").unwrap(), png);
        // 引擎拿到的引用常常还是原文里那一串绝对地址（CSS 的 `url()` 那条路不改写远程形态）：
        // 按原样问也要能取到字节。
        assert_eq!(
            MhtmlAdapter
                .image_bytes(&path, "https://example.com/docs/img/a.png")
                .unwrap(),
            png
        );
    }

    /// `Content-ID` 那一族引用（邮件转出来的存档里唯一稳定的地址就是它）。
    #[test]
    fn a_cid_reference_finds_the_part_that_carries_that_id() {
        let dir = tempfile::tempdir().unwrap();
        let png = png(4, 4);
        let path = write_mht(
            dir.path(),
            "邮件.mht",
            &format!(
                r#"MIME-Version: 1.0
Content-Type: multipart/related; boundary=x

--x
Content-Location: file:///a/doc.htm
Content-Type: text/html

<html><body><p>文</p><img src="cid:img1"></body></html>
--x
Content-ID: <img1>
Content-Type: image/png
Content-Transfer-Encoding: base64

{b}
--x--
"#,
                b = b64(&png)
            ),
        );
        let (_, flow) = reflow(&path);
        assert_eq!(flow.images.len(), 1, "cid 要认出来：{:?}", flow.images);
        assert_eq!(flow.images[0].href, "part-1", "没有地址的那一份用合成键");
        assert_eq!(MhtmlAdapter.image_bytes(&path, "part-1").unwrap(), png);
    }

    /// 部分头里那条 `charset=gb2312` 必须生效：中文网页另存出来的 `.mht` 满地是这一形态，
    /// 而按 UTF-8 收的症状是**一排问号且不报错**（与单文件 HTML 那一坑同一件事）。
    #[test]
    fn a_part_declared_gbk_is_decoded_from_its_own_header() {
        let dir = tempfile::tempdir().unwrap();
        let html =
            "<html><head><title>书名</title></head><body><p>大雪封山的那一夜</p></body></html>";
        let (gbk, _, _) = encoding_rs::GBK.encode(html);
        let mut bytes =
            b"MIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=x\r\n\r\n".to_vec();
        bytes.extend_from_slice(b"--x\r\nContent-Type: text/html; charset=gb2312\r\nContent-Transfer-Encoding: 8bit\r\n\r\n");
        bytes.extend_from_slice(&gbk);
        bytes.extend_from_slice(b"\r\n--x--\r\n");
        let path = dir.path().join("gbk.mht");
        std::fs::write(&path, &bytes).unwrap();
        let (meta, flow) = reflow(&path);
        assert_eq!(meta.title, "书名", "书名要从 GBK 的那一段解出来");
        let body = &flow.chapters[0].source();
        assert!(body.contains("大雪封山"), "正文不能是一排问号：{body}");
        assert!(!body.contains('\u{fffd}'), "解得通就不该留洞：{body}");
    }

    /// 头部被折断了（RFC 822 的续行）也要认得出 `charset`：那一半恰好写在被折断的地方。
    #[test]
    fn a_folded_header_line_keeps_its_parameter() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_mht(
            dir.path(),
            "折断.mht",
            "MIME-Version: 1.0
Content-Type: multipart/related; boundary=x

--x
Content-Type: text/html; charset=\n gb2312
Content-Transfer-Encoding: 8bit

<html><body><p>文</p></body></html>
--x--
",
        );
        let archive = open(&path).unwrap();
        let part = &archive.parts[archive.html[0]];
        assert_eq!(part.charset, Some(encoding_rs::GBK));
    }

    /// 锚点外两张同名的图不许互相顶掉：**先按完整路径对**，再退到折过锚点的那一格，
    /// 最后才退到文件名。反过来会在后那一张上指到前一张的字节（那是画错图）。
    #[test]
    fn two_out_of_anchor_parts_with_the_same_name_do_not_shadow_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let first = png(3, 3);
        let second = png(7, 7);
        let path = write_mht(
            dir.path(),
            "同名.mht",
            &format!(
                r#"MIME-Version: 1.0
Content-Type: multipart/related; boundary=x

--x
Content-Location: file:///C:/Users/me/Desktop/页.htm
Content-Type: text/html
Content-Transfer-Encoding: 8bit

<html><body><p>文</p></body></html>
--x
Content-Location: file:///C:/Windows/Temp/a.png
Content-Type: image/png
Content-Transfer-Encoding: base64

{a}
--x
Content-Location: file:///C:/Temp/a.png
Content-Type: image/png
Content-Transfer-Encoding: base64

{b}
--x--
"#,
                a = b64(&first),
                b = b64(&second)
            ),
        );
        let archive = open(&path).unwrap();
        let keys: Vec<&str> = archive.parts.iter().map(|part| part.key.as_str()).collect();
        assert_eq!(
            keys,
            ["页.htm", "a.png", "Temp/a.png"],
            "撞名的退到完整路径：{keys:?}"
        );
        assert_eq!(
            archive
                .resolve("file:///C:/Temp/a.png", "")
                .and_then(|ix| archive.parts[ix].decoded()),
            Some(second),
            "后那一张要指到自己的字节上"
        );
        assert_eq!(
            archive
                .resolve("file:///C:/Windows/Temp/a.png", "")
                .and_then(|ix| archive.parts[ix].decoded()),
            Some(first)
        );
    }

    /// 邮件那种「先 plain 后 html」的形状：主文档那一格要排到最前，plain 那一版不当章。
    #[test]
    fn the_html_story_leads_even_when_a_plain_one_is_written_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_mht(
            dir.path(),
            "邮件.mht",
            "MIME-Version: 1.0
Content-Type: multipart/related; boundary=x

--x
Content-Location: file:///a/plain.txt
Content-Type: text/plain
Content-Transfer-Encoding: 8bit

纯文本的那一版
--x
Content-Location: file:///a/doc.htm
Content-Type: text/html
Content-Transfer-Encoding: 8bit

<html><head><title>正文版</title></head><body><p>文</p></body></html>
--x--
",
        );
        let (meta, flow) = reflow(&path);
        assert_eq!(meta.title, "正文版", "书名要认 HTML 那一版");
        assert_eq!(flow.chapters.len(), 1, "plain 那一版不算一章");
        assert_eq!(flow.chapters[0].spine_href, "doc.htm");
    }

    /// 多份 HTML 就是多章，按文件里的序（与 HTMLZ「每份 HTML 一章」同一条口径）。
    #[test]
    fn several_html_parts_are_several_chapters_in_file_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_mht(
            dir.path(),
            "两章.mht",
            "MIME-Version: 1.0
Content-Type: multipart/related; boundary=x

--x
Content-Location: file:///a/one.htm
Content-Type: text/html
Content-Transfer-Encoding: 8bit

<html><body><h1>第一章</h1><p>一</p></body></html>
--x
Content-Location: file:///a/two.htm
Content-Type: text/html
Content-Transfer-Encoding: 8bit

<html><body><h1>第二章</h1><p>二</p></body></html>
--x--
",
        );
        let (_, flow) = reflow(&path);
        assert_eq!(flow.chapters.len(), 2);
        assert_eq!(flow.chapters[0].title, "第一章");
        assert_eq!(flow.chapters[1].title, "第二章");
        let texts = MhtmlAdapter.read_texts(&path).unwrap();
        assert_eq!(texts.len(), 2, "`read_texts` 与章表同长");
        assert!(texts[1].contains("二"));
    }

    /// 引用不到任何部分的 `<img>`：**不登记**，但正文里那一格留着（删掉它会让这一章的
    /// 可见文本与锚点一起挪位，那比画不出这张图难查得多）。
    #[test]
    fn a_reference_that_points_at_nothing_is_not_registered_but_stays_in_the_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_mht(
            dir.path(),
            "缺图.mht",
            r#"MIME-Version: 1.0
Content-Type: multipart/related; boundary=x

--x
Content-Location: file:///a/doc.htm
Content-Type: text/html
Content-Transfer-Encoding: 8bit

<html><body><p>文</p><img src="nope.png"><img src="data:image/png;base64,AA"></body></html>
--x--
"#,
        );
        let (_, flow) = reflow(&path);
        assert!(flow.images.is_empty(), "对不上部分的与 `data:` 都不登记");
        let body = flow.chapters[0].source();
        assert!(body.contains("nope.png"), "正文原样透传：{body}");
        // 章内那一列要按**落库那一路**问（`index` 那一趟才是进 `chapters.images_json` 的）：
        // trait 默认的 `read()` 给 chapter.images 填的是未过滤的原始扫描，它只服务测试。
        let (_, content) = MhtmlAdapter.index(&path).unwrap();
        let Content::Reflow(indexed) = content else {
            panic!("该读成可重排");
        };
        assert!(
            indexed.chapters[0].images.is_empty(),
            "引用不到部分的图不许进那一列：{:?}",
            indexed.chapters[0].images
        );
    }

    /// 解不动的格式（WMF 那一族）不登记成 0×0。
    #[test]
    fn an_undecodable_bitmap_is_not_registered() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_mht(
            dir.path(),
            "wmf.mht",
            r#"MIME-Version: 1.0
Content-Type: multipart/related; boundary=x

--x
Content-Location: file:///a/doc.htm
Content-Type: text/html
Content-Transfer-Encoding: 8bit

<html><body><img src="a.wmf"></body></html>
--x
Content-Location: file:///a/a.wmf
Content-Type: image/x-wmf
Content-Transfer-Encoding: 8bit

（这里是一段 WMF）
--x--
"#,
        );
        let (_, flow) = reflow(&path);
        assert!(
            flow.images.is_empty(),
            "登记成 0×0 会让排版按容器宽度硬拉：{:?}",
            flow.images
        );
    }

    /// 三种负形状：穿着 `.mht` 的裸文本、信封说得出而切不出部分、有信封有部分但没有 HTML。
    #[test]
    fn the_three_ways_a_mht_is_not_a_book_are_three_different_words() {
        let dir = tempfile::tempdir().unwrap();

        let bare = dir.path().join("假.mht");
        std::fs::write(&bare, "其实是一段纯文本\n").unwrap();
        assert_eq!(probe(&bare) as u8, Probe::NotMhtml as u8, "没有信封");
        assert!(MhtmlAdapter.index(&bare).is_err());

        let empty = dir.path().join("空壳.mht");
        std::fs::write(
            &empty,
            "MIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=x\r\n\r\n再没有别的东西了\r\n",
        )
        .unwrap();
        assert_eq!(
            probe(&empty) as u8,
            Probe::Corrupt as u8,
            "边界说得出而切不出部分"
        );
        assert!(MhtmlAdapter.index(&empty).is_err());

        let no_html = dir.path().join("邮件.mht");
        std::fs::write(
            &no_html,
            "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: text/plain\r\n\r\n正文\r\n--x--\r\n",
        )
        .unwrap();
        assert_eq!(
            probe(&no_html) as u8,
            Probe::NotMhtml as u8,
            "一份 HTML 都没有"
        );
        assert!(MhtmlAdapter.index(&no_html).is_err());
    }

    /// 判定层与 provider 共用同一份 `scan`：同一个文件两边不许说出相反的话。
    #[test]
    fn the_probe_and_the_provider_agree_on_every_shape_we_can_make() {
        let dir = tempfile::tempdir().unwrap();
        let png = png(6, 6);
        for path in [ie(dir.path(), &png), chrome(dir.path(), &png)] {
            assert!(matches!(probe(&path), Probe::Book), "{}", path.display());
            assert!(MhtmlAdapter.index(&path).is_ok(), "{}", path.display());
        }
    }

    /// 分隔符是**行首**那条线：正文里出现同一串字面量不算分隔符（真文件里见过有人把
    /// 边界串写进正文的代码块）。
    #[test]
    fn a_boundary_written_in_the_body_does_not_split_the_part() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_mht(
            dir.path(),
            "正文里有边界串.mht",
            "MIME-Version: 1.0
Content-Type: multipart/related; boundary=x

--x
Content-Location: file:///a/doc.htm
Content-Type: text/html
Content-Transfer-Encoding: 8bit

<html><body><p>这一行写着 --x 而它不是分隔符</p></body></html>
--x--
",
        );
        let (_, flow) = reflow(&path);
        assert_eq!(flow.chapters.len(), 1);
        assert!(
            flow.chapters[0].source().contains("--x"),
            "正文原样：{}",
            flow.chapters[0].source()
        );
    }

    #[test]
    fn quoted_printable_folds_hex_escapes_and_soft_breaks() {
        // `=20` 空格、`=3D` 等号本身、行尾的 `=` 连换行一起跳掉、认不出的 `=zz` 原样留着。
        assert_eq!(
            quoted_printable(b"ab=20cd=3Def=\ngh=i"),
            b"ab cd=efgh=i".to_vec()
        );
    }

    #[test]
    fn absolute_key_lands_every_producer_on_one_coordinate() {
        assert_eq!(
            absolute_key("file:///C:/Users/me/Desktop/a.png"),
            "Users/me/Desktop/a.png"
        );
        assert_eq!(absolute_key("C:\\Users\\me\\a.png"), "Users/me/a.png");
        assert_eq!(
            absolute_key("https://example.com/docs/two.html?x=1#frag"),
            "docs/two.html"
        );
        assert_eq!(absolute_key("sub/../../a.png"), "a.png");
        assert_eq!(
            absolute_key("file:///a/%E9%A1%B5.htm"),
            "a/页.htm",
            "百分号编码的中文要解得开"
        );
        assert_eq!(absolute_key(""), "");
    }

    #[test]
    fn a_parameter_inside_quotes_keeps_its_semicolon() {
        assert_eq!(
            parameter(
                r#"multipart/related; boundary="a;b"; type="text/html""#,
                "boundary"
            ),
            Some("a;b".to_string())
        );
        assert_eq!(
            parameter("text/html; charset=UTF-8", "charset"),
            Some("UTF-8".to_string())
        );
        assert_eq!(parameter("text/html", "charset"), None);
    }
}
