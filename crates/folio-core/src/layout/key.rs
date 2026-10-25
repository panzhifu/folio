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
//!
//! ## 键不每帧重算：那两格哈希活在 [`LayoutInputs`] 里
//!
//! `author_css` 与 `prefs` 进键之前各要过一次哈希，而 `prefs_hash` 那一趟是
//! **`serde_json::to_vec` 把整份 `Reader` 序列化一遍**、`author_css_hash` 那一趟是
//! SipHash over **整本书的样式表**。界面的 `layout_key()` 是**每帧、每个进链的章各一次**，
//! 所以这两笔过去每帧要付好几遍 —— 这与 §九 那条「`shape_line` 命中也要把整段文本哈希一遍」
//! 是同一个错误，只是在上一层：书越厚、CSS 越大，白付得越多。
//!
//! 现在这两格算一次就存在 [`LayoutInputs`] 上，`compose` 只搬那两个数。纪律**没破**：
//! 样式与偏好的哈希仍然**只由本文件**的 `content_hash` / `prefs_hash` 产出（在
//! [`LayoutInputs::compute`] 一处），调用方拿到的是算好的值，自己算不出第二个来源。
//! 门是 `layout::tests::cache::the_layout_inputs_hash_once` —— 建一份输入、连问两百次键，
//! `Metric::LayoutInputHashes` 必须一次都不涨。

use std::hash::{Hash, Hasher};

use crate::infra::config::Reader;
use crate::infra::metrics::Metric;

/// 度量口径的版本。**改动 `Measure` 的实现口径时 +1**：断行规则、基线与行高的算法、
/// shaping 的规则换了，一切已缓存的几何都不再可信，必须整清 —— 而这些变化不会改章文本、
/// 不会改样式，键里其他格子都对不上，只有这一格会动。
///
/// 内嵌字体集的变化走 [`LayoutKey::fonts_rev`]（那是运行时事件），这一格只管**代码**改了。
///
/// v2（2026-10-13）：[`Measure::font_extents`] 开始收 `text`。此前中英混排的行盒按**声明
/// 字体的**拉丁度量折，汉字回退脸的高度从没进过行盒 —— 那不只是行高不对，`LineRun.ascent`
/// 是选中矩形的高、也是绘制的基线，所以旧几何里那些行盒要按新口径重排。
/// **没有这一格，块缓存会把旧行盒原样搬过来，看起来改了等于没改。**
pub const METRICS_REV: u32 = 2;

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
    /// **这一章的身份**（管线第 3 级的输入）。注意它**不是**章文本的内容哈希 ——
    /// 内容哈希活在 `ChapterArtifacts.key` 上，由排版作业自己算（作业手上才有文本）。
    ///
    /// 为什么键里不能放内容哈希：那一趟取文本 2026-10-15 起挪进了排版作业（见
    /// `reader::document::ChapterSource`），界面线程不再持有正文，于是算不出内容哈希。
    /// 而界面**必须**能算出这把键，它同时管着两件事：「键没变就不重投作业」与
    /// [`context_rev`](LayoutKey::context_rev)（块缓存的复用范围）。
    ///
    /// 身份的取法由来路决定，一处算完（`ChapterSource::identity`）：按位置取的书用
    /// **章号**（一个开着的容器里第 N 章不会变），正文已经在手上的（内联正文、测试替身）
    /// 用**内容哈希**——那种章没有章号可指。
    ///
    /// **这一格换掉的保证，写下来**：从前章文本内容一变，键就变、就重排；现在按位置取的
    /// 那一路只认章号，所以**阅读会话中途把书文件换掉**不会被发现，旧几何会静默留着。
    /// 这不是待修的缺口而是有接受的取舍：重扫出来的新索引本来就是「下次开书才生效」，
    /// 而文件真被换过的那一本，内容指纹（`sha2`）也不同、在库里是另一条记录。
    /// 要推翻这条判断，先看的是「同一会话里正文变过」能不能发生。
    pub chapter: u64,
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
    /// 竖轴基准（`LayoutOptions::page_height`），量化后。`None` = 这一档没有既定高。
    ///
    /// 必须进键：`height: NN%` 那一族折出来是多少**只**由它决定，而它在翻页模式里就是
    /// 「一页多高」—— 转一次屏、下拉一次状态栏，页高变了而栏宽没变，只靠 `width` 那一格
    /// 是问不出来的，症状是排版留着旧楔块的高度、切片按新页高切，封面那类垂直居中的页
    /// 会整幅偏出一截。与 `width` 同批量化，两边用的必须是同一个数。
    pub page_height: Option<u32>,
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

/// 键的输入里**与帧无关**的那两格，各自算好一次。
///
/// 存在的理由只有性能，但它牵住的东西不只是性能：`LayoutKey::compose` 过去收的是
/// `author_css: &str` 与 `prefs: &Reader`，于是**每帧、每个进链的章**都要把整本书的
/// 样式表 SipHash 一遍、再把整份 `Reader` 用 `serde_json` 序列化一遍再哈希
/// （`prefs_hash` 那一趟还带一次堆分配）。而这两样输入根本不随帧变 —— 样式表开书时读
/// 一次，偏好只在 `set_prefs` 变。
///
/// **改成传算好的值，纪律没有变成两个来源**：样式与偏好的哈希仍然只由本文件的
/// `content_hash` 与 `prefs_hash` 产出，而且只从 [`LayoutInputs::compute`] 一个入口出去。
/// 调用方拿到的是算好的数，既不必自己算（自己算就等于把每帧那一趟请回来），
/// 也造不出第二份。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LayoutInputs {
    /// 作者层样式表的哈希。
    pub author_css_hash: u64,
    /// 整份读者偏好的哈希 —— 覆盖范围由 `serde` 的 derive 保证，不是一张手挑清单。
    pub prefs_hash: u64,
}

impl LayoutInputs {
    /// 两份输入的原始值 → 算好的两格哈希。**这两格哈希唯一的产出点。**
    ///
    /// 合法的调用时机只有两处：建 `Document` 时、`set_prefs` 时。每次调用记一笔
    /// `Metric::LayoutInputHashes` —— 那一格就是「输入变了才算」这条门的眼：
    /// 稳态滚动里它必须一次都不涨
    /// （`layout::tests::cache::the_layout_inputs_hash_once`）。
    pub fn compute(author_css: &str, prefs: &Reader) -> Self {
        Metric::LayoutInputHashes.count();
        Self {
            author_css_hash: content_hash(author_css),
            prefs_hash: prefs_hash(prefs),
        }
    }

    /// 只换读者偏好那一格。
    ///
    /// 为什么不重建整份：样式表**没变**，而它的原文在这一刻根本不必在场 ——
    /// 少算一趟 SipHash over 整本 CSS 正是这次改动的目的之一。
    ///
    /// 也不许调用方自己写 `inputs.prefs_hash = 自己算的数`：那才是第二个来源。
    /// 走这一格，`prefs_hash` 的产出点就仍然只有本文件的 `prefs_hash` 函数。
    pub fn set_prefs(&mut self, prefs: &Reader) {
        Metric::LayoutInputHashes.count();
        self.prefs_hash = prefs_hash(prefs);
    }
}

impl LayoutKey {
    /// 唯一的构造点：两格算好的输入（[`LayoutInputs`]）+ 章身份 + 栏宽 + 栏数 +
    /// 两个运行时版本。量化在这里做完 —— 调用方不许自己拼键，也不许只拼其中几格。
    ///
    /// 为什么这里收算好的哈希而不是原文：那两趟哈希的输入不随帧变，而 `compose` 是
    /// **每帧每个进链的章各调一次**的地方（见 [`LayoutInputs`]）。
    ///
    /// 为什么章那一格收**身份**而不是内容指纹：正文在这一刻不在界面线程手上
    /// （见 [`LayoutKey::chapter`]），身份由 `reader::document::ChapterSource::identity`
    /// 一处算完。两处例外守的是同一条规矩：**每一种值只有一个算它的地方。**
    pub fn compose(
        inputs: LayoutInputs,
        chapter: u64,
        width: f32,
        columns: usize,
        page_height: Option<f32>,
        images_rev: u32,
        fonts_rev: u32,
    ) -> Self {
        Self {
            chapter,
            author_css_hash: inputs.author_css_hash,
            prefs_hash: inputs.prefs_hash,
            width: quantize_px(width),
            columns: u8::try_from(columns).unwrap_or(u8::MAX),
            page_height: page_height.map(quantize_px),
            images_rev,
            fonts_rev,
            metrics_rev: METRICS_REV,
        }
    }

    /// **缓存复用范围**的那几格：以 `NodeId` 或字体表为凭的产物，能不能跨次使用。
    ///
    /// 块级缓存的键是「块节点 + 子树样式哈希 + 宽」，而 `NodeId` 只在同一份 DOM 里
    /// 才有意义、算出的宽度只在同一张字体表里才成立。这三格（章、图片索引、字体集）
    /// 加度量版本任一变了，整张块缓存必须清空 —— 样式哈希没变**不是**留下的理由。
    pub fn context_rev(&self) -> u64 {
        let mut hasher = std::hash::DefaultHasher::new();
        self.chapter.hash(&mut hasher);
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
/// 而那一趟取文本在排版作业里做，所以这个哈希也由作业算。自己另写一个 `hash_bytes`
/// 就是第二个来源，两处一旦漂，「产物复用」和「排版失效」说的就不是同一章了。
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
