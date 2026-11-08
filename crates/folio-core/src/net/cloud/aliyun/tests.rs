//! 阿里云盘那一层的门：路径↔file_id、分页、下载直链、建目录、分片上传、错误分档。
//!
//! 全部离线：`Mock` 假传输按顺序给响应，逐条断言「发了什么」。

use std::sync::Arc;

use super::{APP_ROOT, AliyunDrive, ROOT_ID};
use crate::net::remote::dav::Mock;
use crate::net::remote::{Cancel, Error, Method, RemoteStore, Response, Sink, Transport};

fn ok(body: &str) -> Response {
    Response::new(200, body.as_bytes().to_vec())
}

fn drive_with(responses: Vec<Response>) -> (AliyunDrive, Arc<Mock>) {
    let mock = Arc::new(Mock::new(responses));
    let drive = AliyunDrive::new("ali.at", "88", Arc::clone(&mock) as Arc<dyn Transport>);
    (drive, mock)
}

fn body_of(req: &crate::net::remote::Request) -> serde_json::Value {
    serde_json::from_slice(&req.body).expect("请求体该是 JSON")
}

const LISTING: &str = r#"{
  "items": [
    {"file_id":"f1","name":"斗战狂潮","type":"folder"},
    {"file_id":"f2","name":"shejie.epub","type":"file","size":15144675}
  ],
  "next_marker":""
}"#;

/// 列根：POST JSON、带 Bearer 与 drive_id、parent_file_id 是 root；条目按类型分档。
#[test]
fn listing_the_root_uses_the_drive_and_bearer() {
    let (drive, mock) = drive_with(vec![ok(LISTING)]);
    let entries = drive.list("").expect("应当解析出列表");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["斗战狂潮", "shejie.epub"]);
    assert!(entries[0].is_dir);
    assert_eq!(entries[1].size, Some(15_144_675));

    let req = mock.last_request();
    assert_eq!(req.method, Method::Post);
    assert!(
        req.url.ends_with("/adrive/v1.0/openFile/list"),
        "{}",
        req.url
    );
    assert!(
        req.headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer ali.at"),
        "阿里云盘用 Bearer，不是 query 参数"
    );
    let body = body_of(&req);
    assert_eq!(body["drive_id"], "88");
    assert_eq!(body["parent_file_id"], ROOT_ID);
}

/// 分页：`next_marker` 非空时要接着翻，翻完才把整层交回来。
#[test]
fn a_long_directory_is_paged_through() {
    let first =
        r#"{"items":[{"file_id":"a","name":"1.png","type":"file","size":10}],"next_marker":"m2"}"#;
    let second =
        r#"{"items":[{"file_id":"b","name":"2.png","type":"file","size":20}],"next_marker":""}"#;
    let (drive, mock) = drive_with(vec![ok(first), ok(second)]);
    let entries = drive.list("").expect("两页都要拿回来");
    assert_eq!(
        entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        vec!["1.png", "2.png"],
        "少翻一页的症状就是「后面几张图不见」"
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(body_of(&requests[0])["marker"], "");
    assert_eq!(body_of(&requests[1])["marker"], "m2");
}

/// 子目录：路径先解析成父目录的 file_id（列表把它的子项记进表里），再用那个 id 去列。
#[test]
fn a_subdirectory_resolves_its_parent_id_first() {
    let root_listing =
        r#"{"items":[{"file_id":"dir-1","name":"books","type":"folder"}],"next_marker":""}"#;
    let sub_listing =
        r#"{"items":[{"file_id":"bk-1","name":"a.epub","type":"file","size":1}],"next_marker":""}"#;
    let (drive, mock) = drive_with(vec![ok(root_listing), ok(sub_listing)]);
    let entries = drive.list("books").expect("子目录");
    assert_eq!(entries[0].path, "books/a.epub", "返回的路径带上父段");

    let requests = mock.requests();
    assert_eq!(body_of(&requests[0])["parent_file_id"], ROOT_ID);
    assert_eq!(
        body_of(&requests[1])["parent_file_id"],
        "dir-1",
        "第二趟用的是上一趟记下的 id"
    );
}

/// 下载：先 getDownloadUrl（带 Bearer），再 GET 那条直链（不带 Bearer）。
#[test]
fn downloading_asks_for_a_url_then_fetches_it() {
    let root_listing =
        r#"{"items":[{"file_id":"bk-1","name":"a.epub","type":"file","size":7}],"next_marker":""}"#;
    let (drive, mock) = drive_with(vec![
        ok(root_listing),
        ok(r#"{"url":"https://cdn.example/a.epub?sig=1","expiration":"2099-01-01T00:00:00Z"}"#),
        ok(""),
    ]);
    // 先列一次，把 id 记进表里（模拟「浏览完再点下载」）。
    drive.list("").expect("列目录");
    let mut sink = Buffer::default();
    let got = drive
        .download("a.epub", &mut sink, &Cancel::new())
        .expect("下载应当成功");
    assert_eq!(got, 7, "Mock 的块是固定 \"abc\" + \"defg\"");
    assert_eq!(sink.bytes, b"abcdefg");

    let requests = mock.requests();
    let ask = &requests[1];
    assert!(ask.url.ends_with("openFile/getDownloadUrl"), "{}", ask.url);
    assert_eq!(body_of(ask)["file_id"], "bk-1");
    assert_eq!(body_of(ask)["drive_id"], "88");
    let fetch = &requests[2];
    assert_eq!(fetch.method, Method::Get);
    assert_eq!(fetch.url, "https://cdn.example/a.epub?sig=1");
    assert!(
        !fetch.headers.iter().any(|(k, _)| k == "Authorization"),
        "直链自带签名，不该再带 Bearer"
    );
}

/// 建目录：路径逐段解析，最后一段用 create + `type: folder`。
#[test]
fn making_a_directory_creates_the_leaf() {
    let root_listing =
        r#"{"items":[{"file_id":"dir-1","name":"snap","type":"folder"}],"next_marker":""}"#;
    let (drive, mock) = drive_with(vec![
        ok(root_listing),
        ok(r#"{"file_id":"dir-2","parent_file_id":"dir-1","name":"2026"}"#),
    ]);
    drive.mkdir("snap/2026").expect("建目录");

    let requests = mock.requests();
    let create = requests.last().unwrap();
    assert!(create.url.ends_with("openFile/create"), "{}", create.url);
    let body = body_of(create);
    assert_eq!(body["parent_file_id"], "dir-1");
    assert_eq!(body["name"], "2026");
    assert_eq!(body["type"], "folder");
}

/// 已存在的目录算成功：错误体里说 Exist 时回去找它的 id。
#[test]
fn an_existing_directory_counts_as_success() {
    let (drive, mock) = drive_with(vec![
        Response::new(409, br#"{"code":"Exist","message":"name exists"}"#.to_vec()),
        ok(r#"{"items":[{"file_id":"dir-9","name":"2026","type":"folder"}],"next_marker":""}"#),
    ]);
    drive.mkdir("2026").expect("已存在算成功");
    let requests = mock.requests();
    assert!(
        requests.len() >= 2,
        "第二趟是回到父目录里找那个 id：{requests:?}"
    );
}

/// 上传：create 要分片槽位 → 逐片 PUT → complete；字节数按实际发出的算。
///
/// 父目录 `out` 先得解析成 id —— 上传不建目录（那是 `mkdir_all` 的事，同步流程里
/// 先建后传），所以这里先给一趟根目录清单，里面就是那个 `out`。
#[test]
fn uploading_walks_the_three_steps() {
    let created = r#"{
      "file_id":"new-1",
      "upload_id":"up-1",
      "part_info_list":[{"part_number":1,"upload_url":"https://cdn.example/part1"}]
    }"#;
    let (drive, mock) = drive_with(vec![
        ok(r#"{"items":[{"file_id":"out-1","name":"out","type":"folder"}],"next_marker":""}"#),
        ok(created),
        ok(""),
        ok(r#"{"file_id":"new-1"}"#),
    ]);
    let data = b"hello alipan".to_vec();
    let mut source = std::io::Cursor::new(data.clone());
    let size = data.len() as u64;
    let got = drive
        .put_file("out/book.epub", &mut source, size, &Cancel::new())
        .expect("上传应当成功");
    assert_eq!(got, size);

    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        4,
        "列根解析父目录 / create / PUT 一片 / complete"
    );
    let create = body_of(&requests[1]);
    assert_eq!(create["name"], "book.epub");
    assert_eq!(create["type"], "file");
    assert_eq!(create["size"], size);
    assert_eq!(create["check_name_mode"], "overwrite");
    assert_eq!(
        create["parent_file_id"], "out-1",
        "父目录 id 由那一趟列表解析出来"
    );
    assert_eq!(requests[2].method, Method::Put);
    assert_eq!(requests[2].url, "https://cdn.example/part1");
    assert_eq!(requests[2].body, data, "那一片的字节要原样发出去");
    let complete = body_of(&requests[3]);
    assert_eq!(complete["file_id"], "new-1");
    assert_eq!(complete["upload_id"], "up-1");
}

/// 错误分档：token 失效归认证、文件名冲突归 Malformed 但原话留住、404 归 NotFound。
#[test]
fn errors_are_sorted_by_their_codes() {
    let (drive, _) = drive_with(vec![Response::new(
        401,
        br#"{"code":"AccessTokenInvalid","message":"bad token"}"#.to_vec(),
    )]);
    assert!(matches!(
        drive.list(""),
        Err(Error::Unauthorized { status: 401 })
    ));

    let (drive, _) = drive_with(vec![Response::new(
        404,
        br#"{"code":"NotFound.File","message":"gone"}"#.to_vec(),
    )]);
    assert!(matches!(drive.list(""), Err(Error::NotFound)));

    // 403 且没有 code：按状态分档。
    let (drive, _) = drive_with(vec![Response::new(403, Vec::new())]);
    assert!(matches!(
        drive.list(""),
        Err(Error::Forbidden { status: 403 })
    ));
}

/// 同步那一档的根：`APP_ROOT` 会先被建出来，之后逐层。
#[test]
fn the_sync_root_is_created_before_its_children() {
    let mock = Arc::new(Mock::new(vec![
        // mkdir_all 的第一步：建根。
        ok(r#"{"file_id":"app-1","name":"Folio"}"#),
        // 根建好之后，列根找它的子项（`mkdir("a")` 要先解析父目录）。
        ok(r#"{"items":[{"file_id":"app-1","name":"Folio","type":"folder"}],"next_marker":""}"#),
        // 建 `a`。
        ok(r#"{"file_id":"a-1","name":"a"}"#),
    ]));
    let drive = AliyunDrive::with_root(
        "ali.at",
        "88",
        Arc::clone(&mock) as Arc<dyn Transport>,
        APP_ROOT,
    );
    drive.mkdir_all("a").expect("逐层建目录");
    let requests = mock.requests();
    let first = body_of(&requests[0]);
    assert_eq!(first["name"], APP_ROOT, "先建应用自己的根");
    assert_eq!(first["parent_file_id"], ROOT_ID);
}

/// 攒字节的落点。
#[derive(Default)]
struct Buffer {
    bytes: Vec<u8>,
}

impl Sink for Buffer {
    fn write(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        self.bytes.extend_from_slice(chunk);
        Ok(())
    }

    fn finish(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn abort(&mut self) {
        self.bytes.clear();
    }
}
