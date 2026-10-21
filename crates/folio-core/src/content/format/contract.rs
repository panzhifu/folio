//! **引擎输入契约**：`Chapter::source` 永远是引擎就绪的 XHTML。
//!
//! 这一层以前是破的 —— EPUB / FB2 / HTML 的章源是 XHTML，Markdown / TXT 是规范化
//! 纯文本，于是界面必须知道「哪个格式还要包一层」（`Format::source_is_xhtml` +
//! `chapter_xhtml`），`Format` 就这样渗进了排版链。包装下沉到 provider 之后，
//! 这条契约成为 provider 的**唯一输出约定**，界面与引擎都不再问格式。
//!
//! 这里用真实 provider 跑（不是构造 `Chapter`），因为要钉住的正是「provider 有没有
//! 真的守约」。五类源都覆盖：本来就是标记的（HTML / FB2 / EPUB）、没有标记只需补一层
//! 包装的（TXT）、把 Markdown 的 `**` 与 `- ` **解析成**结构的（Markdown）、把 RTF 的
//! 格式指令**翻译成**标记的（RTF），以及不进引擎的定版式
//! （CBZ 的页是图源、PDF 的页要栅格化 —— 两族成员各一条门）。

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::content::anchor::TextMap;
use crate::content::dom::Dom;
use crate::content::format::{Content, Format, provider_for};

/// 一段 XHTML 的可见文本。契约里真正要保的是**内容**，不是标记 —— 断言只能看它。
fn visible(xhtml: &str) -> String {
    let dom = Dom::parse(xhtml.as_bytes());
    let body = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map_or(dom.document, |(id, _)| id);
    TextMap::build(&dom, body).text().to_string()
}

fn without_whitespace(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

/// EPUB 用仓库里那份合成书（`tests/fixtures/oss/minimal-zh.epub`），
/// 它是唯一一个「真容器」样本，其余格式临时造一份就够。
fn epub_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/minimal-zh.epub")
}

fn fb2(dir: &Path) -> PathBuf {
    write(
        dir,
        "a.fb2",
        r#"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">
<description><title-info><book-title>契约</book-title></title-info></description>
<body><section><title><p>第一章</p></title><p>正文</p></section></body>
</FictionBook>"#,
    )
}

/// RTF 用真书里那一形态：`\ansicpg936` + GBK 双字节的 `\'hh`，标题走 `\stylesheet`，
/// 外加一个不带 `*` 的 `{\colortbl …}`（里面是分号裸文本，漏出来就是正文前多一串分号）。
fn rtf(dir: &Path) -> PathBuf {
    write(
        dir,
        "a.rtf",
        r"{\rtf1\ansi\ansicpg936\deff0
           {\stylesheet{\s17\snext0 heading 1;}}
           {\colortbl ;\red0\green0\blue0;}
           {\pard\s17\fs28 \'b5\'da\'d2\'bb\'d5\'c2\par}
           \pard\fs21\'d6\'d0\'ce\'c8\par}",
    )
}


fn pdb(dir: &Path) -> PathBuf {
    const HEADER: usize = 78;
    let path = dir.join("a.pdb");
    let header: [u8; 16] = [0, 1, 0, 0, 0, 0, 0, 4, 0, 1, 16, 0, 0, 0, 0, 0];
    let body = b"text";
    let records: [&[u8]; 2] = [&header, body];
    let mut file = std::fs::File::create(&path).unwrap();
    let mut head = vec![0u8; HEADER];
    head[60..64].copy_from_slice(b"TEXt");
    head[64..68].copy_from_slice(b"REAd");
    head[76..78].copy_from_slice(&(records.len() as u16).to_be_bytes());
    file.write_all(&head).unwrap();
    let mut start = HEADER + records.len() * 8;
    let mut table = Vec::new();
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

/// 可重排的每一种 provider：产物必须能直接进引擎。
///
/// 断言是「能解析 + body 里有可见文本」，不是「标签在白名单里」—— 引擎对不认识的
/// 标签本来就有 block 降级（`css::ua`），把清单写死会把合法的新标签误判成违约，
/// 而这条契约真正要保的是**引擎拿到的东西是标记、且内容没丢**。
#[test]
fn every_reflow_provider_hands_the_engine_parseable_xhtml() {
    let dir = tempfile::tempdir().unwrap();
    let cases: Vec<(&str, PathBuf)> = vec![
        (
            "markdown",
            write(
                dir.path(),
                "a.md",
                "# 第一章\n\n正文一\n\n## 第二章\n\n正文二\n",
            ),
        ),
        ("text", write(dir.path(), "a.txt", "第一段\n\n第二段\n")),
        (
            "html",
            write(
                dir.path(),
                "a.html",
                "<html><body><h1>标题</h1><p>正文</p></body></html>",
            ),
        ),
        ("fb2", fb2(dir.path())),
        ("rtf", rtf(dir.path())),
        ("epub", epub_fixture()),
        ("pdb", pdb(dir.path())),
    ];

    for (name, path) in cases {
        let provider = provider_for(&path).unwrap_or_else(|| panic!("{name} 该有 provider"));
        let (_, content) = provider
            .read(&path)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let Content::Reflow(flow) = content else {
            panic!("{name} 该读成可重排");
        };
        assert!(
            provider.format().as_str() == name,
            "{name} 的 format 对不上"
        );
        assert!(!flow.chapters.is_empty(), "{name} 一章都没有");
        for (ix, chapter) in flow.chapters.iter().enumerate() {
            assert!(
                chapter.source().contains('<'),
                "{name} 第 {ix} 章的 source 不是标记：{:?}",
                &chapter.source()[..chapter.source().len().min(120)]
            );
            let text = visible(chapter.source());
            assert!(
                !text.trim().is_empty(),
                "{name} 第 {ix} 章解析出来没有可见文本"
            );
        }
    }
}

/// 纯文本那一支的核心不变量：**包装不增删一个可见字符**。
///
/// 这条断了，读者的进度与批注坐标会整体漂移 —— 而屏幕上什么都看不出来。
///
/// 这一条**只管 `.txt`**：Markdown 那一支的包装一定要删字（`**` 那四个字符不是给人看的
/// 正文），它自己的那份在 [`the_markdown_wrapper_only_removes_markers`]。硬把两族并到
/// 一条断言里，就得让期望值替 Markdown 把所有标记都剥一遍 —— 那等于在测试里再写一个
/// Markdown 解析器，而它坏了的时候正好照不出解析器坏在哪。
#[test]
fn the_plain_text_wrapper_preserves_visible_text() {
    let dir = tempfile::tempdir().unwrap();
    for (name, body) in [
        ("a.txt", "第一段\n\n第二段\n"),
        // 规则表那一族会把命中的标记行改写成 `# 章名` 再交给包装，所以 `#` 这一族也要
        // 按这条算 —— 它正是「允许少几个字符」的那唯一一处。
        (
            "a.txt",
            "# 第一章\n\n正文一\n第二行\n\n## 第二章\n\n正文二\n",
        ),
    ] {
        let path = write(dir.path(), name, body);
        let provider = provider_for(&path).unwrap();
        let (_, content) = provider.read(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("{name} 该读成可重排");
        };
        let joined: String = flow.chapters.iter().map(|c| visible(c.source())).collect();
        let expected = super::xhtml::atx_markers_removed(body);
        assert_eq!(
            without_whitespace(&joined),
            without_whitespace(&expected),
            "{name} 的包装改了内容"
        );
    }
}

/// Markdown 那一支的不变量与纯文本那条**方向不同**：标记一定要删，但一个字符都不许造，
/// 而「长得像标记的字」在代码块与转义下来的原始 HTML 里必须一字不差地留着。
///
/// 三条各钉住一种会**静默**坏掉的实现：
///
/// - **不造字**：渲染出的可见文本（去空白）必须是源文本（去空白）的**子序列**。给空 alt
///   的图片补「图：」那类占位、把脚注标签塞进正文、alt 为空时替读者编一句话 —— 全在这里红。
///   用子序列而不是等号，是因为「删掉多少标记」这件事本身就等于再写一个解析器。
/// - **标记不许残留**：`**`、`~~`、反引号、`[文字](链接)` 那对括号、列表项前的 `-`，
///   渲染后都该不见了。这一条防的是「包装根本没换成结构」。
/// - **像标记的字不许被吃掉**：围栏代码里的 `# 注释` 与 `int *p` 还在，作者手写的
///   `<i>` 还带着尖括号。这一条防的是「拿全局字符串替换冒充解析」—— 那种实现会把代码
///   清单一起洗掉，而屏上正好看不出少了几个星号。
/// - 外加一条本族特有的：**图片不许发 `<img>`**（单文件格式取不到任何字节），alt 出文字、
///   没有 alt 就整条不见，而 `chapter_summary` 扫出来的图片清单必须是空的 —— 那一列
///   进了库就是图片查看器里一张永远点不开的图。
#[test]
fn the_markdown_wrapper_only_removes_markers() {
    let source = "# 书名\n\n这是**重要**的一句，还有~~删掉~~与`码`，外加[一张链接](https://example.com/a)。\n\n```\n# 注释不是标题\nint *p = 0; // **\n```\n\n> 引用一行\n\n- 甲项\n- 乙项\n\n作者手写 <i>venus</i> 在这里是字面。\n\n![插图说明](./assets/a.png)\n\n![](no-alt.png)\n\n| 甲 | 乙 |\n|---|---|\n";
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "d.md", source);
    let provider = provider_for(&path).unwrap();
    let (_, content) = provider.read(&path).unwrap();
    let Content::Reflow(flow) = content else {
        panic!("该读成可重排");
    };
    assert_eq!(flow.chapters.len(), 1, "只有一个一级标题，就只该有一章");
    let xhtml = flow.chapters[0].source();
    let text = visible(xhtml);
    let rendered = without_whitespace(&text);
    let original = without_whitespace(source);

    // 一、不造字。逐字符在下标数组上走，不用 `&str[cursor..]`：`position` 数的是
    // **字符**而切片下标要的是**字节**，中文文本上把两者混起来会当场 panic。
    let original_chars: Vec<char> = original.chars().collect();
    let mut cursor = 0usize;
    for ch in rendered.chars() {
        let offset = original_chars[cursor..]
            .iter()
            .position(|&candidate| candidate == ch)
            .unwrap_or_else(|| panic!("渲染出的 {ch:?} 在源文本里根本没有：{text}"));
        cursor += offset + 1;
    }

    // 二、标记不残留。断「连起来的那句字」而不是断「没有星号」：代码块里本来就合法地
    // 有星号，后者会把这一条变成一条永远不成立的死断言。
    assert!(
        rendered.contains("这是重要的一句"),
        "粗体标记没被吃掉：{text}"
    );
    assert!(
        rendered.contains("还有删掉与码"),
        "删除线与行内码的标记没被吃掉：{text}"
    );
    assert!(
        rendered.contains("外加一张链接"),
        "链接那对方括号没被吃掉：{text}"
    );
    assert!(
        !rendered.contains("- 甲项"),
        "列表项前面那个短横该由排版来画：{text}"
    );
    assert!(!rendered.contains("# 书名"), "标题的井号该不见了：{text}");

    // 三、像标记的字不许被吃。
    assert!(
        text.contains("# 注释不是标题") && text.contains("int *p = 0; // **"),
        "围栏代码里的 `#` 与 `**` 是正文，不是标记：{text}"
    );
    assert!(
        text.contains("<i>venus</i>"),
        "作者手写的 HTML 按字面收，尖括号要还在：{text}"
    );

    // 表格那一档没开（引擎的 UA 表认不出 table，未认标签会被拆成一竖列字块），
    // 所以那几行该原样是一段带竖线的字。有人把 `gfm_table` 打开，这一条就红着提醒他
    // 先去补 layout 的行盒。
    assert!(
        text.contains("| 甲 | 乙 |"),
        "表格这一版**不该**渲成结构：{text}"
    );

    // 四、图片。
    assert!(
        !xhtml.contains("<img"),
        "单文件格式拿不到字节，不许发 <img>：{xhtml}"
    );
    assert!(
        text.contains("插图说明"),
        "有 alt 的图该把 alt 出成正文：{text}"
    );
    assert!(
        !text.contains("no-alt"),
        "没有 alt 的整条消失，但 alt 之外不许造字：{text}"
    );
    assert!(
        crate::content::scan::chapter_summary(xhtml)
            .images
            .is_empty(),
        "图片清单里不许有这条永远点不开的 src"
    );
}

/// 定版式走另一族：产出的是页表，不是章源。它压根不该有 `Chapter`。
#[test]
fn the_fixed_family_hands_pages_not_chapters() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.cbz");
    let file = std::fs::File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    use std::io::Write as _;
    for (name, w, h) in [("1.png", 4u32, 6u32), ("10.png", 4, 6)] {
        zip.start_file(name, opts).unwrap();
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([1, 2, 3, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        zip.write_all(&bytes).unwrap();
    }
    zip.finish().unwrap();

    let provider = provider_for(&path).unwrap();
    assert_eq!(provider.format(), Format::Cbz);
    let (_, content) = provider.read(&path).unwrap();
    let Content::Fixed(fixed) = content else {
        panic!("CBZ 该读成定版式");
    };
    assert_eq!(fixed.page_count(), 2);
    for page in &fixed.pages {
        assert!(page.image_href().is_some(), "CBZ 的页都是图源");
    }
}

/// 定版式这一族的第二个成员：PDF 的一页**不是容器里的一条图**，而是「要算出来的一张图」。
///
/// 这一条钉的是那一刀切在哪个位置：`PageSource::Render` 没有 `href`，所以它不该出现在
/// 图片表里（进了库就是图片查看器中一张永远点不开的图），而它的字节只能问
/// [`DocumentProvider::render_page`]。契约测试跑真 provider 而不是构造 `FixedPage`，
/// 要钉住的正是「provider 有没有真的按这个形状交出来」。
///
/// 样本是 `tests/fixtures/oss/pdf/three-pages.pdf`（形状由 poppler 独立验过，来源说明在
/// 那个目录的 `README.md`）。
#[test]
fn the_rendered_member_of_the_fixed_family_has_no_hrefs() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/pdf/three-pages.pdf");
    let provider = provider_for(&path).expect("PDF 该有 provider");
    assert_eq!(provider.format(), Format::Pdf);
    let (_, content) = provider
        .read(&path)
        .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let Content::Fixed(fixed) = content else {
        panic!("PDF 该读成定版式");
    };
    assert_eq!(fixed.page_count(), 3, "pdfinfo 说 3 页");
    for (index, page) in fixed.pages.iter().enumerate() {
        assert!(
            page.image_href().is_none(),
            "渲染型的一页没有容器路径，第 {index} 页却有"
        );
        assert!(
            matches!(page.source, crate::content::format::PageSource::Render { index: at } if at as usize == index),
            "第 {index} 页的落点该是自己的页号"
        );
    }
    assert!(
        fixed.images.is_empty(),
        "渲染型的页不许进图片表：{:?}",
        fixed.images.iter().map(|i| &i.href).collect::<Vec<_>>()
    );
    // 字节问得到，只是要现算。
    let png = provider
        .render_page(&path, 0, 512)
        .expect("第 1 页该画得出来");
    assert!(png.starts_with(b"\x89PNG"), "交回的该是 PNG");
    // 而它压根没有「正文」这一问：定版式不进排版引擎。
    assert!(
        provider
            .chapter_source(&path, &crate::content::format::ChapterLocator::Section(0))
            .is_err(),
        "PDF 没有章，第 0 章这一问该说不"
    );
}
