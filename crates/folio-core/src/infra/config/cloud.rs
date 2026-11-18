use serde::{Deserialize, Serialize};

/// OPDS 书源列表。与 `Sync` 同一条:密码住在 0600 的 `config.json` 里,
/// 不单开秘密文件。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Opds {
    pub catalogs: Vec<OpdsCatalog>,
}

impl Opds {
    /// 名称、地址、用户名去掉误带空白；密码原样保留。地址不改写（feed 的相对链接
    /// 要按它解析，擅自补斜杠会让 `../` 的基准变一格）。
    pub fn normalized(mut self) -> Self {
        for catalog in &mut self.catalogs {
            catalog.name = catalog.name.trim().to_string();
            catalog.url = catalog.url.trim().to_string();
            catalog.username = catalog.username.trim().to_string();
        }
        self
    }
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpdsCatalog {
    pub name: String,
    pub url: String,
    pub username: String,
    pub password: String,
}

/// 取书网盘凭据；与进度同步设置分开。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cloud {
    pub webdav: WebDavDrive,
    pub baidu: BaiduDrive,
    pub aliyun: AliyunDrive,
}

impl Cloud {
    /// 地址、用户名、目录去掉误带的首尾空白；密码原样保留 —— 空格可能是密码的一部分。
    /// token 不同：那是机器签发的密文，本来就不可能含空格，粘贴带进来的首尾空白
    /// 清掉才是帮读者。
    pub fn normalized(mut self) -> Self {
        self.webdav = self.webdav.normalized();
        self.baidu = self.baidu.normalized();
        self.aliyun = self.aliyun.normalized();
        self
    }
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebDavDrive {
    /// 入口根地址（建议 https）。浏览器从它下面的 [`WebDavDrive::dir`] 打开。
    pub url: String,
    pub username: String,
    pub password: String,
    /// 打开浏览器时落在哪一级目录；空串 = 从根开始。
    pub dir: String,
}

impl WebDavDrive {
    pub fn normalized(mut self) -> Self {
        self.url = self.url.trim().to_string();
        self.username = self.username.trim().to_string();
        // 去掉两端斜杠，拼路径时只由浏览器层补一次 —— 与 [`Sync::normalized`] 对
        // `remote_dir` 同一条规矩。
        self.dir = self.dir.trim().trim_matches('/').to_string();
        self
    }

    /// 有地址且至少填了用户名，才谈得上浏览。
    pub fn configured(&self) -> bool {
        !self.url.is_empty() && !self.username.is_empty()
    }
}

/// 百度网盘凭据；OAuth 可刷新，手贴 token 无刷新凭据。expires_at 为 Unix 秒，0 表示未知。
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BaiduDrive {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub account: String,
}

impl BaiduDrive {
    /// token 与刷新凭据都是机器签发的密文，粘贴带进来的首尾空白清掉 —— 与「密码原样」
    /// 相反的一条，判据是密文不可能含空格（见 [`Cloud::normalized`]）。账号名是给人看的，
    /// 照地址那条规矩修剪。
    pub fn normalized(mut self) -> Self {
        self.access_token = self.access_token.trim().to_string();
        self.refresh_token = self.refresh_token.trim().to_string();
        self.account = self.account.trim().to_string();
        // 到期时刻只认正数：手贴 token 那条路没有这个数，坏数据归 0（未知）。
        if self.expires_at <= 0 {
            self.expires_at = 0;
        }
        self
    }

    /// 有一段 token，才谈得上浏览；过没过期不用在这里管，请求自己会说。
    pub fn configured(&self) -> bool {
        !self.access_token.is_empty()
    }

    /// 有没有 OAuth 的刷新凭据 —— 有它，token 过期才能自己续。
    pub fn refreshable(&self) -> bool {
        !self.refresh_token.is_empty()
    }
}

/// 阿里云盘凭据；每次文件请求同时需要 token 与授权所得 drive_id。
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AliyunDrive {
    pub access_token: String,
    pub refresh_token: String,
    /// 到期时刻（Unix 秒）。0 = 未知（老配置 / 手贴 token）。
    pub expires_at: i64,
    /// 云盘编号。授权后由 `getDriveInfo` 问一次，之后随配置走。
    pub drive_id: String,
    /// 账号名（昵称），账号列表上给读者看「这一格是谁」。
    pub account: String,
}

impl AliyunDrive {
    /// 与 [`BaiduDrive::normalized`] 同一条规矩：密文修剪、账号名修剪、到期时刻只认正数。
    pub fn normalized(mut self) -> Self {
        self.access_token = self.access_token.trim().to_string();
        self.refresh_token = self.refresh_token.trim().to_string();
        self.drive_id = self.drive_id.trim().to_string();
        self.account = self.account.trim().to_string();
        if self.expires_at <= 0 {
            self.expires_at = 0;
        }
        self
    }

    /// token 与 `drive_id` **都**要有，才谈得上浏览：缺 drive_id 的请求一定被拒，
    /// 与其让读者看见一串看不懂的 `InvalidParameter`，不如在入口处就当没配好。
    pub fn configured(&self) -> bool {
        !self.access_token.is_empty() && !self.drive_id.is_empty()
    }

    pub fn refreshable(&self) -> bool {
        !self.refresh_token.is_empty()
    }
}
