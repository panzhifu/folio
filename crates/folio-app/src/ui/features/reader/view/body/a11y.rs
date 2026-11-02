//! 读屏（TalkBack 那一类）要的那份「这一屏的字」。

use std::ops::Range;

use crate::ui::features::reader::model::chapter_layout::Document;
use gpui_kit::SharedString;

/// 读屏（TalkBack 那一类）要的那份「这一屏的字」：`lines` 里每一行的可见文本，空行不占一格。
///
/// 文本向引擎要（`Layout::line_text`，与状态栏和那一族测试断言同一个来源），界面不重算：
/// 行内图片位置上的哨兵字符它已经摘掉，念出来不会带一串乱码。列表标记（`::marker` 那个
/// 「1.」）也不在这一份里 —— 它与选区、命中、批注锚点那三条路同一个口径，都不进可见文本，
/// 把它念进去等于让同一个节点上两套坐标系并存。
///
/// 只在有读屏连着的时候调（判据是调用点那一句 `Window::is_a11y_active`）：一屏几十行、
/// 每行一次分配，压在画字那条最热的路径上就是每帧白花的钱。
pub(in crate::ui::features::reader) fn spoken_lines(
    doc: &Document,
    lines: Range<usize>,
) -> Vec<SharedString> {
    lines
        .filter_map(|ix| {
            // 一叠图只念「现在这一张」那一条注：一叠五张全念，就是把五句话连着念一遍。
            if doc
                .layout()
                .lines
                .get(ix)
                .and_then(|line| line.stack)
                .is_some_and(|mark| doc.stack_current(mark.id) != mark.index)
            {
                return None;
            }
            let text = doc.layout().line_text(ix);
            (!text.is_empty()).then(|| SharedString::from(text))
        })
        .collect()
}
