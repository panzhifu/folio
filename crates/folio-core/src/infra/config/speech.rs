use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;
/// 朗读语速、音高以常速 1.0 的倍数保存，后端单位由 speech::gate 换算。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Speech {
    /// 语速，常速 1.0。
    pub rate: f32,
    /// 音高，常速 1.0。
    pub pitch: f32,
    /// 念到哪一段就翻到哪一段。关掉是「念它的，我翻我的」—— 屏上的高亮仍跟着
    /// 念的那段走，只是不动页。
    pub auto_scroll: bool,
}

impl Speech {
    /// 常速原点，也是归一里非有限值的退路。
    pub const NORMAL: f32 = 1.0;
    /// 语速的语义区间。设置页那条滑条直接引用，两端共用同一份。
    pub const RATE_RANGE: RangeInclusive<f32> = 0.5..=2.0;
    /// 音高的语义区间，与语速同一档。
    pub const PITCH_RANGE: RangeInclusive<f32> = 0.5..=2.0;

    /// 非有限值退回**常速**而不是夹到端点：手改成 `inf`/坏出来的 `NaN` 被读成
    /// 「最快」等于把坏数据当成一个选择（与 [`Appearance::normalized`] 里
    /// `turn_duration` 那条同一口径）；区间两端照常夹住。
    pub fn normalized(mut self) -> Self {
        self.rate = clamp_semantic(self.rate, &Self::RATE_RANGE);
        self.pitch = clamp_semantic(self.pitch, &Self::PITCH_RANGE);
        self
    }
}

/// 手写而不是 derive（与各节同一条规矩）：`rate`/`pitch` 的默认是**常速 1.0**，
/// 不是浮点的零值 —— derive 出来的那份会把听书念成引擎档的最慢速。
impl Default for Speech {
    fn default() -> Self {
        Self {
            rate: Self::NORMAL,
            pitch: Self::NORMAL,
            auto_scroll: true,
        }
    }
}

/// 语义值（0.5–2.0）的归一：非有限退常速，其余夹进区间。
fn clamp_semantic(value: f32, range: &RangeInclusive<f32>) -> f32 {
    if !value.is_finite() {
        Speech::NORMAL
    } else {
        value.clamp(*range.start(), *range.end())
    }
}
