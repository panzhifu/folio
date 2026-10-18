//! 分享卡片：把选中的那一段连同书名与位置渲成一张图，递给系统的分享面板。
//!
//! 卡片是在一个不显示的窗口里**现画**的，不是截屏幕。理由不是省事：整页快照要把整个
//! 阅读视图在第二个窗口里重建一遍（会话、图集、滚动状态都挂在里面），而卡片要的只有
//! 「一段字 + 出处」——现画一份反而是小的那一半，且不受读者那块屏当前的字号、栏宽与
//! 亮度影响。离屏窗口与 PNG 编码都不属于界面层，走 [`crate::ui::platform`] 的边界
//! （[`crate::ui::platform::render_png_offscreen`]），本模块因此一个 `cfg` 都不写。
//!
//! 摆不摆这一格由编译期那一条判据决定（[`crate::ui::platform::RENDERS_SHARE_CARDS`]）：
//! 桌面上没有离屏窗口可开，按钮按下去不会有任何结果，所以菜单里根本不长那一格。
//!
//! 卡片的尺寸与设备像素比都是**定值**，不读读者屏幕的那一个：这张图是要离开应用的成品，
//! 它的像素数就该在每台设备上一样，而不是跟着屏幕密度忽大忽小。
//!
//! 文字全部按 `px` 给，不用 `text_sm()` 那一族相对字号：离屏窗口外面没有套框架的 `Root`，
//! 因此没有窗口级的 rem 基准，相对字号会读到一个与阅读界面不同的默认值。字体名从主题里
//! 取（`ShareCard::render` 开头那一句），因为 gpui 文字样式的默认家族在 Android 上可能整串
//! 回退名都解析不出来，而解析不到是 panic。

use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Window, WindowAppearance, div, px, size,
};

use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::platform;

/// 卡片的宽与高（逻辑像素）。竖版 2:3 是社交平台那张卡的比例。
const CARD_WIDTH: f32 = 720.0;
const CARD_HEIGHT: f32 = 1080.0;
/// 这张图自己的设备像素比：交出去的 PNG 因此恒为 1440×2160。
const CARD_SCALE: f32 = 2.0;
/// 引文最多留这么多字。卡片是固定尺寸排的，超长不会把卡片撑大，只会把版式挤成一团，
/// 所以在数据那一层就裁掉并补一个省略号。按 608 逻辑像素的栏宽与 34 的字号算，
/// 中日文每行约 17 字、220 字约十三行，落得进上面那一块。
const QUOTE_MAX: usize = 220;
/// 卡片内的留白。
const CARD_PAD: f32 = 56.0;

/// 卡片上要摆的那几行字。纯数据，所以「裁到哪、空的那一行摆不摆」这两条判断能在
/// 宿主的 `cargo test` 里钉住，不需要开窗口也不需要 GPU。
pub(in crate::ui::features::reader) struct CardCopy {
    quote: SharedString,
    title: SharedString,
    /// 空串 = 这一行不摆（没有位置的来源时卡片仍然该分享得出去）。
    position: SharedString,
}

/// 把选区与出处收拾成卡片那份文字。
///
/// 选区跨段时带回来的是原始文本节点之间的换行与空白，直接上屏会在卡片里开出空洞；
/// 收成单个空格是为了让 [`QUOTE_MAX`] 按「读者看得见的字数」算，不是按源码里的空白算。
pub(in crate::ui::features::reader) fn card_copy(
    quote: &str,
    title: &str,
    position: &str,
) -> CardCopy {
    let flat = quote.split_whitespace().collect::<Vec<_>>().join(" ");
    let quote = if flat.chars().count() <= QUOTE_MAX {
        flat
    } else {
        let mut kept: String = flat.chars().take(QUOTE_MAX).collect();
        kept.push('…');
        kept
    };
    CardCopy {
        quote: quote.into(),
        title: title.trim().into(),
        position: position.trim().into(),
    }
}

/// 文件名里带书名，而书名里可能有 `/`、空格甚至引号：递给 Java 那侧会被当成路径或直接
/// 拒掉。所以只留字母数字与 CJK，其余一律丢掉，并截到 40 个字符；一个字符都不剩时用
/// `folio` 兜底，不然文件名会只剩 `-quote.png`。
fn file_name(title: &str) -> String {
    let stem: String = title
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(40)
        .collect();
    let stem = if stem.is_empty() {
        "folio".into()
    } else {
        stem
    };
    format!("{stem}-quote.png")
}

/// 把卡片渲成 PNG 并递给系统的分享面板。
///
/// 整趟是同步的（离屏那一帧由窗口自己的回调当场交出来），但必须离开按钮的事件处理
/// 函数才能跑：处理函数里 `App` 正被借用，而开窗与渲帧都要自己借它。所以这里
/// `cx.spawn` 出去一跳，任务里先渲再递 —— 见 [`platform::render_png_offscreen`] 那一段。
///
/// `appearance` 取的是阅读窗口自己那一档，卡片配色因此跟着读者当前看到的浅深走。
pub(in crate::ui::features::reader) fn present(
    copy: CardCopy,
    appearance: WindowAppearance,
    cx: &mut Context<ReaderSession>,
) {
    let name = file_name(copy.title.as_ref());
    cx.spawn(async move |_, cx| {
        let bytes = platform::render_png_offscreen(
            size(px(CARD_WIDTH), px(CARD_HEIGHT)),
            CARD_SCALE,
            appearance,
            move |_, app| app.new(|_| ShareCard { copy }),
            cx,
        );
        let Some(bytes) = bytes else { return };
        cx.update(|cx| crate::ui::host::share_image(&bytes, "image/png", &name, cx));
    })
    .detach();
}

/// 卡片本身。它只是一个 `Render` 视图，被装进那个不显示的窗口里画一帧。
struct ShareCard {
    copy: CardCopy,
}

impl Render for ShareCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let quote = self.copy.quote.clone();
        let title = self.copy.title.clone();
        let position = self.copy.position.clone();
        v_flex()
            .size_full()
            .p(px(CARD_PAD))
            .justify_between()
            .font_family(theme.font_family.clone())
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                // 引文占上面那一块：字号比正文大一档，因为它现在是「给人看的那一句」，
                // 不是要读完的一页。
                div()
                    .w_full()
                    .text_size(px(34.0))
                    .line_height(px(50.0))
                    .child(quote),
            )
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(div().w_full().h(px(1.0)).bg(theme.border))
                    .child(
                        div()
                            .w_full()
                            .text_size(px(22.0))
                            .line_height(px(30.0))
                            .text_color(theme.muted_foreground)
                            .child(title),
                    )
                    .children((!position.is_empty()).then(|| {
                        div()
                            .w_full()
                            .text_size(px(18.0))
                            .line_height(px(26.0))
                            .text_color(theme.muted_foreground)
                            .child(position)
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    //! 只钉这份数据里的判断：裁到哪、空白收成什么、空着的那一行摆不摆、文件名收成什么。
    //! 卡片画出来什么样要到真机上才看得见（离屏渲染那一半只在移动目标上编得动），
    //! 而这四条都是纯函数，在宿主上就能钉死。

    use super::{QUOTE_MAX, card_copy, file_name};

    /// 裁字是按**码点**数的：一个 emoji 后面接着裁不能把字符切一半。
    #[test]
    fn a_long_quote_is_cut_on_char_boundaries_with_an_ellipsis() {
        let long = "句".repeat(QUOTE_MAX + 50);
        let text = card_copy(&long, "书", "").quote;
        assert_eq!(
            text.chars().count(),
            QUOTE_MAX + 1,
            "裁到上限并补一个省略号"
        );
        assert!(text.ends_with('…'));

        let with_emoji = format!("{}🦀✅", "字".repeat(QUOTE_MAX));
        let cut = card_copy(&with_emoji, "书", "").quote;
        assert_eq!(cut.chars().count(), QUOTE_MAX + 1);
        assert!(
            cut.ends_with('…'),
            "裁掉的是尾巴上的 emoji，不是把一个 emoji 切一半"
        );
    }

    /// 选区跨段带回来的换行与连续空白收成单个空格 —— 否则上限是按看不见的空白算的。
    #[test]
    fn whitespace_across_paragraphs_collapses_to_one_space() {
        let card = card_copy("第一段\n\n   第二段\t第三段", "书", "位置");
        assert_eq!(card.quote.as_ref(), "第一段 第二段 第三段");
    }

    /// 没有位置也要分享得出去：那一行不摆，其余照旧。
    #[test]
    fn an_empty_position_line_is_dropped_not_printed() {
        let card = card_copy("引文", "《书》", "   ");
        assert_eq!(card.title.as_ref(), "《书》");
        assert!(card.position.is_empty());
    }

    /// 书名进文件名要过一遍：`/` 会被 Java 那侧当成路径。
    #[test]
    fn a_title_with_path_separators_still_yields_a_plain_file_name() {
        assert_eq!(file_name("a/b:c*d"), "abcd-quote.png");
        assert_eq!(file_name("  "), "folio-quote.png");
        assert_eq!(file_name("未来简史"), "未来简史-quote.png");
    }
}
