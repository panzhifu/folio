use serde::{Deserialize, Serialize};

/// 同步存储提供方；网盘凭据复用 Cloud，WebDAV 使用 Sync。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncProvider {
    #[default]
    WebDav,
    Baidu,
    Aliyun,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sync {
    pub enabled: bool,
    /// 往哪一家走：WebDAV（默认）或百度网盘。
    pub provider: SyncProvider,
    /// WebDAV 入口根地址（建议 https）。同步文件挂在它下面的 `remote_dir`。
    pub url: String,
    pub username: String,
    pub password: String,
    /// 远端目录，默认 `folio-sync`；空串表示直接写在根下。
    pub remote_dir: String,
    /// 这台安装的稳定 id，也是它在远端那份快照的文件名（`<device_id>.json`）。
    pub device_id: String,
}

impl Default for Sync {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: SyncProvider::WebDav,
            url: String::new(),
            username: String::new(),
            password: String::new(),
            remote_dir: "folio-sync".into(),
            device_id: String::new(),
        }
    }
}

impl Sync {
    /// 与其它节同一条：手改过的值先去掉误带的首尾空白。**密码除外** —— 空格可能是密码
    /// 的一部分；远端目录去掉两端斜杠，拼路径时只由同步层补一次。
    pub fn normalized(mut self) -> Self {
        self.url = self.url.trim().to_string();
        self.username = self.username.trim().to_string();
        self.remote_dir = self.remote_dir.trim().trim_matches('/').to_string();
        self.device_id = self.device_id.trim().to_string();
        self
    }

    /// 有去处且打开了开关，才谈得上同步。两家网盘都不看地址 —— 它们的凭据在
    /// [`Cloud::baidu`] / [`Cloud::aliyun`] 里，缺没缺由同步服务那一层报。
    pub fn configured(&self) -> bool {
        self.enabled
            && match self.provider {
                SyncProvider::WebDav => !self.url.is_empty(),
                SyncProvider::Baidu | SyncProvider::Aliyun => true,
            }
    }
}

/// 局域网同步身份；本机保存密钥，账号 id 与校验码由密钥派生。
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    /// 这台设备参不参加局域网同步：既决定它肯不肯被同账号的设备读走书架与进度，
    /// 也决定它会不会主动去连别人。关掉不会抹掉密钥，重新打开还是同一个账号。
    pub enabled: bool,
    /// 那把密钥的十六进制（32 个字符）。**空串或解不出 = 这台安装还没有账号**，
    /// 判「有没有」要用 [`crate::account::AccountKey::from_hex`]，这里只存这一串。
    pub key: String,
    /// 要去连的那一台的地址（`192.168.1.5:8410` 这样的一句，形状见
    /// [`crate::net::lansync::normalize_peer`]）。
    ///
    /// 只有一格而不是一个列表：局域网里能当场连上的对端本来就是「另一台设备」，
    /// 三台要收敛就点两次，每次填对面那一个地址 —— 列表要增删改的界面比这一格贵，
    /// 而它买回来的东西一趟往返也能做到。
    pub peer: String,
}

impl Account {
    /// 与其它节同一条：手填或粘贴进来的密钥去掉误带的首尾空白。
    pub fn normalized(mut self) -> Self {
        self.key = self.key.trim().to_string();
        self.peer = self.peer.trim().to_string();
        self
    }

    /// 这台设备**现在**能不能走局域网同步：开着、且手里真有一把解得开的密钥。
    ///
    /// 判据用 [`crate::account::AccountKey::from_hex`] 而不是「这一格非空」：手抄进来少一位
    /// 的密钥存得住、解不出，那时屏幕上该说的是「还没有账号」，而不是让后面每一趟都撞在
    /// 一个解不开的形状上。
    pub fn lan_ready(&self) -> bool {
        self.enabled && crate::account::AccountKey::from_hex(&self.key).is_some()
    }
}

/// 本机会员权益；到期日为本地日序号，买断位独立于旧天数权益。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Membership {
    /// 还算数的最后那一天，本地日序号。`0` 就是从没开过 —— 1970-01-01 对任何今天都已是过去，
    /// 所以这一档不需要单独的哨兵值。
    ///
    /// 这是**天数档**的权益位。买断制上线后的新订单走 [`Membership::lifetime`]，这一格只
    /// 服务老订单与「 lifetime 记账前买的时间」—— 两种权益并存，谁在谁都不重复给。
    pub until_day: i64,
    /// 买断档：一次购买，权益跟设备绑定走，没有到期日。
    ///
    /// 与 [`Self::until_day`] 并列而不是互斥：老的天数订单照常记到期日，买断单置这一位。
    /// `tier_on` 只要有任意一份权益在就算 Premium。
    pub lifetime: bool,
    /// 这台设备上兑过的那几笔，见 [`Claim`]。
    ///
    /// **只用来给「这台设备」那一屏说话**：到期日不从它重算。台账是从这一版才开始记的，
    /// 之前买的那些年不在里面 —— 拿它重放会把老安装的权益吃掉。解除走的是整段清零那一条
    /// （[`Membership::clear_activation`]），所以这里不需要一份完整的账。
    pub claims: Vec<Claim>,
}

/// 本机订单留痕；订单号不进入同步或日志，台账不用于重算旧权益。
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Claim {
    pub order: String,
    pub days: i64,
    /// 到账那一天，本地日序号 —— 与 [`Membership::until_day`] 同一个口径。
    pub on_day: i64,
    /// 这一笔是买断单。`days` 在买断单上是 0，屏上的句子由这一位决定，不由天数。
    pub lifetime: bool,
}

/// 会员等级。**是派生值，不落盘**（见 [`Membership`] 的说明），所以没有 `Serialize`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MembershipTier {
    /// 普通读者。
    #[default]
    Standard,
    /// 高级会员，且在有效期内。
    Premium,
}

impl Membership {
    /// 在 `today`（本地日序号）这一天算哪一档。含当天。
    ///
    /// 买断位与到期日**并存不互斥**：老订单在 `until_day` 里过日子，买断单一票通过 ——
    /// 到期日早就不算数的读者，只要他买过一次断，就还是 Premium。
    pub fn tier_on(&self, today: i64) -> MembershipTier {
        if self.lifetime || today <= self.until_day {
            MembershipTier::Premium
        } else {
            MembershipTier::Standard
        }
    }

    /// 续上 `days` 天，返回新的到期日（本地日序号，含当天）。
    ///
    /// 两条起算路分别算，因为「含当天」在两种情形下位置不同：
    /// - 还在有效期内 —— 剩下那段一天不许少给，直接在旧到期日后面接：`until_day + days`；
    /// - 已过期或从没开过 —— 今天就是买到的第一天，`today + days - 1`（提前续费不亏剩下的天数，
    ///   过期那段也不继续吃掉刚买的时间）。
    ///
    /// `days` 为 0（同一单重交）时**原样返回**：那一档的权益在第一次就给出去了，
    /// 接上去等于白送，把 `today` 收进来则会在已过期时凭空多给一天。
    pub fn extended_on(&self, days: i64, today: i64) -> i64 {
        if days <= 0 {
            return self.until_day;
        }
        if self.until_day >= today {
            self.until_day + days
        } else {
            today + days - 1
        }
    }

    /// 记一笔到账：天数按 [`extended_on`](Self::extended_on) 接上去，同时在台账里留一行。
    ///
    /// `today` 必须是**出发那一刻**取好的那一个（与 `services/activation.rs` 那条同一口径）：
    /// 回来再取会让跨日界线那一天的「买到的第一天」差一格。
    ///
    /// `days <= 0`（同一单重交）与空订单号都不记：前者那一笔的权益在第一次就给出去了，
    /// 记了会让「这台设备名下的几笔」里出现一行 0 天的东西；后者没什么可认的。
    pub fn record(&mut self, order: impl Into<String>, days: i64, today: i64) {
        let order = order.into();
        if days <= 0 || order.is_empty() {
            return;
        }
        self.until_day = self.extended_on(days, today);
        // 同一单再记一次（解除后在这台重新兑）以最后一次为准，不叠成两行 —— 台账是给人读的，
        // 不是流水账。
        if let Some(existing) = self.claims.iter_mut().find(|claim| claim.order == order) {
            existing.days = days;
            existing.on_day = today;
        } else {
            self.claims.push(Claim {
                order,
                days,
                on_day: today,
                lifetime: false,
            });
        }
    }

    /// 记一笔**买断**到账：置永久位，台账里留一行（`days` 为 0，句子由 [`Claim::lifetime`] 说）。
    ///
    /// `today` 的口径与 [`Self::record`] 相同：出发那一刻取好的那一个。
    /// 空订单号不记 —— 没什么可认的。
    pub fn record_lifetime(&mut self, order: impl Into<String>, today: i64) {
        let order = order.into().trim().to_string();
        if order.is_empty() {
            return;
        }
        self.lifetime = true;
        if let Some(existing) = self.claims.iter_mut().find(|claim| claim.order == order) {
            existing.days = 0;
            existing.lifetime = true;
            existing.on_day = today;
        } else {
            self.claims.push(Claim {
                order,
                days: 0,
                on_day: today,
                lifetime: true,
            });
        }
    }

    /// 解除这台设备的激活：权益清零、台账清空。
    ///
    /// 整段清零而不是「扣掉那一笔的天数」，是因为扣不动也说不清：一笔 365 天买了两个月，
    /// 该退回来的到底是 365 还是 300？而服务端那一侧只有「这一单还绑不绑在这台」这一个事实。
    /// 界面在按下去之前就把这一句摆出来了（`confirm.release.*`）。买断位一并清 ——
    /// 服务端把那一单标回「可认领」，权益跟着设备走，不跟这台的过去走。
    pub fn clear_activation(&mut self) {
        self.until_day = 0;
        self.lifetime = false;
        self.claims.clear();
    }
}
