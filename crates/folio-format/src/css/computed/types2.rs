//! 值类型：分片/连字符意图、颜色、SVG 呈现属性的小枚举。
//!
//! 从 `css::computed` 切出来只因为 computed.rs 贴着行数上限；对外路径经 `pub use` 保持不变。

/// 连字符相关的折行意图，来自 `hyphens`（含 EPUB 的 `-epub-` / `-webkit-` 前缀写法）。
///
/// 一期没有连字词典，所以 `Auto` 与 `Manual` 的**行为**相同：都只在已有的连字符处断，
/// 不会把 `sais` 拆成 `sa-is`。差别留在「作者的意图有没有被记下来」——词典接上时不用
/// 回头改书的样式表。这条边界记在 CSS 的忽略清单里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Hyphens {
    /// CSS 初值：已有连字符可断，不主动造连字符。
    #[default]
    Manual,
    /// 连字符**不是**断点：`je-ne-sais-quoi` 整个留着。
    None,
    /// 请求词典连字；一期按 `Manual` 执行。
    Auto,
}

/// 分片（分页 / 分栏）时的断点意图。
///
/// `Left` / `Right` 会补出空片占位，「双页对开」的左右页才不会错位；
/// `Always` 是 CSS2.1 的旧写法，与 `Page` 同义 —— 老 EPUB 里满地都是，
/// 认不出它就等于把这些书的作者意图丢掉。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BreakWhen {
    /// 没有声明：由剩余可用高度决定。
    #[default]
    Auto,
    Page,
    Left,
    Right,
    Always,
    /// `page-break-*: avoid` / `break-*: avoid`：尽量别在这断。不是强制断，
    /// 所以 `is_forced()` 为假；分片器在「块能整块放下」时尊重它（见 fragment.rs）。
    Avoid,
}

impl BreakWhen {
    /// 是不是「强制断在这里」的意图。`Auto` 与 `Avoid` 都不是。
    pub fn is_forced(self) -> bool {
        matches!(self, Self::Page | Self::Left | Self::Right | Self::Always)
    }

    /// 是不是「尽量别在这断」的意图。分片器据此在放得下时避免切开。
    pub fn is_avoid(self) -> bool {
        matches!(self, Self::Avoid)
    }

    /// 强制断之后应当落在哪一侧。`None` = 不关心奇偶。
    pub fn parity(self) -> Option<bool> {
        match self {
            Self::Left => Some(false),
            Self::Right => Some(true),
            _ => None,
        }
    }
}

/// 一个 CSS 颜色：RGBA 四通道。
///
/// 第四格是 alpha（0 = 全透明，255 = 不透明）。出版书里 `rgba(80, 70, 86, .7)`
/// 这类半透明底色很常见（多看系的「人物志」框就是），丢掉 alpha 会把底图完全盖死。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color(pub u8, pub u8, pub u8, pub u8);

/// SVG 呈现属性 `fill` / `stroke` 的取值：`none` 或一格颜色。
///
/// `fill` 的 CSS 初值是黑色、`stroke` 是 `none`；这两个初值在
/// `ComputedStyle::default` 里分别写死，不靠 [`Default`] 的派生值 ——
/// 派生的 `None` 只是占位，用来给「还没算到这个属性」的临时样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SvgPaint {
    #[default]
    None,
    Color(Color),
}

/// SVG `stroke-linecap`：线端的形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SvgLineCap {
    #[default]
    Butt,
    Round,
    Square,
}

/// SVG `stroke-linejoin`：折角的形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SvgLineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}
