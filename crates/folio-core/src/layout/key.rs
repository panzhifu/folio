//! 排版失效键：**一个键、一处构造、各级缓存共用**（`plans/阅读管线.md` §四）。
//!
//! 存在的理由不是性能，是**防漂**：排版结果、块级缓存、切片缓存、批注解算缓存分别记住
//! 「自己是按什么输入算出来的」，而输入散落在章文本、作者样式、读者偏好、视口、字体表
//! 五个地方。两处各答一遍「现在变了没有」，两处必然漂 —— 漂出来的现象是「改了字号但
//! 某一屏还是旧行盒」，这种 bug 在屏幕上看不出来，只会在进度与锚点上飘。
//!
//! 所以：构造只有 [`LayoutKey::compose`] 这一处，比较只有 `PartialEq` 这一处，
//! 各级缓存都拿它当键（[`BlockCache`](super::BlockCache) 用
//! [`context_rev`](LayoutKey::context_rev)，界面层用整把键）。
//!
//! ## 键里为什么是 `prefs_hash` 而不是「字号 + 行距 + …」几格
//!
//! 逐格列 = 一张需要人肉维护的清单：新增一个会影响版式的偏好时，编译器不会提醒你，
//! 而漏掉的后果是**该重排的不重排**。接线前这一条已经真的漏了一次：手挑字段的
//! `prefs_hash` 不含 `mono_font_family`，而等宽泛族名是折行的输入（`reader/measure.rs:522`），
//! 于是「换等宽字体」不会让任何缓存失效。整份哈希之后这类漏洞结构性地消失了。
//!
//! 反过来，只影响目录的偏好（`toc_density` 那三格）会被算进来，代价是多一次空转的重排 ——
//! 而那次重排里 [`BlockCache`](super::BlockCache) 的块级命中**照样命中**（块键含的是计算后
//! 样式的哈希，目录档位进不了正文的计算样式），所以这个取舍是「宁多一次空转，不少一次重排」。

use std::hash::{Hash, Hasher};

use crate::config::Reader;

/// 度量口径的版本。**改动 `Measure` 的实现口径时 +1**：断行规则、基线与行高的算法、
/// shaping 的规则换了，一切已缓存的几何都不再可信，必须整清 —— 而这些变化不会改章文本、
/// 不会改样式，键里其他格子都对不上，只有这一格会动。
///
/// 内嵌字体集的变化走 [`LayoutKey::fonts_rev`]（那是运行时事件），这一格只管**代码**改了。
pub const METRICS_REV: u32 = 1;

/// 长度量化：1 px 一档。选 1 px 是为了「跟手」（拖窗口时边拖边排），代价是多几次重排；
/// 2–4 px 更省但会晚一拍（见 `plans/阅读管线.md` 待决第 2 条）。
///
/// **切片边界必须用同一个函数**：排版按量化后的宽折行，切片若按未量化的高切，
/// 同一行在不同滚动位置会被切在两个地方，表现为行盒抖半像素。
pub fn quantize_px(value: f32) -> u32 {
    // 负数是调用方算错了（窗口还没量出来），按 0 处理而不是 panic：
    // 排版会把它夹到 1 px 最小版心，键只需保证「同一份输入同一格值」。
    value.max(0.0).round() as u32
}

/// 一次排版的全部输入。**只有值语义**（`Copy + Eq + Hash`）：它可以被抄进作业、
/// 与作业带回来的那份比对，也可以在几级缓存之间来回比较，谁拿到它都不能改动别人的。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LayoutKey {
    /// 章文本的哈希（管线第 3 级的输入）。同一格里还兜着**写在章里的 `<style>`**：
    /// 它就长在章文本里，所以作者把样式表挂在文件还是挂在章里，键都一致。
    pub chapter_hash: u64,
    /// 书自己的样式表的哈希（第 4 级的输入之一，与读者偏好无关）。
    pub author_css_hash: u64,
    /// 读者偏好的哈希 —— **整份** `Reader`，不是其中几格（见模块头的取舍）。
    pub prefs_hash: u64,
    /// 栏宽（量化后）。折行发生在栏宽上，所以这一格进键；整窗宽不进，
    /// 窗口变宽而栏宽被 `max_measure` 卡住时，排版本来就不该重做。
    pub width: u32,
    /// 一页几栏。与 `width` 两格都在，是因为「两栏各 300 px」与「单栏 300 px」
    /// 排出来的行盒相同、切片却不同 —— 切片那一级要区分它们。
    pub columns: u8,
    /// 本章图片索引的版本。`<img>` 的固有尺寸是排版输入，换索引就可能换几何；
    /// 而它既不在章文本里也不在样式里，只能单独一格。
    pub images_rev: u32,
    /// 内嵌字体集的版本：注册进设备字体表的那一批面目的编号。
    ///
    /// 这一格是**块级缓存的口径修正**：块键里含的是「子树计算样式哈希 + 宽」，
    /// 字体表换了而样式没换时旧块会被复用（算出的宽度是旧字体的）。旧写法只能把这条
    /// 记成「已知缺口」，有了这一格就能在 [`context_rev`](LayoutKey::context_rev) 里清掉。
    pub fonts_rev: u32,
    /// 度量口径版本，见 [`METRICS_REV`]。
    pub metrics_rev: u32,
}

impl LayoutKey {
    /// 唯一的构造点。输入是**原始值**（章文本、CSS 文本、偏好、栏宽、栏数、两个运行时版本），
    /// 哈希与量化都在这里做完 —— 调用方不许自己拼键，也不许只拼其中几格。
    ///
    /// 传文本而不是哈希：谁传哈希进来谁就得自己保证哈希覆盖同样的内容，那又是第二个来源。
    #[allow(clippy::too_many_arguments)]
    pub fn compose(
        chapter_source: &str,
        author_css: &str,
        prefs: &Reader,
        width: f32,
        columns: usize,
        images_rev: u32,
        fonts_rev: u32,
    ) -> Self {
        Self {
            chapter_hash: content_hash(chapter_source),
            author_css_hash: content_hash(author_css),
            prefs_hash: prefs_hash(prefs),
            width: quantize_px(width),
            columns: u8::try_from(columns).unwrap_or(u8::MAX),
            images_rev,
            fonts_rev,
            metrics_rev: METRICS_REV,
        }
    }

    /// **缓存复用范围**的那几格：以 `NodeId` 或字体表为凭的产物，能不能跨次使用。
    ///
    /// 块级缓存的键是「块节点 + 子树样式哈希 + 宽」，而 `NodeId` 只在同一份 DOM 里
    /// 才有意义、算出的宽度只在同一张字体表里才成立。这三格（章文本、图片索引、字体集）
    /// 加度量版本任一变了，整张块缓存必须清空 —— 样式哈希没变**不是**留下的理由。
    pub fn context_rev(&self) -> u64 {
        let mut hasher = std::hash::DefaultHasher::new();
        self.chapter_hash.hash(&mut hasher);
        self.images_rev.hash(&mut hasher);
        self.fonts_rev.hash(&mut hasher);
        self.metrics_rev.hash(&mut hasher);
        hasher.finish()
    }
}

/// 一段文本内容的哈希。用 [`std::hash::DefaultHasher`]（SipHash）而不是固定算法：
/// 键活在**单个进程**里，不落库也不跨设备，所以不需要可复现的哈希值，只需要
/// 「同一进程内两次一致」+「不同内容几乎不同」。落库的键（`Locator`、`sha2` 内容指纹）
/// 是另一回事，那些必须跨进程稳定。
///
/// 公开这一格是为了**章的解析产物**：缓存产物时要问「这份 DOM 是从哪段文本来的」，
/// 而那个哈希必须与 [`LayoutKey::chapter_hash`] 同源。自己另写一个 `hash_bytes` 就是
/// 第二个来源，两处一旦漂，「产物复用」和「排版失效」说的就不是同一章了。
pub fn content_hash(text: &str) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    // 长度先进去：否则「一段文本」与「同一文本加了个尾部空白」在某些切法下难分辨。
    text.len().hash(&mut hasher);
    text.hash(&mut hasher);
    hasher.finish()
}

/// 整份读者偏好的哈希 —— 覆盖范围由 `serde` 的 derive 保证，不是一张手挑清单。
///
/// 做法是把 [`Reader`] 序列化成 JSON 再哈希那些字节：derive 出来的 `Serialize` 一定
/// 枚举**全部**字段，所以给 `Reader` 加一个会影响版式的偏好，它自动就进了键，不需要
/// 有人记得来这边补一行。这正是接线前那条手挑 `prefs_hash` 缺的东西（它漏了
/// `mono_font_family`，见 `crates/folio-core/src/layout/tests.rs` 的
/// `every_reader_preference_changes_the_key`）。
///
/// 为什么不用 `derive(Hash)`：`f32` 不实现 `Hash`，要 derive 就得先把浮点改成按位哈希，
/// 那又是一张人肉清单。为什么不在键里存整份 `Reader` 做 `==` 比较：`LayoutKey` 要
/// `Copy`，要能抄进后台作业再比回来，一个 `u64` 比一份克隆便宜得多。
///
/// 代价是一次序列化（`Reader` 十几个字段，一次排版请求算一次，相对整章排版可忽略）。
/// 坏值（NaN 这类序列化不了的浮点）走 `Err` 那一支：哈希一个固定哨兵，同一份坏配置
/// 仍然两次一致，只是必然与好配置不同键。
fn prefs_hash(prefs: &Reader) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    match serde_json::to_vec(prefs) {
        Ok(bytes) => bytes.hash(&mut hasher),
        Err(_) => b"unreadable-prefs".hash(&mut hasher),
    }
    hasher.finish()
}
