use super::*;

fn endpoint() -> Endpoint {
    Endpoint::new("https://dav.example.com/dav/books", "noke", "app-pass")
}

/// 日志文件会被贴进工单，所以地址里那一段凭据不能跟着进去 —— 而路径要留着，
/// 排查要的正是它。
#[test]
fn a_url_with_credentials_in_it_logs_without_them() {
    assert_eq!(
        url_for_log("https://noke:app-pass@dav.example.com/dav/books/蜗牛.epub"),
        "https://***@dav.example.com/dav/books/蜗牛.epub"
    );
    // 没写凭据时要原样，不能把主机当 userinfo 吃掉。
    assert_eq!(
        url_for_log("https://dav.example.com/dav/books/蜗牛.epub"),
        "https://dav.example.com/dav/books/蜗牛.epub"
    );
    // 路径里的 `@`（邮箱式目录名）不算凭据。
    assert_eq!(
        url_for_log("https://dav.example.com/a@b/c"),
        "https://dav.example.com/a@b/c"
    );
}

fn client(responses: Vec<Response>) -> (Client, Arc<Mock>) {
    let mock = Arc::new(Mock::new(responses));
    (
        Client::new(endpoint(), Arc::clone(&mock) as Arc<dyn Transport>),
        mock,
    )
}

/// 一份最小可用的 207：被查询的集合自己 + 一个中文目录 + 一个带全属性的大文件。
const SHELF: &str = r#"<?xml version="1.0"?>
    <d:multistatus xmlns:d="DAV:">
      <d:response>
        <d:href>/dav/books/</d:href>
        <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat>
      </d:response>
      <d:response>
        <d:href>/dav/books/%E6%96%97%E6%88%98%E7%8B%82%E6%BD%AE/</d:href>
        <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat>
      </d:response>
      <d:response>
        <d:href>/dav/books/shejie.epub</d:href>
        <d:propstat><d:prop>
          <d:resourcetype/>
          <d:getcontentlength>15144675</d:getcontentlength>
          <d:getlastmodified>Sat, 11 Dec 2019 22:55:00 GMT</d:getlastmodified>
          <d:getetag>"a1b2"</d:getetag>
        </d:prop></d:propstat>
      </d:response>
    </d:multistatus>"#;

fn ok207(xml: &str) -> Response {
    Response::new(207, xml.as_bytes().to_vec())
}

#[test]
fn entity_in_a_display_name_survives_whole() {
    // 书名里带 `&` 与实体的真实样子。解析器要是把文本事件按「一见就清标记」处理，
    // 这里就会只剩头一段；`&amp;` 不被反转义则会把 `&` 原样留在名字里。
    let xml = r#"<d:multistatus xmlns:d="DAV:"><response>
          <d:href>/dav/books/a.epub</d:href>
          <d:propstat><d:prop>
            <d:displayname>蜗牛 &amp; 熊 &#20013;，与“引号”</d:displayname>
          </d:prop></d:propstat>
        </response></d:multistatus>"#;
    let (client, _mock) = client(vec![ok207(xml)]);
    let entries = client.list("").unwrap();
    assert_eq!(entries[0].name, "蜗牛 & 熊 中，与“引号”");
}

#[test]
fn list_returns_one_level_with_decoded_paths() {
    let (client, mock) = client(vec![ok207(SHELF)]);
    let entries = client.list("").expect("应当解析出列表");
    let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    // 被查询的集合自己不该出现在结果里。
    assert_eq!(paths, ["斗战狂潮", "shejie.epub"]);
    assert!(entries[0].is_dir, "目录靠 resourcetype 里的 collection 判");
    assert!(!entries[1].is_dir);
    assert_eq!(entries[1].size, Some(15144675));
    assert_eq!(entries[1].etag.as_deref(), Some("\"a1b2\""));
    assert_eq!(
        entries[1].modified.as_deref(),
        Some("Sat, 11 Dec 2019 22:55:00 GMT")
    );
    assert_eq!(mock.last_request().method, Method::Propfind);
}

#[test]
fn propfind_carries_depth_one_and_basic_credentials() {
    let (client, mock) = client(vec![ok207(SHELF)]);
    client.list("").unwrap();
    let req = mock.last_request();
    // 钉住这个值：以后改凭据来源（钥匙串、应用密码）时这条必须跟着变，
    // 否则等于悄悄换了认证方式而没人知道。`noke:app-pass` 的 base64。
    assert_eq!(
        req.headers[0],
        ("Authorization".into(), "Basic bm9rZTphcHAtcGFzcw==".into())
    );
    assert!(
        req.headers.iter().any(|(k, v)| k == "Depth" && v == "1"),
        "PROPFIND 必须带 Depth"
    );
    assert_eq!(req.url, "https://dav.example.com/dav/books/");
    assert!(!req.body.is_empty());
}

#[test]
fn paths_are_encoded_segment_by_segment() {
    assert_eq!(
        encode_path("斗战狂潮/第 1 章.epub"),
        "%E6%96%97%E6%88%98%E7%8B%82%E6%BD%AE/%E7%AC%AC%201%20%E7%AB%A0.epub"
    );
    // 这三个字符不编码会改变 URL 的结构，不是「不好看」的问题。
    assert_eq!(encode_path("a#b"), "a%23b");
    assert_eq!(encode_path("a?b"), "a%3Fb");
    assert_eq!(encode_path("50%"), "50%25");
    assert_eq!(encode_path(""), "");
}

#[test]
fn decode_then_encode_round_trips_a_percent_in_the_name() {
    // 服务器给 `a%25b.epub`，解出 `a%b.epub`，再编码必须回到 `a%25b.epub`。
    // 任何一步漏了就会二次编码成 `%2525`，然后 404 —— 而且只在带 `%` 的书名上。
    let decoded = percent_decode("a%25b.epub");
    assert_eq!(decoded, "a%b.epub");
    assert_eq!(encode_path(&decoded), "a%25b.epub");
}

#[test]
fn lone_percent_survives_instead_of_eating_the_entry() {
    // 有的服务器不编码字面 `%`。解码器既不能丢条目也不能崩。
    assert_eq!(percent_decode("100%25"), "100%");
    assert_eq!(percent_decode("100%zz"), "100%zz");
    assert_eq!(percent_decode("100%"), "100%", "末尾断掉的序列原样留着");
    assert_eq!(
        percent_decode("+"),
        "+",
        "`+` 是表单编码的规矩，路径里不适用"
    );
}

#[test]
fn href_base_variants_all_resolve() {
    let prefix = "/dav/books/";
    assert_eq!(
        relative_path(prefix, "/dav/books/a.epub").unwrap().0,
        "a.epub"
    );
    assert_eq!(relative_path(prefix, "a.epub").unwrap().0, "a.epub");
    assert_eq!(
        relative_path(prefix, "/proxy/dav/books/a.epub").unwrap().0,
        "a.epub"
    );
    assert_eq!(
        relative_path(prefix, "/dav/books/%E5%AD%90/").unwrap().0,
        "子"
    );
    // 认不出基准就丢弃，别猜一个路径出来：猜错的表现是下载 404，比列表少一项更难查。
    assert!(relative_path(prefix, "/srv/other/a.epub").is_none());
    // 尾斜杠要带出来，它是「这是个目录」的第二条证据。
    assert!(relative_path(prefix, "/dav/books/sub/").unwrap().1);
    // 集合自己 → 空串，由 `children` 负责丢弃。
    assert_eq!(relative_path(prefix, "/dav/books/").unwrap().0, "");
}

#[test]
fn unprefixed_multistatus_parses_too() {
    // 默认命名空间的写法（不带 `d:` 前缀）在真实服务器上同样存在。
    let xml = r#"<multistatus xmlns="DAV:"><response><href>/dav/books/a.epub</href>
        <propstat><prop><getcontentlength>10</getcontentlength></prop></propstat></response></multistatus>"#;
    let (client, _mock) = client(vec![ok207(xml)]);
    let entries = client.list("").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, "a.epub");
    assert_eq!(entries[0].size, Some(10));
    assert_eq!(
        entries[0].name, "a.epub",
        "没有 displayname 时用路径末段顶名"
    );
}

#[test]
fn self_row_dropped_deep_rows_cut_failed_rows_dropped() {
    // 一台无视 `Depth: 1` 的服务器：整棵树都回来了，还夹着一条 404。
    let xml = r#"<d:multistatus xmlns:d="DAV:">
          <d:response><d:href>/dav/books/</d:href></d:response>
          <d:response><d:href>/dav/books/keep.epub</d:href></d:response>
          <d:response><d:href>/dav/books/sub/deep.epub</d:href></d:response>
          <d:response><d:href>/dav/books/gone.epub</d:href>
            <d:status>HTTP/1.1 404 Not Found</d:status></d:response>
        </d:multistatus>"#;
    let (client, _mock) = client(vec![ok207(xml)]);
    let entries = client.list("").unwrap();
    assert_eq!(
        entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
        ["keep.epub"]
    );
}

#[test]
fn displayname_wins_over_the_path_tail() {
    let xml = r#"<d:multistatus xmlns:d="DAV:"><response>
          <d:href>/dav/books/x-123.epub</d:href>
          <d:propstat><d:prop><d:displayname>斗战狂潮.epub</d:displayname></d:prop></d:propstat>
        </response></d:multistatus>"#;
    let (client, _mock) = client(vec![ok207(xml)]);
    let entries = client.list("").unwrap();
    assert_eq!(entries[0].name, "斗战狂潮.epub");
    assert_eq!(
        entries[0].path, "x-123.epub",
        "显示名可以换，取文件的路径不能跟着换"
    );
}

#[test]
fn digest_only_server_gets_its_own_error() {
    let mut resp = Response::new(401, Vec::new());
    resp.headers
        .push(("www-authenticate".into(), "Digest realm=\"dav\"".into()));
    assert!(matches!(
        check_status(&resp, &[207]),
        Err(Error::DigestRequired)
    ));

    // 同时提供 Basic 的服务器要给「认证被拒」，不是「不支持 Digest」—— 这种是能修好的。
    let mut both = Response::new(401, Vec::new());
    both.headers.push((
        "WWW-Authenticate".into(),
        "Basic realm=\"dav\", Digest realm=\"dav\"".into(),
    ));
    assert!(matches!(
        check_status(&both, &[207]),
        Err(Error::Unauthorized { status: 401 })
    ));
    // 头完全没有（有些实现就只回 401）也要落在「认证被拒」这一档。
    assert!(matches!(
        check_status(&Response::new(401, Vec::new()), &[207]),
        Err(Error::Unauthorized { status: 401 })
    ));
}

#[test]
fn other_status_shapes_say_what_they_are() {
    assert!(check_status(&Response::new(207, Vec::new()), &[207, 200]).is_ok());
    assert!(matches!(
        check_status(&Response::new(404, Vec::new()), &[207]),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        check_status(&Response::new(403, Vec::new()), &[207]),
        Err(Error::Forbidden { status: 403 })
    ));
    // 507 是配额不足，备份写不进去时的真实答案，别只报「未知错误」。
    assert!(matches!(
        check_status(&Response::new(507, Vec::new()), &[201]),
        Err(Error::Unexpected { status: 507 })
    ));
    let err = check_status(&Response::new(405, Vec::new()), &[207]).unwrap_err();
    assert!(err.to_string().contains("WebDAV"), "{err}");
}

#[test]
fn list_of_a_subdirectory_queries_that_collection_url() {
    let (client, mock) = client(vec![ok207(SHELF)]);
    client.list("斗战狂潮").unwrap();
    assert_eq!(
        mock.last_request().url,
        "https://dav.example.com/dav/books/%E6%96%97%E6%88%98%E7%8B%82%E6%BD%AE/"
    );
}

#[test]
fn download_fills_the_sink_then_finishes_it_once() {
    // sink 通过一份共享记录留下自己的结局，因为 `Box<dyn Sink>` 的所有权进了传输层，
    // 调用方拿不回来 —— 真实实现（写到临时文件再改名）也是这个形状。
    #[derive(Clone, Default)]
    struct Log(Arc<Mutex<Vec<String>>>);
    impl Sink for Log {
        fn write(&mut self, chunk: &[u8]) -> std::io::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("write:{}", chunk.len()));
            Ok(())
        }
        fn finish(&mut self) -> std::io::Result<()> {
            self.0.lock().unwrap().push("finish".into());
            Ok(())
        }
        fn abort(&mut self) {
            self.0.lock().unwrap().push("abort".into());
        }
    }
    let log = Log::default();
    let (client, _mock) = client(vec![Response::new(200, Vec::new())]);
    let n = client
        .download("a.epub", &mut log.clone(), &Cancel::new())
        .unwrap();
    assert_eq!(n, 7, "abc + defg");
    assert_eq!(
        log.0.lock().unwrap().as_slice(),
        ["write:3", "write:4", "finish"],
        "finish 恰好一次，abort 一次没有"
    );
}

#[test]
fn cancelled_download_aborts_instead_of_finishing() {
    #[derive(Clone, Default)]
    struct Log(Arc<Mutex<Vec<String>>>);
    impl Sink for Log {
        fn write(&mut self, chunk: &[u8]) -> std::io::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("write:{}", chunk.len()));
            Ok(())
        }
        fn finish(&mut self) -> std::io::Result<()> {
            self.0.lock().unwrap().push("finish".into());
            Ok(())
        }
        fn abort(&mut self) {
            self.0.lock().unwrap().push("abort".into());
        }
    }
    let log = Log::default();
    let (client, _mock) = client(vec![Response::new(200, Vec::new())]);
    let cancel = Cancel::new();
    cancel.trigger();
    let err = client
        .download("a.epub", &mut log.clone(), &cancel)
        .expect_err("取消要当成一次失败返回");
    assert!(matches!(err, Error::Cancelled), "{err:?}");
    assert_eq!(log.0.lock().unwrap().as_slice(), ["abort"]);
}

#[test]
fn put_and_mkdir_target_the_same_escaped_url() {
    let (client, mock) = client(vec![
        Response::new(201, Vec::new()),
        Response::new(204, Vec::new()),
    ]);
    client.mkdir("备份/书库").unwrap();
    client.put("备份/书库/folio.json", b"{}".to_vec()).unwrap();
    let seen = mock.seen.lock().unwrap();
    assert_eq!(seen[0].method, Method::Mkcol);
    assert!(
        seen[0].url.ends_with('/') && seen[0].url.contains("%E5%A4%87"),
        "{}",
        seen[0].url
    );
    assert_eq!(seen[1].method, Method::Put);
    assert_eq!(seen[1].body, b"{}");
    assert!(!seen[1].url.ends_with('/'), "写文件不该带尾斜杠");
    assert!(
        seen[1].headers.iter().any(|(k, _)| k == "Content-Type"),
        "PUT 要声明类型"
    );
}

#[test]
fn mkdir_on_an_existing_collection_counts_as_success() {
    // 第二次跑备份会撞「目录已存在」，各家给的状态码不统一，流程不该因此中断。
    for status in [405u16, 409, 423] {
        let (client, _mock) = client(vec![Response::new(status, Vec::new())]);
        client.mkdir("备份").expect("已存在就等于可用");
    }
}

#[test]
fn status_code_extraction_tolerates_the_reason_phrase() {
    assert_eq!(status_code("HTTP/1.1 404 Not Found"), Some(404));
    assert_eq!(status_code("HTTP/1.0 200 OK"), Some(200));
    assert_eq!(status_code("not a status"), None);
}

#[test]
fn endpoint_normalizes_the_base_and_exposes_the_path_prefix() {
    assert_eq!(
        Endpoint::new("https://host/dav", "u", "p").base,
        "https://host/dav/"
    );
    assert_eq!(
        Endpoint::new("https://host/dav/", "u", "p").path_prefix(),
        "/dav/"
    );
    assert_eq!(
        Endpoint::new("https://host", "u", "p").path_prefix(),
        "/",
        "根挂载点也要有个能比对的前缀"
    );
}

/// 列子目录时只收**那一层**的直接孩子，`Entry.path` 仍是相对 Base 的完整路径 ——
/// 同步的远端目录（`folio-sync/`）就在子目录里，符号错了会一条都收不到。
#[test]
fn listing_a_subdirectory_keeps_its_direct_children_with_full_paths() {
    const SUB: &str = r#"<?xml version="1.0"?>
        <d:multistatus xmlns:d="DAV:">
          <d:response><d:href>/dav/books/folio/</d:href>
            <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat>
          </d:response>
          <d:response><d:href>/dav/books/folio/dev-a.json</d:href>
            <d:propstat><d:prop><d:resourcetype/></d:prop></d:propstat>
          </d:response>
          <d:response><d:href>/dav/books/folio/nested/ignore.json</d:href>
            <d:propstat><d:prop><d:resourcetype/></d:prop></d:propstat>
          </d:response>
          <d:response><d:href>/dav/books/other.json</d:href>
            <d:propstat><d:prop><d:resourcetype/></d:prop></d:propstat>
          </d:response>
        </d:multistatus>"#;
    let (client, _mock) = client(vec![ok207(SUB)]);
    let entries = client.list("folio").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, "folio/dev-a.json");
    assert_eq!(entries[0].name, "dev-a.json");
}
