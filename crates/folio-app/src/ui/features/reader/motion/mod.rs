//! 一次动作「怎么走」：翻页动画的摆位层。
//!
//! 决策层（`model::turn`）回答「这一步落在哪」，这一层回答「那两张纸各自怎么走、谁压着谁」。
//! 全是纯函数，能在宿主的 `cargo test` 里表驱动钉住：gpui 的 `TestWindow` 看不到绘制场景，
//! 位移只在这一层摸得到。

pub(in crate::ui::features::reader) mod turn_animation;
