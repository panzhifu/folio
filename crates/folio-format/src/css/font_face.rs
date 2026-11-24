//! `@font-face` 的声明类型：只存声明，不存字节。
//!
//! 从 `css::mod` 切出来只因为 mod.rs 贴着行数上限；对外路径经 `pub use` 保持不变。

/// `@font-face` 的一条声明。**只存声明，不存字节** —— 字节由 `format` 那层按
/// `src` 里的容器路径去读，CSS 这层不该知道 ZIP 的存在。
#[derive(Debug, Clone, PartialEq)]
pub struct FontFaceRule {
    /// `font-family` 描述符，已去引号。一条 `@font-face` 写了逗号列表时**每个名字一条**。
    pub family: String,
    /// `src`，按书写顺序，第一个是首选。
    pub src: Vec<FontSrc>,
    /// `font-weight`。写了区间（`400 700`）取第一个值。缺省 400。
    pub weight: u16,
    /// `font-style`：`italic` / `oblique` 为 `Some(true)`，`normal` 为 `Some(false)`，
    /// **没写是 `None`**，不等于 `normal` —— 没写时书的意图是「这套字体的常规面」，
    /// 而注册进来的内部名自己带面信息。
    pub italic: Option<bool>,
}

/// `src` 描述符里的一项。`format()` / `tech()` 这些修饰下一个来源的函数认得但不影响
/// 来源本身，解析时跳过。
#[derive(Debug, Clone, PartialEq)]
pub enum FontSrc {
    /// 容器内的规范路径。EPUB 的 CSS 在拼接前把 `url(../Fonts/a.otf)` 改写成了绝对路径
    /// （见 `format::epub`），所以这里的串可以直接当查表键。
    Url(String),
    /// 设备上已装的字体名：只查注册名单，不注册字节。
    Local(String),
    /// 设备上的字体文件（`res://…` / `file://…` / `/system/fonts/…`）。
    ///
    /// 字节不随书来，也读不到：只拿文件名去设备已注册的家族里找同一张脸
    /// （见 `BookFonts::for_document`）。http(s) 之类的来源仍然整条丢掉。
    System(String),
}

