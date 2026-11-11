//! WebDAV 作为一种网盘：把 [`dav::Client`] 的能力收进 [`Drive`] 的形状。
//!
//! 转发层薄得几乎不存在 —— 它存在的意义是让上层（浏览界面与导入来源）只认
//! [`Drive`]，不必知道对面是不是与同步那节同一份客户端。WebDAV 的协议规则
//! （PROPFIND 怎么解、`href` 怎么退让、路径怎么转义）都住在
//! [`crate::dav`]，那里有整套离线测试；这里不重复它们，只钉一件事：
//! `Drive` 这条转发线真的通（路径、认证、字节都到位）。

use std::sync::Arc;

use super::{Drive, Entry};
use crate::{
    Cancel, RemoteStore, Result, Sink, Transport,
    dav::{self, Client},
};

/// 一个 WebDAV 入口。构造即定死凭据与传输；协议规则全在 [`Client`]。
pub struct DavDrive {
    client: Client,
}

impl DavDrive {
    /// 从界面上那三格（地址 / 用户名 / 密码）开一台。地址补尾斜杠的规矩在
    /// [`dav::Endpoint::new`]。
    pub fn open(
        url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
        transport: Arc<dyn Transport>,
    ) -> Self {
        Self {
            client: Client::new(dav::Endpoint::new(url, username, password), transport),
        }
    }
}

impl RemoteStore for DavDrive {
    fn list(&self, path: &str) -> Result<Vec<Entry>> {
        self.client.list(path)
    }

    fn get(&self, path: &str) -> Result<Vec<u8>> {
        self.client.get(path)
    }

    fn put(&self, path: &str, body: Vec<u8>) -> Result<()> {
        self.client.put(path, body)
    }

    fn mkdir(&self, path: &str) -> Result<()> {
        self.client.mkdir(path)
    }

    fn download(&self, path: &str, sink: &mut dyn Sink, cancel: &Cancel) -> Result<u64> {
        self.client.download(path, sink, cancel)
    }
}

impl Drive for DavDrive {
    fn label(&self) -> &'static str {
        "webdav"
    }
}

#[cfg(test)]
mod tests {
    //! 转发层只钉一条：`Drive` 这条线真的通。WebDAV 的协议规则在
    //! [`crate::dav`] 的测试里有整套，不在这里重测。

    use std::sync::Arc;

    use super::DavDrive;
    use crate::dav::Mock;
    use crate::{Cancel, RemoteStore, Sink, Transport};

    /// 攒字节的落点，让「下了什么」能被断言。
    struct Buffer(Vec<u8>);

    impl Sink for Buffer {
        fn write(&mut self, chunk: &[u8]) -> std::io::Result<()> {
            self.0.extend_from_slice(chunk);
            Ok(())
        }

        fn finish(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        fn abort(&mut self) {}
    }

    /// 与 `dav` 测试里那份同形的 207：被查询的集合自己 + 一个中文目录 + 一个文件。
    const SHELF: &str = r#"<?xml version="1.0"?>
        <d:multistatus xmlns:d="DAV:">
          <d:response>
            <d:href>/dav/</d:href>
            <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat>
          </d:response>
          <d:response>
            <d:href>/dav/%E6%96%97%E6%88%98%E7%8B%82%E6%BD%AE/</d:href>
            <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat>
          </d:response>
          <d:response>
            <d:href>/dav/shejie.epub</d:href>
            <d:propstat><d:prop>
              <d:resourcetype/>
              <d:getcontentlength>15144675</d:getcontentlength>
            </d:prop></d:propstat>
          </d:response>
        </d:multistatus>"#;

    fn drive(responses: Vec<crate::Response>) -> (DavDrive, Arc<Mock>) {
        let mock = Arc::new(Mock::new(responses));
        (
            DavDrive::open(
                "https://dav.example.com/dav",
                "noke",
                "app-pass",
                Arc::clone(&mock) as Arc<dyn Transport>,
            ),
            mock,
        )
    }

    #[test]
    fn drive_lists_a_layer_and_carries_the_credentials() {
        let (drive, mock) = drive(vec![crate::Response::new(207, SHELF.as_bytes().to_vec())]);
        let entries = drive.list("").expect("应当解析出列表");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["斗战狂潮", "shejie.epub"]);

        let req = mock.last_request();
        assert_eq!(req.method, crate::Method::Propfind);
        assert_eq!(req.url, "https://dav.example.com/dav/");
        assert!(
            req.headers
                .iter()
                .any(|(k, v)| k == "Authorization" && v.starts_with("Basic ")),
            "列表请求要带上 Basic 认证"
        );
    }

    #[test]
    fn drive_downloads_into_the_sink() {
        let (drive, mock) = drive(vec![crate::Response::new(200, Vec::new())]);
        let mut sink = Buffer(Vec::new());
        let got = drive
            .download("books/蜗牛.epub", &mut sink, &Cancel::new())
            .expect("下载应当成功");
        // Mock 的块是固定的 "abc" + "defg"。
        assert_eq!(got, 7);
        assert_eq!(sink.0, b"abcdefg");

        let req = mock.last_request();
        assert_eq!(
            req.url, "https://dav.example.com/dav/books/%E8%9C%97%E7%89%9B.epub",
            "路径要逐段编码后拼到 Base 上"
        );
        assert!(
            req.headers.iter().any(|(k, _)| k == "Authorization"),
            "下载请求要带上认证"
        );
    }
}
