use crate::infra::i18n::Locale;
use serde::{Deserialize, Serialize};
/// 备份那一页的两格选择。
///
/// 只有「要不要把书文件与词典一起装进去」这一格需要记住：它决定备份包的大小，
/// 而读者在慢网络 / 空间紧的那台机器上会一直选小档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Backup {
    /// 导出时是否把书库文件、封面与已导入词典一起打包。
    pub include_files: bool,
}

impl Default for Backup {
    fn default() -> Self {
        // 默认完整备份：备份的第一目的是「换一台机器还能读」，只有库没有书做不到。
        Self {
            include_files: true,
        }
    }
}

/// TXT 分章规则的用户差量；与 folio-format 出厂规则合成。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Novel {
    /// 差量摊平在这一节下（`novel.off` / `novel.extra` / `novel.gates`），
    /// 不再多一层 `rules` 的名字：这一节除了那张表没有别的东西。
    #[serde(flatten)]
    pub edits: folio_model::novel::Edits,
}

impl Novel {
    /// 名字与模式去掉误带的空白。空名字的那条自定义条目丢掉 —— 关掉条目是按名字作用的，
    /// 一条没有名字的条目既关不掉也说不清，留着就是给读者埋坑。
    pub fn normalized(mut self) -> Self {
        self.edits.off.retain(|name| !name.trim().is_empty());
        self.edits.off = self
            .edits
            .off
            .iter()
            .map(|name| name.trim().to_string())
            .collect();
        let mut kept: Vec<_> = Vec::new();
        for marker in self.edits.extra {
            let name = marker.name.trim().to_string();
            if name.is_empty() {
                tracing::warn!("小说规则里有一条自定义条目没有名字，已丢掉");
                continue;
            }
            let pattern = marker
                .pattern
                .map(|pattern| pattern.trim().to_string())
                .filter(|pattern| !pattern.is_empty());
            let text = marker
                .text
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty());
            kept.push(folio_model::novel::Marker {
                name,
                pattern,
                text,
                ..marker
            });
        }
        self.edits.extra = kept;
        self
    }
}

/// 书库：导入的书**复制**到哪儿。
///
/// 只有一个目录，而且必须由读者确认过才算数 —— 空串就是「还没确认」，导入要拦住。
/// 理由是导入不再是引用原文件而是往里拷一份：不先说清楚拷到哪儿，读者只会发现
/// 硬盘上多了一批自己没打算要的文件。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    /// 确认过的书库目录。空串 = 没确认过。
    pub dir: String,
    /// 文件夹监听：`Some` = 开着。盯一个「收件箱」目录，新出现的书自动入库
    /// （`folio_core::ingest::watch`）。监听书库目录自己没有意义 —— 导进来的就是从
    /// 那里拷出去的，会自我循环，normalize 把它当没开。
    pub watch: Option<WatchDir>,
}

/// 监听的目标与节奏。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchDir {
    /// 盯住的目录。
    pub dir: String,
    /// 兜底轮询间隔（秒）。事件驱动（inotify）的平台上不起作用，只在
    /// 没有系统级监听 API 的平台上是真实的节奏。
    pub interval_secs: u32,
}

impl Default for WatchDir {
    fn default() -> Self {
        Self {
            dir: String::new(),
            interval_secs: 30,
        }
    }
}

impl Library {
    /// 位置确认过了没有。认过之后导入才放行。
    pub fn is_confirmed(&self) -> bool {
        !self.dir.trim().is_empty()
    }
}

/// 界面语言。换设备同步配置时它应当跟着走，而它不属于任何一本书，
/// 也不影响书的正文语言，所以单独一节。
///
/// 默认是简体中文（[`Locale`] 的 `Default`）：这个项目的读者以中文书为第一目标；
/// 之后读回来的是用户选过的那个。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Language {
    pub locale: Locale,
}
