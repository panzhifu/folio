//! OPDS 书源：1.x Atom 与 2.0 JSON 的 feed 形状、请求与相对链接解析。
//!
//! 只做「把一页目录读成条目」：浏览、封面、下载都在 app 侧。这样协议字段的兼容规则
//! 可以离线测，而导入仍走现有那条 URL → `.staging` → 管线。
//!
//! 相对链接一律按**这次请求的 feed URL** 解析后交给上层：上层拿到的是绝对地址，
//! 不必知道某家服务器把 `next` 写成了 `/opds/next` 还是 `../next?page=2`。

use base64::Engine as _;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use serde_json::Value;
use url::Url;

use super::remote::{self, Method, Request, Transport};

/// 一页 OPDS 目录。导航条目与出版物混在 `entries` 里，由 [`Entry::navigation`]
/// 与 [`Entry::acquisitions`] 区分。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Feed {
    pub title: String,
    pub entries: Vec<Entry>,
    /// `rel="next"`，已经解析成绝对地址。
    pub next: Option<String>,
    pub self_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    pub title: String,
    /// 多位作者用逗号连成一行；界面只摆这一行，不必为多作者再拆。
    pub authors: String,
    pub summary: Option<String>,
    /// 子目录（`rel="subsection"`），绝对地址。
    pub navigation: Option<String>,
    /// 完整封面；`thumbnail` 优先用于列表。
    pub cover: Option<String>,
    pub thumbnail: Option<String>,
    pub acquisitions: Vec<Acquisition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acquisition {
    pub href: String,
    pub media_type: String,
    pub rel: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("远端：{0}")]
    Remote(#[from] remote::Error),
    #[error("feed 读不懂：{0}")]
    Parse(String),
}

/// Basic 认证头。用户名与密码都空时返回 `None`（匿名书源）。
pub fn auth_header(username: &str, password: &str) -> Option<(String, String)> {
    if username.is_empty() && password.is_empty() {
        return None;
    }
    let pair = format!("{username}:{password}");
    Some((
        "Authorization".into(),
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(pair.as_bytes())
        ),
    ))
}

/// 读一页 feed。`url` 是这一页自己的地址，相对链接以它为基准。
pub fn fetch(
    transport: &dyn Transport,
    url: &str,
    username: &str,
    password: &str,
) -> Result<Feed, Error> {
    let mut request = Request::new(Method::Get, url.to_string()).header(
        "Accept",
        "application/atom+xml,application/opds+json;q=0.9,*/*;q=0.8",
    );
    if let Some((name, value)) = auth_header(username, password) {
        request = request.header(name, value);
    }
    let response = transport.request(request)?;
    if !(200..300).contains(&response.status) {
        return Err(Error::Remote(match response.status {
            401 => remote::Error::Unauthorized {
                status: response.status,
            },
            403 => remote::Error::Forbidden {
                status: response.status,
            },
            404 => remote::Error::NotFound,
            other => remote::Error::Unexpected { status: other },
        }));
    }
    let content_type = response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let body = String::from_utf8_lossy(&response.body);
    parse(&body, url, &content_type)
}

/// 按内容类型（缺失时按首字符）选 1.x Atom 还是 2.0 JSON。
pub fn parse(body: &str, base: &str, content_type: &str) -> Result<Feed, Error> {
    let trimmed = body.trim_start();
    if content_type.contains("json") || trimmed.starts_with('{') {
        parse_opds2(body, base)
    } else {
        parse_atom(body, base)
    }
}

// ── OPDS 1.x Atom ──────────────────────────────────────────────────────────

#[derive(Default)]
struct RawEntry {
    title: String,
    summary: String,
    content: String,
    author: String,
    links: Vec<RawLink>,
}

#[derive(Default, Clone)]
struct RawLink {
    href: String,
    rel: String,
    media_type: String,
    title: String,
}

#[derive(Default)]
struct RawFeed {
    title: String,
    links: Vec<RawLink>,
    entries: Vec<RawEntry>,
}

fn parse_atom(body: &str, base: &str) -> Result<Feed, Error> {
    let mut reader = Reader::from_reader(body.as_bytes());
    let mut feed = RawFeed::default();
    let mut entry: Option<RawEntry> = None;
    let mut in_author = false;
    let mut field: Option<&'static str> = None;
    let mut text = String::new();
    let mut depth = 0usize;

    loop {
        match reader
            .read_event()
            .map_err(|error| Error::Parse(error.to_string()))?
        {
            Event::Eof => break,
            Event::Start(e) => {
                depth += 1;
                let name = local_name(e.name().as_ref()).to_string();
                match name.as_str() {
                    "entry" => {
                        entry = Some(RawEntry::default());
                        in_author = false;
                        field = None;
                    }
                    "author" => in_author = true,
                    "name" if in_author => start_field(&mut field, &mut text, "author"),
                    "title" => start_field(&mut field, &mut text, "title"),
                    "summary" => start_field(&mut field, &mut text, "summary"),
                    "content" => start_field(&mut field, &mut text, "content"),
                    "link" => push_link(&mut feed, &mut entry, link_of(&e)),
                    _ => {}
                }
            }
            // `<link/>` 是常态；写成 `<link></link>` 也收。
            Event::Empty(e) => {
                if local_name(e.name().as_ref()) == "link" {
                    push_link(&mut feed, &mut entry, link_of(&e));
                }
            }
            Event::Text(e) => {
                if field.is_some() {
                    text.push_str(&xml_text(e.as_ref()));
                }
            }
            Event::GeneralRef(e) => {
                if field.is_some() {
                    text.push_str(&xml_text(&format!("&{};", e.as_ref())));
                }
            }
            Event::End(e) => {
                depth = depth.saturating_sub(1);
                let name = local_name(e.name().as_ref()).to_string();
                match name.as_str() {
                    "author" => in_author = false,
                    "title" | "summary" | "content" | "name" => {
                        if let Some(which) = field.take() {
                            let value = collapse(&text);
                            text.clear();
                            match which {
                                "title" => {
                                    if let Some(current) = entry.as_mut() {
                                        if current.title.is_empty() {
                                            current.title = value;
                                        }
                                    } else if feed.title.is_empty() {
                                        feed.title = value;
                                    }
                                }
                                "summary" => {
                                    if let Some(current) = entry.as_mut() {
                                        current.summary = value;
                                    }
                                }
                                "content" => {
                                    if let Some(current) = entry.as_mut() {
                                        current.content = value;
                                    }
                                }
                                "author" => {
                                    if let Some(current) = entry.as_mut()
                                        && !value.is_empty()
                                    {
                                        if current.author.is_empty() {
                                            current.author = value;
                                        } else {
                                            current.author.push_str(", ");
                                            current.author.push_str(&value);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    "entry" => {
                        if let Some(current) = entry.take() {
                            feed.entries.push(current);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        // quick-xml 读到 Eof 不会替我们判「还有没闭合的元素」；半截 feed 若被当成空目录，
        // 读者看到的是「这个书源没有书」，而不是「这个地址不像 OPDS」。
        return Err(Error::Parse("feed 在结尾处仍有未闭合的元素".into()));
    }

    let next = feed
        .links
        .iter()
        .find(|link| link.rel == "next")
        .map(|link| resolve(base, &link.href));
    let self_url = feed
        .links
        .iter()
        .find(|link| link.rel == "self")
        .map(|link| resolve(base, &link.href));
    let entries = feed
        .entries
        .into_iter()
        .map(|raw| convert_entry(raw, base))
        .collect();
    Ok(Feed {
        title: if feed.title.is_empty() {
            "OPDS".into()
        } else {
            feed.title
        },
        entries,
        next,
        self_url,
    })
}

fn start_field(field: &mut Option<&'static str>, text: &mut String, name: &'static str) {
    *field = Some(name);
    text.clear();
}

fn push_link(feed: &mut RawFeed, entry: &mut Option<RawEntry>, link: RawLink) {
    if link.href.is_empty() {
        return;
    }
    match entry.as_mut() {
        Some(current) => current.links.push(link),
        None => feed.links.push(link),
    }
}

fn link_of(e: &BytesStart<'_>) -> RawLink {
    let mut link = RawLink::default();
    for attr in e.attributes().flatten() {
        let key = local_name(attr.key.as_ref()).to_ascii_lowercase();
        let value = attr
            .normalized_value(XmlVersion::Implicit1_0)
            .map(|value| value.into_owned())
            .unwrap_or_default();
        match key.as_str() {
            "href" => link.href = value,
            "rel" => link.rel = value,
            "type" => link.media_type = value,
            "title" => link.title = value,
            _ => {}
        }
    }
    link
}

fn convert_entry(raw: RawEntry, base: &str) -> Entry {
    let mut navigation = None;
    let mut cover = None;
    let mut thumbnail = None;
    let mut acquisitions = Vec::new();
    for link in raw.links {
        let href = resolve(base, &link.href);
        match link.rel.as_str() {
            "subsection" => navigation = Some(href),
            "http://opds-spec.org/image/thumbnail" => thumbnail = Some(href),
            "http://opds-spec.org/image" | "http://opds-spec.org/cover" => cover = Some(href),
            rel if rel == "enclosure" || rel.contains("acquisition") => {
                acquisitions.push(Acquisition {
                    href,
                    media_type: link.media_type,
                    rel: link.rel,
                });
            }
            _ => {}
        }
    }
    Entry {
        title: raw.title,
        authors: raw.author,
        summary: if raw.summary.is_empty() {
            (!raw.content.is_empty()).then_some(raw.content)
        } else {
            Some(raw.summary)
        },
        navigation,
        cover,
        thumbnail,
        acquisitions,
    }
}

// ── OPDS 2.0 JSON ──────────────────────────────────────────────────────────

fn parse_opds2(body: &str, base: &str) -> Result<Feed, Error> {
    let root: Value =
        serde_json::from_str(body).map_err(|error| Error::Parse(error.to_string()))?;
    let title = root
        .get("metadata")
        .and_then(|meta| meta.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("OPDS")
        .to_string();

    let feed_links = links_of(root.get("links"));
    let next = find_link(&feed_links, "next").map(|link| resolve(base, &link.href));
    let self_url = find_link(&feed_links, "self").map(|link| resolve(base, &link.href));

    let mut entries = Vec::new();
    if let Some(navigation) = root.get("navigation").and_then(Value::as_array) {
        for item in navigation {
            let Some(href) = item.get("href").and_then(Value::as_str) else {
                continue;
            };
            entries.push(Entry {
                title: text_field(item, "title"),
                summary: optional_text(item, "description"),
                navigation: Some(resolve(base, href)),
                ..Entry::default()
            });
        }
    }
    if let Some(publications) = root.get("publications").and_then(Value::as_array) {
        for publication in publications {
            let metadata = publication.get("metadata");
            let title = metadata
                .map(|meta| text_field(meta, "title"))
                .unwrap_or_default();
            let authors = authors_of(metadata);
            let summary = metadata
                .and_then(|meta| optional_text(meta, "description"))
                .or_else(|| metadata.and_then(|meta| optional_text(meta, "subtitle")));

            let mut links = links_of(publication.get("links"));
            // 少数 2.0 feed 把图放在 `images` 里而不是 links。
            if let Some(images) = publication.get("images").and_then(Value::as_array) {
                for image in images {
                    if let Some(href) = image.get("href").and_then(Value::as_str) {
                        links.push(RawLink {
                            href: href.into(),
                            rel: "http://opds-spec.org/image".into(),
                            media_type: image
                                .get("type")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .into(),
                            title: String::new(),
                        });
                    }
                }
            }
            let mut cover = None;
            let mut thumbnail = None;
            let mut acquisitions = Vec::new();
            for link in links {
                let href = resolve(base, &link.href);
                match link.rel.as_str() {
                    "http://opds-spec.org/image/thumbnail" => thumbnail = Some(href),
                    "http://opds-spec.org/image" | "http://opds-spec.org/cover" => {
                        cover = Some(href)
                    }
                    rel if rel == "enclosure" || rel.contains("acquisition") => {
                        acquisitions.push(Acquisition {
                            href,
                            media_type: link.media_type,
                            rel: link.rel,
                        });
                    }
                    _ => {}
                }
            }
            entries.push(Entry {
                title,
                authors,
                summary,
                navigation: None,
                cover,
                thumbnail,
                acquisitions,
            });
        }
    }

    Ok(Feed {
        title,
        entries,
        next,
        self_url,
    })
}

fn links_of(value: Option<&Value>) -> Vec<RawLink> {
    value
        .and_then(Value::as_array)
        .map(|links| {
            links
                .iter()
                .filter_map(|link| {
                    Some(RawLink {
                        href: link.get("href")?.as_str()?.to_string(),
                        rel: link
                            .get("rel")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        media_type: link
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        title: link
                            .get("title")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn find_link<'a>(links: &'a [RawLink], rel: &str) -> Option<&'a RawLink> {
    links.iter().find(|link| link.rel == rel)
}

fn text_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(collapse)
        .unwrap_or_default()
}

fn optional_text(value: &Value, key: &str) -> Option<String> {
    let text = text_field(value, key);
    (!text.is_empty()).then_some(text)
}

fn authors_of(metadata: Option<&Value>) -> String {
    let Some(metadata) = metadata else {
        return String::new();
    };
    let mut names = Vec::new();
    for key in ["author", "contributor"] {
        let Some(value) = metadata.get(key) else {
            continue;
        };
        match value {
            Value::Array(items) => {
                for item in items {
                    let name = match item {
                        Value::String(name) => name.clone(),
                        _ => text_field(item, "name"),
                    };
                    if !name.is_empty() {
                        names.push(name);
                    }
                }
            }
            Value::String(name) => names.push(name.clone()),
            _ => {
                let name = text_field(value, "name");
                if !name.is_empty() {
                    names.push(name);
                }
            }
        }
    }
    names.join(", ")
}

// ── 共用小件 ───────────────────────────────────────────────────────────────

/// 相对链接按 feed 地址解析。解析不了时原样交回 —— 让上层的请求报错，
/// 比在这里静默拼出一个半截 URL 更好查。
fn resolve(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    Url::parse(base)
        .and_then(|base| base.join(href))
        .map(|url| url.to_string())
        .unwrap_or_else(|_| href.to_string())
}

fn local_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn xml_text(raw: &str) -> String {
    quick_xml::escape::unescape(raw)
        .map(|text| text.into_owned())
        .unwrap_or_else(|_| raw.to_string())
}

/// feed 里的摘要常见多行 / HTML 实体；列表只摆一行，压成单空格。
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{Entry, Error, Feed, auth_header, parse, resolve};
    use crate::net::remote::{Cancel, Method, Request, Response, Sink, Transport};

    const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
    <feed xmlns="http://www.w3.org/2005/Atom">
      <title>测试书源</title>
      <link rel="self" href="/opds/"/>
      <link rel="next" href="?page=2"/>
      <entry>
        <title>子目录</title>
        <link rel="subsection" type="application/atom+xml" href="sub/"/>
      </entry>
      <entry>
        <title>一本书</title>
        <author><name>作者甲</name></author>
        <author><name>作者乙</name></author>
        <summary>简介在这里</summary>
        <link rel="http://opds-spec.org/image/thumbnail" href="covers/1.jpg"/>
        <link rel="http://opds-spec.org/acquisition/open-access" type="application/epub+zip" href="books/1.epub"/>
        <link rel="http://opds-spec.org/acquisition/open-access" type="application/pdf" href="books/1.pdf"/>
      </entry>
    </feed>"#;

    const OPDS2: &str = r#"{
      "metadata": {"title": "2.0 书源"},
      "links": [{"rel": "next", "href": "page2.json"}],
      "navigation": [{"title": "子目录", "href": "sub/"}],
      "publications": [{
        "metadata": {
          "title": "一本 2.0 的书",
          "author": [{"name": "作者甲"}, {"name": "作者乙"}],
          "description": "简介"
        },
        "links": [
          {"rel": "http://opds-spec.org/image/thumbnail", "href": "covers/2.jpg", "type": "image/jpeg"},
          {"rel": "http://opds-spec.org/acquisition", "href": "books/2.epub", "type": "application/epub+zip"}
        ]
      }]
    }"#;

    #[test]
    fn atom_feed_keeps_navigation_publications_and_resolves_links() {
        let feed = parse(ATOM, "https://example.test/opds/", "application/atom+xml").unwrap();
        assert_eq!(feed.title, "测试书源");
        assert_eq!(feed.entries.len(), 2);
        assert_eq!(feed.entries[0].title, "子目录");
        assert_eq!(
            feed.entries[0].navigation.as_deref(),
            Some("https://example.test/opds/sub/")
        );
        let book = &feed.entries[1];
        assert_eq!(book.authors, "作者甲, 作者乙");
        assert_eq!(book.summary.as_deref(), Some("简介在这里"));
        assert_eq!(
            book.thumbnail.as_deref(),
            Some("https://example.test/opds/covers/1.jpg")
        );
        assert_eq!(book.acquisitions.len(), 2);
        assert_eq!(
            book.acquisitions[0].href,
            "https://example.test/opds/books/1.epub"
        );
        assert_eq!(
            feed.next.as_deref(),
            Some("https://example.test/opds/?page=2")
        );
    }

    #[test]
    fn opds2_feed_keeps_navigation_and_publications() {
        let feed = parse(OPDS2, "https://example.test/opds/", "application/opds+json").unwrap();
        assert_eq!(feed.title, "2.0 书源");
        assert_eq!(feed.entries.len(), 2);
        assert_eq!(feed.entries[0].title, "子目录");
        let book = &feed.entries[1];
        assert_eq!(book.title, "一本 2.0 的书");
        assert_eq!(book.authors, "作者甲, 作者乙");
        assert_eq!(book.summary.as_deref(), Some("简介"));
        assert_eq!(
            book.thumbnail.as_deref(),
            Some("https://example.test/opds/covers/2.jpg")
        );
        assert_eq!(
            book.acquisitions[0].href,
            "https://example.test/opds/books/2.epub"
        );
        assert_eq!(
            feed.next.as_deref(),
            Some("https://example.test/opds/page2.json")
        );
    }

    #[test]
    fn relative_hrefs_resolve_against_the_feed_url() {
        assert_eq!(
            resolve("https://host/a/b/feed.xml", "../c.xml"),
            "https://host/a/c.xml"
        );
        assert_eq!(
            resolve("https://host/a/b/feed.xml", "/root.xml"),
            "https://host/root.xml"
        );
        assert_eq!(
            resolve("https://host/a/", "next.xml"),
            "https://host/a/next.xml"
        );
    }

    #[test]
    fn auth_header_is_basic_and_absent_when_empty() {
        assert_eq!(auth_header("", ""), None);
        let (name, value) = auth_header("user", "pass").unwrap();
        assert_eq!(name, "Authorization");
        assert_eq!(value, "Basic dXNlcjpwYXNz");
    }

    #[test]
    fn a_body_that_looks_like_json_is_parsed_as_20_even_without_content_type() {
        let feed = parse(OPDS2, "https://example.test/opds/", "").unwrap();
        assert_eq!(feed.entries.len(), 2);
    }

    #[test]
    fn a_broken_feed_says_so_instead_of_returning_an_empty_one() {
        let error = parse("<feed><entry>", "https://example.test/", "").unwrap_err();
        assert!(matches!(error, Error::Parse(_)));
    }

    struct Mock(Response, std::sync::Mutex<Option<Request>>);
    impl Transport for Mock {
        fn request(&self, req: Request) -> crate::net::remote::Result<Response> {
            *self.1.lock().unwrap() = Some(req);
            Ok(self.0.clone())
        }
        fn download(
            &self,
            _req: Request,
            _sink: &mut dyn Sink,
            _cancel: &Cancel,
        ) -> crate::net::remote::Result<u64> {
            Err(crate::net::remote::Error::Cancelled)
        }
    }

    #[test]
    fn fetch_sends_basic_auth_and_accepts_atom() {
        let response = Response {
            status: 200,
            headers: vec![(
                "content-type".into(),
                "application/atom+xml; charset=utf-8".into(),
            )],
            body: ATOM.as_bytes().to_vec(),
        };
        let mock = Mock(response, std::sync::Mutex::new(None));
        let feed = super::fetch(&mock, "https://example.test/opds/", "user", "pass").unwrap();
        assert_eq!(feed.entries.len(), 2);
        let req = mock.1.lock().unwrap().take().unwrap();
        assert_eq!(req.method, Method::Get);
        assert!(
            req.headers
                .iter()
                .any(|(name, value)| name == "Authorization" && value == "Basic dXNlcjpwYXNz")
        );
        assert!(
            req.headers
                .iter()
                .any(|(name, value)| name == "Accept" && value.contains("application/atom+xml"))
        );
    }

    #[test]
    fn feed_entries_can_be_compared_and_cloned() {
        let feed = Feed::default();
        assert_eq!(feed.entries, Vec::<Entry>::new());
        assert_eq!(feed.clone(), feed);
    }
}
